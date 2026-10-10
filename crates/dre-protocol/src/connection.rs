//! Static checks of a profile entry's connection settings, from the fields a plugin declares:
//! a required field missing, an unknown key, a value of the wrong [`FieldKind`] or not one of
//! its `choices`. The SDK runs them for `validate_connection` and before `open` and `deliver`,
//! then the plugin's own rules, so `dre validate` and `dre run` say the same thing. Nothing here
//! touches the network.

use serde_json::{Map, Value};

use crate::Kind;
use crate::msg::{ConnectionField, FieldKind};

/// Keys of a profile entry that core reads, which a plugin may ignore.
pub const CORE_KEYS: &[&str] = &["threads"];

/// What the checks found: errors stop a run; warnings (an unknown key) don't.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Checked {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Check `connection` against `fields`. Keys in `unresolved` (an unset `env_var()`) count as set
/// but aren't checked. Messages name fields, never values.
pub fn check(
    kind: Kind,
    name: &str,
    fields: &[ConnectionField],
    connection: &Map<String, Value>,
    unresolved: &[String],
) -> Checked {
    let mut out = Checked::default();
    for (key, v) in connection {
        let Some(f) = fields.iter().find(|f| &f.name == key) else {
            // Core's own keys on a connection entry: `threads` (how many Bindings run at once).
            if CORE_KEYS.contains(&key.as_str()) {
                continue;
            }
            let hint = closest(key, fields.iter().map(|f| f.name.as_str()))
                .map(|c| format!("; did you mean `{c}`?"))
                .unwrap_or_default();
            out.warnings
                .push(format!("unknown key `{key}` for {kind} `{name}`, ignored{hint}"));
            continue;
        };
        if v.is_null() || unresolved.iter().any(|u| u == key) {
            continue;
        }
        // Jinja left in a value is rendered by core before it arrives; a literal `{{` isn't checked.
        if v.as_str().is_some_and(crate::options::is_template) {
            continue;
        }
        if let Some(e) = check_value(f, v) {
            out.errors.push(format!("`{key}` {e}"));
        }
    }
    for f in fields.iter().filter(|f| f.required && f.default.is_none()) {
        let set =
            connection.get(&f.name).is_some_and(|v| !v.is_null()) || unresolved.iter().any(|u| u == &f.name);
        if !set {
            out.errors.push(format!("`{}` is required", f.name));
        }
    }
    out
}

fn check_value(f: &ConnectionField, v: &Value) -> Option<String> {
    let ok = match f.kind {
        None => true,
        Some(FieldKind::String) => v.is_string() || v.is_number() || v.is_boolean(),
        Some(FieldKind::Integer) => {
            v.is_i64() || v.is_u64() || v.as_str().is_some_and(|s| s.trim().parse::<i64>().is_ok())
        }
        Some(FieldKind::Boolean) => v.is_boolean(),
        Some(FieldKind::Duration) => crate::delivery::parse_duration(v).is_ok(),
        Some(FieldKind::Map) => v.is_object(),
        Some(FieldKind::Path) => match v.as_str() {
            Some(p) => {
                let path = expand_home(p);
                if !path.exists() {
                    return Some(format!("names a file that doesn't exist ({})", path.display()));
                }
                true
            }
            None => false,
        },
    };
    if !ok {
        return Some(match f.kind {
            Some(FieldKind::Integer) => "must be a whole number".into(),
            Some(FieldKind::Boolean) => "must be true or false".into(),
            Some(FieldKind::Duration) => "must be a duration such as `30s` or `2m`, or seconds".into(),
            Some(FieldKind::Map) => "must be a block of settings".into(),
            Some(FieldKind::Path) => "must be a file path".into(),
            _ => "must be a string".into(),
        });
    }
    if !f.choices.is_empty() {
        let s = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if !f.choices.contains(&s) {
            let shown: Vec<String> = f.choices.iter().map(|c| format!("`{c}`")).collect();
            return Some(format!("must be one of {}", shown.join(", ")));
        }
    }
    None
}

/// `~/` as the home directory, as the plugins read paths.
fn expand_home(p: &str) -> std::path::PathBuf {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => std::path::PathBuf::from(p),
    }
}

/// The declared name closest to `key`, if it's near enough to be a typo.
pub fn closest<'a>(key: &str, names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    names
        .map(|n| (distance(key, n), n))
        .filter(|(d, n)| *d <= 2.max(n.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// `messages` with every secret field's value replaced by `*****`, so a plugin's rule that
/// quotes a value can't leak it.
pub fn redact(
    messages: Vec<String>,
    fields: &[ConnectionField],
    connection: &Map<String, Value>,
) -> Vec<String> {
    let secrets: Vec<&str> = fields
        .iter()
        .filter(|f| f.secret)
        .filter_map(|f| connection.get(&f.name).and_then(Value::as_str))
        .filter(|s| s.len() >= 3)
        .collect();
    messages
        .into_iter()
        .map(|m| secrets.iter().fold(m, |m, s| m.replace(s, "*****")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields() -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "").required(),
            ConnectionField::new("port", "")
                .kind(FieldKind::Integer)
                .default(5432),
            ConnectionField::new("password", "").secret(),
            ConnectionField::new("sslmode", "").choices(&["disable", "require"]),
            ConnectionField::new("key_path", "").kind(FieldKind::Path),
            ConnectionField::new("timeout", "").kind(FieldKind::Duration),
            ConnectionField::new("passive", "").kind(FieldKind::Boolean),
        ]
    }

    fn run(c: Value, unresolved: &[&str]) -> Checked {
        let u: Vec<String> = unresolved.iter().map(|s| s.to_string()).collect();
        check(Kind::Source, "pg", &fields(), c.as_object().unwrap(), &u)
    }

    #[test]
    fn required_kind_choices_and_unknown_keys() {
        let c = run(
            json!({"port": "abc", "sslmode": "verify", "timeout": "soon", "passive": "yes", "hots": "x"}),
            &[],
        );
        assert_eq!(
            c.errors,
            [
                "`port` must be a whole number",
                "`sslmode` must be one of `disable`, `require`",
                "`timeout` must be a duration such as `30s` or `2m`, or seconds",
                "`passive` must be true or false",
                "`host` is required",
            ]
        );
        assert_eq!(
            c.warnings,
            ["unknown key `hots` for source `pg`, ignored; did you mean `host`?"]
        );
        let ok = run(
            json!({"host": "h", "port": "5432", "timeout": "30s", "passive": true}),
            &[],
        );
        assert_eq!(ok, Checked::default());
    }

    #[test]
    fn a_path_must_exist_and_unresolved_values_count_as_set() {
        let c = run(json!({"host": "h", "key_path": "/no/such/key"}), &[]);
        assert!(
            c.errors[0].starts_with("`key_path` names a file that doesn't exist"),
            "{c:?}"
        );
        let c = run(json!({"host": null, "port": null}), &["host", "port"]);
        assert_eq!(c, Checked::default());
    }

    #[test]
    fn secrets_are_redacted() {
        let c = json!({"password": "hunter22"});
        let m = redact(
            vec!["bad password hunter22".into()],
            &fields(),
            c.as_object().unwrap(),
        );
        assert_eq!(m, ["bad password *****"]);
    }
}
