//! Per-row outputs (`for_each`): the pieces that don't need a run. The report's queries run once;
//! the driver query gives one row per copy of the output, and every other query of the output is
//! split by the same key, so each copy gets only its own slice. See `run.rs` for the run itself.

use std::sync::Arc;

/// A key as a file name: letters and digits of any script, `.`, `-` and `_` kept, anything else
/// `_`; never empty, `.` or `..`, at most 100 bytes (on a character boundary), so the extension
/// still fits in a file system's 255-byte limit.
pub fn safe_name(key: &str) -> String {
    let mut s = String::new();
    for c in key.chars() {
        let c = if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '_'
        };
        if s.len() + c.len_utf8() > 100 {
            break;
        }
        s.push(c);
    }
    if s.is_empty() || s.chars().all(|c| c == '.') || (cfg!(windows) && device_name(&s)) {
        s = format!("_{s}");
    }
    s
}

/// A Windows device name (`CON`, `nul.txt`, `COM1.xlsx`...): never a file there.
fn device_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    match stem.as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', n] | [b'L', b'P', b'T', n] => n.is_ascii_digit() && *n != b'0',
        _ => false,
    }
}

/// Whether a rendered per-row `file_name` names one file in the output's folder: no folder,
/// `.`/`..`, root or drive; on Windows also no `:` (a drive or a stream) and no device name.
pub fn plain_file_name(name: &str) -> bool {
    let mut parts = std::path::Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(n)), None) if n == name
    ) && !name.contains(['/', '\\'])
        && !(cfg!(windows) && (name.contains(':') || device_name(name)))
}

/// What a per-row destination path rendered from `source` got from the row's values that
/// would take it out of the destination's folder: a `..`, an absolute path or drive, or a URI
/// scheme (`s3://other-bucket/...`, which names another bucket) the YAML didn't have, or a
/// Windows device name. `None` when it's safe.
pub fn path_escape(source: &str, rendered: &str) -> Option<&'static str> {
    // `<scheme>://` at the start: a URI the path's own scheme and authority come from.
    let uri = |p: &str| {
        let p = p.trim_start();
        p.split_once("://").is_some_and(|(scheme, _)| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        })
    };
    if uri(rendered) && !uri(source) {
        return Some("a URI scheme or authority");
    }
    let rooted = |p: &str| {
        let b = p.trim_start().as_bytes();
        b.first().is_some_and(|c| matches!(c, b'/' | b'\\'))
            || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
    };
    let parents = |p: &str| p.split(['/', '\\']).filter(|s| s.trim() == "..").count();
    if rooted(rendered) && !rooted(source) {
        return Some("an absolute path or a drive");
    }
    if parents(rendered) > parents(source) {
        return Some("a `..`");
    }
    if rendered.split(['/', '\\']).any(device_name) {
        return Some("a Windows device name");
    }
    None
}

/// A per-row value as logs and results show it: an email keeps its first character and domain
/// (`j***@example.com`), anything else is `*****`.
pub fn mask_value(v: &str) -> String {
    if let Some((user, domain)) = v.split_once('@')
        && !user.is_empty()
        && domain.contains('.')
        && !v.contains(char::is_whitespace)
    {
        let first: String = user.chars().take(1).collect();
        return format!("{first}***@{domain}");
    }
    crate::secrets::MASK.to_string()
}

/// Masks a row's values wherever they appear in a text (a rendered path, a plugin's error).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RowMask {
    /// (value, its mask), longest first.
    values: Arc<Vec<(String, String)>>,
}

impl RowMask {
    /// The row's string values worth hiding: every `each.*` value but the key's, at least three
    /// characters long (shorter ones would mask parts of unrelated words).
    pub fn new(each: &serde_json::Map<String, serde_json::Value>, key: &[String]) -> RowMask {
        let mut values: Vec<(String, String)> = each
            .iter()
            .filter(|(k, _)| !key.contains(k))
            .filter_map(|(_, v)| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| s.chars().count() >= 3)
            .map(|s| {
                let m = mask_value(&s);
                (s, m)
            })
            .collect();
        values.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        values.dedup();
        RowMask {
            values: Arc::new(values),
        }
    }

    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (v, m) in self.values.iter() {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), m);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_name("ab/../c d"), "ab_.._c_d");
        assert_eq!(safe_name(".."), "_..");
        assert_eq!(safe_name(""), "_");
        // Keys in any script keep their letters, so `東京` and `大阪` get files of their own.
        assert_eq!(safe_name("東京"), "東京");
        assert_eq!(safe_name("Zoë/Ødegaard 東京"), "Zoë_Ødegaard_東京");
        assert_eq!(safe_name("株式会社 ABC"), "株式会社_ABC");
        // Cut at 100 bytes, never inside a character: 33 three-byte characters.
        let long = safe_name(&"東".repeat(50));
        assert_eq!(long, "東".repeat(33));
        assert_eq!(safe_name(&"a".repeat(150)).len(), 100);
    }

    #[test]
    fn a_row_value_cannot_add_a_uri_scheme_or_bucket() {
        let bare = "{{ each.id }}/statement.pdf";
        for rendered in [
            "s3://outside/stolen/statement.pdf",
            "gs://outside/stolen/statement.pdf",
            " az://outside/stolen/statement.pdf",
        ] {
            assert_eq!(
                path_escape(bare, rendered),
                Some("a URI scheme or authority"),
                "{rendered}"
            );
        }
        assert_eq!(path_escape(bare, "101/statement.pdf"), None);
        // A URI the YAML wrote, with or without a templated bucket, is the author's.
        assert_eq!(
            path_escape("s3://statements/{{ each.id }}.pdf", "s3://statements/101.pdf"),
            None
        );
        assert_eq!(
            path_escape(
                "s3://{{ each.bucket }}/{{ each.id }}.pdf",
                "s3://eu-statements/101.pdf"
            ),
            None
        );
    }

    #[test]
    fn masking() {
        assert_eq!(mask_value("jane.doe@example.com"), "j***@example.com");
        assert_eq!(mask_value("Jane Doe"), "*****");
        let each = serde_json::json!({"member_id": 12345, "email": "jane@example.com", "name": "Jane Doe", "x": "ab"});
        let m = RowMask::new(each.as_object().unwrap(), &["member_id".into()]);
        assert_eq!(
            m.apply("to jane@example.com for Jane Doe, member 12345, ab"),
            "to j***@example.com for *****, member 12345, ab"
        );
    }
}
