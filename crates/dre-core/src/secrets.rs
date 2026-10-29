//! Secret masking. The value of every environment variable named `DRE_SECRET_*`, and of every
//! environment variable `profiles.yml` reads through `env_var()` into a secret field (one the
//! plugin declares secret, or named like `password`, `token`, `api_key`), is replaced by
//! `*****` in everything DRE writes for people to read: the console, `logs/dre.log`, JSON events,
//! `run_results.json` and the compiled SQL in `target/compiled/`. The SQL actually sent to the
//! database is unchanged. `mask_secrets: false` in `dre_project.yml` turns masking off.
//!
//! JSON is redacted structurally, before it is serialized: serializing escapes `"`, `\` and
//! control characters, so masking the finished text would miss a secret that contains them. Use
//! [`to_json_pretty`] / [`to_json_line`] for every JSON surface, never `mask` on JSON text.
//!
//! Every value is masked, however short. One shorter than 4 characters is masked where it stands
//! alone (not inside a longer word or number), so a PIN of `12` masks `PIN 12` but leaves `2012`
//! readable.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use crate::context::Env;

pub const PREFIX: &str = "DRE_SECRET_";
pub const MASK: &str = "*****";
pub const MASKED_SOURCE_ERROR: &str =
    "source error details omitted because the request contains a masked secret";
const MIN_LEN: usize = 4;

fn eligible(value: &str) -> bool {
    !value.is_empty()
}

/// Short values mask only where they stand alone.
fn short(value: &str) -> bool {
    value.chars().count() < MIN_LEN
}

/// `s` with each standalone `x` (not touching a letter or digit) replaced by the mask.
fn replace_standalone(s: &str, x: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut before: Option<char> = None;
    while let Some(i) = rest.find(x) {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let prev = head.chars().next_back().or(before);
        let next = tail[x.len()..].chars().next();
        if word(prev) || word(next) {
            out.push_str(x);
        } else {
            out.push_str(MASK);
        }
        before = x.chars().next_back();
        rest = &tail[x.len()..];
    }
    out.push_str(rest);
    out
}

/// The secret values, and the forms they take in Markdown and in collapsed titles.
struct Values {
    plain: Vec<String>,
    markdown: Vec<String>,
    title: Vec<String>,
}

impl Values {
    fn new(plain: Vec<String>) -> Values {
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
        Values {
            plain,
            markdown,
            title,
        }
    }
}

fn sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = values.into_iter().filter(|v| eligible(v)).collect();
    v.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    v.dedup();
    v
}

/// The environment variables a template reads through `env_var('NAME')` (either quote).
pub fn env_references(text: &str) -> Vec<String> {
    static CALL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"env_var\(\s*(?:'([^']+)'|"([^"]+)")"#).expect("the pattern is a valid regex")
    });
    CALL.captures_iter(text)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_owned())
        .collect()
}

/// Whether a profile field's name says it holds a secret. Paths, hosts and fingerprints don't
/// count (`private_key_path`, `host_key_fingerprint`): masking them would hide what a message is
/// about, and they aren't secrets.
pub fn secret_field_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["password", "passphrase", "secret", "token", "credential"]
        .iter()
        .any(|w| name.contains(w))
        || name == "key"
        || name.ends_with("_key")
        || name == "apikey"
}

fn collect_strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(a) => {
            for v in a {
                collect_strings(v, out);
            }
        }
        serde_json::Value::Object(o) => {
            for v in o.values() {
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}

fn mask_with<'a>(s: &'a str, secrets: &[String]) -> Cow<'a, str> {
    if !secrets.iter().any(|x| s.contains(x.as_str())) {
        return Cow::Borrowed(s);
    }
    let mut out = s.to_string();
    for x in secrets {
        out = if short(x) {
            replace_standalone(&out, x)
        } else {
            out.replace(x.as_str(), MASK)
        };
    }
    if out == s {
        return Cow::Borrowed(s);
    }
    Cow::Owned(out)
}

fn mask_source_error_with<'a>(error: &'a str, sensitive: bool, secrets: &[String]) -> Cow<'a, str> {
    if sensitive {
        Cow::Borrowed(MASKED_SOURCE_ERROR)
    } else {
        mask_with(error, secrets)
    }
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
        Value::Array(a) => {
            for v in a {
                redact_json_with(v, secrets);
            }
        }
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

/// The secret values one command masks: `DRE_SECRET_*` from its environment, then whatever the
/// command registers as it goes (secret profile fields, `secret_columns`). Cloning shares the
/// values, so every thread of a run masks the same set; two commands in one process each have
/// their own.
#[derive(Clone)]
pub struct Secrets {
    values: Arc<RwLock<Arc<Values>>>,
    enabled: Arc<AtomicBool>,
    env: Env,
}

impl Secrets {
    /// The `DRE_SECRET_*` values of `env`, masked; secret fields read their variables from it.
    pub fn new(env: &Env) -> Secrets {
        let plain = sorted(
            env.vars()
                .filter(|(k, _)| k.starts_with(PREFIX))
                .map(|(_, v)| v.to_string()),
        );
        Secrets {
            values: Arc::new(RwLock::new(Arc::new(Values::new(plain)))),
            enabled: Arc::new(AtomicBool::new(true)),
            env: env.clone(),
        }
    }

    fn current(&self) -> Arc<Values> {
        self.values.read().unwrap().clone()
    }

    /// `mask_secrets:` in `dre_project.yml`: off masks nothing.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Mask the values of the environment variables a secret profile field reads through
    /// `env_var()`, at any depth of `value`.
    pub fn register_secret_field(&self, value: &serde_json::Value) {
        let mut text = Vec::new();
        collect_strings(value, &mut text);
        let names: Vec<String> = text.iter().flat_map(|s| env_references(s)).collect();
        self.register_values(
            names
                .iter()
                .filter_map(|name| self.env.var(name).map(str::to_string)),
        );
    }

    /// Mask the `env_var()` values that the secret fields of `fields` (a profile target) read,
    /// nested blocks (`ssh:`) included. `secret` adds names a plugin declares secret.
    pub fn register_secret_fields(
        &self,
        fields: &serde_json::Map<String, serde_json::Value>,
        secret: &[String],
    ) {
        for (name, value) in fields {
            if secret_field_name(name) || secret.contains(name) {
                self.register_secret_field(value);
            } else if let serde_json::Value::Object(nested) = value {
                self.register_secret_fields(nested, &[]);
            }
        }
    }

    /// Mask these values from now on: a query's `secret_columns`.
    pub fn register_values(&self, values: impl IntoIterator<Item = String>) {
        let values: Vec<String> = values.into_iter().filter(|v| eligible(v)).collect();
        if values.is_empty() {
            return;
        }
        let mut guard = self.values.write().unwrap();
        let merged = sorted(guard.plain.iter().cloned().chain(values));
        *guard = Arc::new(Values::new(merged));
    }

    /// `s` with every secret value replaced by `*****`.
    pub fn mask<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if !self.enabled() {
            return Cow::Borrowed(s);
        }
        mask_with(s, &self.current().plain)
    }

    /// Mask a rendered message without changing the report payload. Markdown interpolation has
    /// already escaped punctuation, so both literal and escaped secret values must be matched.
    pub(crate) fn mask_markdown<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if !self.enabled() {
            return Cow::Borrowed(s);
        }
        mask_with(s, &self.current().markdown)
    }

    /// Message titles collapse whitespace before delivery. Match that representation as well as
    /// the literal secret when protecting a title in logs and run metadata.
    pub(crate) fn mask_message_title<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if !self.enabled() {
            return Cow::Borrowed(s);
        }
        mask_with(s, &self.current().title)
    }

    /// Whether masking is enabled and `s` contains a complete registered secret.
    pub fn contains_secret(&self, s: &str) -> bool {
        self.enabled() && matches!(mask_with(s, &self.current().plain), Cow::Owned(_))
    }

    /// Mask a source error before it reaches logs or output. Source plugins can transform or
    /// truncate a request in their diagnostics, so exact replacement is not sufficient for a
    /// secret-bearing request. In that case the opaque detail is omitted instead.
    pub fn mask_source_error<'a>(&self, error: &'a str, sensitive: bool) -> Cow<'a, str> {
        if !self.enabled() {
            return Cow::Borrowed(error);
        }
        mask_source_error_with(error, sensitive, &self.current().plain)
    }

    /// `v` with every secret value replaced by `*****` in all strings and object keys. Colliding
    /// masked keys receive `#2`, `#3`, and so on rather than silently replacing another entry.
    pub fn redact_json(&self, v: &mut serde_json::Value) {
        if self.enabled() {
            redact_json_with(v, &self.current().plain);
        }
    }

    fn redacted<T: serde::Serialize>(&self, v: &T) -> serde_json::Result<serde_json::Value> {
        let mut v = serde_json::to_value(v)?;
        self.redact_json(&mut v);
        Ok(v)
    }

    /// Pretty JSON of `v` with secrets redacted first.
    pub fn to_json_pretty<T: serde::Serialize>(&self, v: &T) -> serde_json::Result<String> {
        serde_json::to_string_pretty(&self.redacted(v)?)
    }

    /// One-line JSON of `v` with secrets redacted first.
    pub fn to_json_line<T: serde::Serialize>(&self, v: &T) -> serde_json::Result<String> {
        serde_json::to_string(&self.redacted(v)?)
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("enabled", &self.enabled())
            .finish_non_exhaustive()
    }
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
    fn secret_field_names_leave_paths_and_fingerprints_alone() {
        for name in [
            "password",
            "client_secret",
            "token",
            "api_key",
            "private_key",
            "key",
            "passphrase",
        ] {
            assert!(secret_field_name(name), "{name}");
        }
        for name in [
            "path",
            "host",
            "private_key_path",
            "host_key_fingerprint",
            "key_id",
            "keyspace",
            "schema",
        ] {
            assert!(!secret_field_name(name), "{name}");
        }
    }

    #[test]
    fn short_values_mask_where_they_stand_alone() {
        assert!(short("ééé") && !short("éééé") && !eligible(""));
        let secrets = sorted(["12".to_string(), "Ab9".to_string(), "s3cret".to_string()]);
        assert_eq!(
            mask_with("PIN 12, code Ab9; year 2012, x12y, Ab99, s3cretly", &secrets),
            "PIN *****, code *****; year 2012, x12y, Ab99, *****ly"
        );
        assert_eq!(mask_with("12", &secrets), "*****");
        assert_eq!(mask_with("12 12", &secrets), "***** *****");
        assert!(matches!(mask_with("2012", &secrets), Cow::Borrowed(_)));
    }
}
