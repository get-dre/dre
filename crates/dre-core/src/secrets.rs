//! Secret masking. The value of every environment variable named `DRE_SECRET_*`, and of every
//! environment variable `profiles.yml` reads through `env_var()`, is replaced by
//! `*****` in everything DRE writes for people to read: the console, `logs/dre.log`, JSON events,
//! `run_results.json` and the compiled SQL in `target/compiled/`. The SQL actually sent to the
//! database is unchanged. `mask_secrets: false` in `dre_project.yml` turns masking off.
//!
//! JSON is redacted structurally, before it is serialized: serializing escapes `"`, `\` and
//! control characters, so masking the finished text would miss a secret that contains them. Use
//! [`to_json_pretty`] / [`to_json_line`] for every JSON surface, never `mask` on JSON text.
//!
//! Values shorter than 4 characters aren't masked: replacing every `a` or `12` in a log would
//! make it unreadable, and a value that short isn't a secret anyway.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

pub const PREFIX: &str = "DRE_SECRET_";
pub const MASK: &str = "*****";
pub const MASKED_SOURCE_ERROR: &str =
    "source error details omitted because the request contains a masked secret";
const MIN_LEN: usize = 4;

static ENABLED: AtomicBool = AtomicBool::new(true);

fn eligible(value: &str) -> bool {
    value.chars().count() >= MIN_LEN
}

/// Secret values, longest first so a secret containing another is masked whole. Starts with
/// `DRE_SECRET_*`; [`register_env_references`] adds the variables `profiles.yml` reads.
static SECRETS: LazyLock<RwLock<Arc<Secrets>>> = LazyLock::new(|| {
    RwLock::new(Arc::new(Secrets::new(sorted(
        std::env::vars()
            .filter(|(k, _)| k.starts_with(PREFIX))
            .map(|(_, v)| v),
    ))))
});

/// The secret values, and the forms they take in Markdown and in collapsed titles.
struct Secrets {
    plain: Vec<String>,
    markdown: Vec<String>,
    title: Vec<String>,
}

impl Secrets {
    fn new(plain: Vec<String>) -> Secrets {
        let markdown = sorted(
            plain
                .iter()
                .flat_map(|s| [s.clone(), dre_protocol::markdown::escape(s)]),
        );
        let title = sorted(
            plain
                .iter()
                .flat_map(|s| [s.clone(), s.split_whitespace().collect::<Vec<_>>().join(" ")]),
        );
        Secrets {
            plain,
            markdown,
            title,
        }
    }
}

fn sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = values.into_iter().filter(|v| eligible(v)).collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    v
}

fn secrets() -> Arc<Secrets> {
    SECRETS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The environment variables a template reads through `env_var('NAME')` (either quote).
pub fn env_references(text: &str) -> Vec<String> {
    static CALL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"env_var\(\s*(?:'([^']+)'|"([^"]+)")"#).unwrap());
    CALL.captures_iter(text)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_owned())
        .collect()
}

/// Mask the values of the environment variables `text` (a `profiles.yml`) reads through
/// `env_var()`: connection passwords and tokens are usually passed that way.
pub fn register_env_references(text: &str) {
    let values: Vec<String> = env_references(text)
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    if values.is_empty() {
        return;
    }
    let mut guard = SECRETS.write().unwrap_or_else(|e| e.into_inner());
    let merged = sorted(guard.plain.iter().cloned().chain(values));
    *guard = Arc::new(Secrets::new(merged));
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn mask_with<'a>(s: &'a str, secrets: &[String]) -> Cow<'a, str> {
    if !secrets.iter().any(|x| s.contains(x.as_str())) {
        return Cow::Borrowed(s);
    }
    let mut out = s.to_string();
    for x in secrets {
        out = out.replace(x.as_str(), MASK);
    }
    Cow::Owned(out)
}

/// `s` with every secret value replaced by `*****`.
pub fn mask(s: &str) -> Cow<'_, str> {
    if !ENABLED.load(Ordering::Relaxed) {
        return Cow::Borrowed(s);
    }
    mask_with(s, &secrets().plain)
}

/// Mask a rendered message without changing the report payload. Markdown interpolation has
/// already escaped punctuation, so both literal and escaped secret values must be matched.
pub(crate) fn mask_markdown(s: &str) -> Cow<'_, str> {
    if !ENABLED.load(Ordering::Relaxed) {
        return Cow::Borrowed(s);
    }
    mask_with(s, &secrets().markdown)
}

/// Message titles collapse whitespace before delivery. Match that representation as well as
/// the literal secret when protecting a title in logs and run metadata.
pub(crate) fn mask_message_title(s: &str) -> Cow<'_, str> {
    if !ENABLED.load(Ordering::Relaxed) {
        return Cow::Borrowed(s);
    }
    mask_with(s, &secrets().title)
}

/// Whether masking is enabled and `s` contains a complete registered secret.
pub fn contains_secret(s: &str) -> bool {
    ENABLED.load(Ordering::Relaxed) && secrets().plain.iter().any(|secret| s.contains(secret.as_str()))
}

fn mask_source_error_with<'a>(error: &'a str, sensitive: bool, secrets: &[String]) -> Cow<'a, str> {
    if sensitive {
        Cow::Borrowed(MASKED_SOURCE_ERROR)
    } else {
        mask_with(error, secrets)
    }
}

/// Mask a source error before it reaches logs or output. Source plugins can transform or truncate
/// a request in their diagnostics, so exact replacement is not sufficient for a secret-bearing
/// request. In that case the opaque detail is omitted instead.
pub fn mask_source_error(error: &str, sensitive: bool) -> Cow<'_, str> {
    if !ENABLED.load(Ordering::Relaxed) {
        return Cow::Borrowed(error);
    }
    mask_source_error_with(error, sensitive, &secrets().plain)
}

fn unique_masked_names_with(names: &[String], secrets: &[String]) -> Vec<String> {
    use std::collections::BTreeSet;

    let masked: Vec<String> = names
        .iter()
        .map(|name| mask_with(name, secrets).into_owned())
        .collect();
    if names == masked {
        return names.to_vec();
    }
    let mut used = BTreeSet::new();
    let mut output = vec![String::new(); names.len()];
    let mut pending = Vec::new();

    // Literal names keep their public identity and reserve every suffix they already occupy.
    for (index, (original, protected)) in names.iter().zip(&masked).enumerate() {
        if original == protected {
            used.insert(original.clone());
            output[index].clone_from(original);
        } else {
            pending.push((protected.clone(), original.clone(), index));
        }
    }

    // Original names make the assignment stable when source ordering changes.
    pending.sort();
    for (base, _, index) in pending {
        let mut candidate = base.clone();
        let mut suffix = 2;
        while !used.insert(candidate.clone()) {
            candidate = format!("{base}#{suffix}");
            suffix += 1;
        }
        output[index] = candidate;
    }
    output
}

fn redact_json_with(v: &mut serde_json::Value, secrets: &[String]) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if let Cow::Owned(m) = mask_with(s, secrets) {
                *s = m;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| redact_json_with(v, secrets)),
        Value::Object(o) => {
            let entries = std::mem::take(o);
            let keys: Vec<String> = entries.keys().cloned().collect();
            let protected_keys = unique_masked_names_with(&keys, secrets);
            for ((_, mut val), protected_key) in entries.into_iter().zip(protected_keys) {
                redact_json_with(&mut val, secrets);
                o.insert(protected_key, val);
            }
        }
        _ => {}
    }
}

/// `v` with every secret value replaced by `*****` in all strings and object keys. Colliding
/// masked keys receive `#2`, `#3`, and so on rather than silently replacing another entry.
pub fn redact_json(v: &mut serde_json::Value) {
    if ENABLED.load(Ordering::Relaxed) {
        redact_json_with(v, &secrets().plain);
    }
}

fn redacted<T: serde::Serialize>(v: &T) -> serde_json::Result<serde_json::Value> {
    let mut v = serde_json::to_value(v)?;
    redact_json(&mut v);
    Ok(v)
}

/// Pretty JSON of `v` with secrets redacted first.
pub fn to_json_pretty<T: serde::Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&redacted(v)?)
}

/// One-line JSON of `v` with secrets redacted first.
pub fn to_json_line<T: serde::Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string(&redacted(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secrets(values: &[&str]) -> Vec<String> {
        let mut values: Vec<String> = values.iter().map(|v| (*v).to_string()).collect();
        values.sort_by_key(|s| std::cmp::Reverse(s.len()));
        values
    }

    #[test]
    fn redacts_nested_values_longest_first() {
        let secrets = secrets(&["nested-secret", "nested-secret-and-more"]);
        let mut value = json!({"items": ["nested-secret-and-more", {"value": "x nested-secret y"}]});
        redact_json_with(&mut value, &secrets);
        assert_eq!(value, json!({"items": ["*****", {"value": "x ***** y"}]}));
    }

    #[test]
    fn preserves_entries_when_masked_keys_collide() {
        let secrets = secrets(&["alpha-secret-key", "bravo-secret-key"]);
        let mut value = json!({
            "alpha-secret-key": 1,
            "bravo-secret-key": 2,
            "*****": 3,
            "*****#2": 4,
        });
        redact_json_with(&mut value, &secrets);
        assert_eq!(
            value,
            json!({
                "*****": 3,
                "*****#2": 4,
                "*****#3": 1,
                "*****#4": 2,
            })
        );
    }

    #[test]
    fn masking_one_name_does_not_rename_duplicate_literal_names() {
        let secrets = secrets(&["secret-column"]);
        let names = ["a", "a", "secret-column"].map(str::to_string);
        assert_eq!(unique_masked_names_with(&names, &secrets), ["a", "a", "*****"]);
    }

    #[test]
    fn suppresses_transformed_source_errors_only_for_secret_requests() {
        let secrets = secrets(&["long-secret-value-that-a-source-truncated"]);
        let error = "LINE 1: select * from long-secret-value-that-a-source...";
        assert_eq!(mask_source_error_with(error, true, &secrets), MASKED_SOURCE_ERROR);
        assert_eq!(
            mask_source_error_with("ordinary syntax error", false, &secrets),
            "ordinary syntax error"
        );
    }

    #[test]
    fn finds_env_var_references_with_either_quote() {
        assert_eq!(
            env_references(
                "password: \"{{ env_var('PG_PASSWORD') }}\"\ntoken: '{{ env_var(\"WAREHOUSE_TOKEN\", \"x\") }}'"
            ),
            ["PG_PASSWORD", "WAREHOUSE_TOKEN"]
        );
    }

    #[test]
    fn minimum_length_counts_unicode_characters() {
        assert!(!eligible("ééé"));
        assert!(eligible("éééé"));
    }
}
