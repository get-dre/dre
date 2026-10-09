//! YAML loading with file/line locations for diagnostics.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::codes::Code;
use crate::config::node::{self, Kind, Node};
use crate::diag::Diagnostics;

/// A parsed YAML file, kept with its source text so keys can be located for diagnostics.
#[derive(Debug, Clone)]
pub struct YamlFile {
    /// Path as shown in diagnostics (relative to the project root when inside it).
    pub display: PathBuf,
    pub text: String,
    /// The file as the value config not yet read into typed structs works on.
    pub value: Value,
    /// The parsed file, with every value's line.
    pub node: Node,
}

impl YamlFile {
    /// Parse `path`, recording a diagnostic (with line) and returning `None` on failure.
    pub fn load(path: &Path, display: PathBuf, diags: &mut Diagnostics) -> Option<YamlFile> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                diags.error(
                    Code::IoError,
                    Some(display),
                    None,
                    format!("cannot read file: {e}"),
                );
                return None;
            }
        };
        Self::parse(text, display, diags)
    }

    pub fn parse(text: String, display: PathBuf, diags: &mut Diagnostics) -> Option<YamlFile> {
        match node::parse(&text) {
            Ok(node) => {
                let value = node.to_json();
                Some(YamlFile {
                    display,
                    text,
                    value,
                    node,
                })
            }
            Err(e) => {
                diags.error(
                    Code::YamlSyntax,
                    Some(display),
                    e.line,
                    format!("invalid YAML: {}", e.message),
                );
                None
            }
        }
    }

    /// Best-effort line of `key:` in this file. With `after`, searches from that line on,
    /// which is enough to find nested keys under a located parent.
    pub fn line_of(&self, key: &str, after: Option<usize>) -> Option<usize> {
        let start = after.unwrap_or(1);
        self.text.lines().enumerate().skip(start - 1).find_map(|(i, l)| {
            let t = l.trim_start().trim_start_matches("- ").trim_start();
            let t = t
                .strip_prefix('"')
                .and_then(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('"')))
                .or_else(|| {
                    t.strip_prefix('\'')
                        .and_then(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('\'')))
                })
                .or_else(|| t.strip_prefix(key));
            match t {
                Some(rest) if rest.trim_start().starts_with(':') => Some(i + 1),
                _ => None,
            }
        })
    }

    /// Best-effort line of the first occurrence of `needle` anywhere in the file.
    pub fn line_containing(&self, needle: &str) -> Option<usize> {
        self.text.lines().position(|l| l.contains(needle)).map(|i| i + 1)
    }
}

/// A value as a node with no lines, to read typed config from values merged from several files
/// (outputs).
pub fn to_node(v: &Value) -> Node {
    let kind = match v {
        Value::Null => Kind::Null,
        Value::Bool(b) => Kind::Bool(*b),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => Kind::Int(i),
            (None, Some(u), _) => Kind::UInt(u),
            (_, _, f) => Kind::Float(f.unwrap_or(f64::NAN)),
        },
        Value::String(s) => Kind::Str(s.clone()),
        Value::Array(s) => Kind::Seq(s.iter().map(to_node).collect()),
        Value::Object(m) => Kind::Map(
            m.iter()
                .map(|(k, v)| {
                    (
                        node::Key {
                            name: k.clone(),
                            line: 0,
                        },
                        to_node(v),
                    )
                })
                .collect(),
        ),
    };
    Node {
        kind,
        line: 0,
        column: 0,
    }
}

/// Render a YAML scalar as a short string for messages.
pub fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Option<YamlFile>, Diagnostics) {
        let mut d = Diagnostics::default();
        (
            YamlFile::parse(text.to_string(), PathBuf::from("t.yml"), &mut d),
            d,
        )
    }

    #[test]
    fn merge_keys_are_applied_and_explicit_keys_win() {
        let (yf, _) = parse(
            "base: &b {type: postgres, host: h, database: shop}\n\
             other: &o {port: 5433, host: other}\n\
             prod:\n  <<: *b\n  database: shop_prod\n\
             both:\n  <<: [*b, *o]\n",
        );
        let v = yf.unwrap().value;
        assert_eq!(v["prod"]["type"], "postgres");
        assert_eq!(v["prod"]["database"], "shop_prod");
        assert!(v["prod"].get("<<").is_none());
        // With a list, earlier maps win over later ones.
        assert_eq!(v["both"]["host"], "h");
        assert_eq!(v["both"]["port"], 5433);
    }

    #[test]
    fn merges_chain() {
        let (yf, _) = parse(
            "dev: &pg {type: postgres, host: h, sslmode: prefer}\n\
             verify: &v {<<: *pg, sslmode: verify-full}\n\
             wrong: {<<: *v, host: other}\n",
        );
        let v = yf.unwrap().value;
        assert_eq!(v["wrong"]["type"], "postgres");
        assert_eq!(v["wrong"]["sslmode"], "verify-full");
        assert_eq!(v["wrong"]["host"], "other");
        assert!(v["wrong"].get("<<").is_none());
        let (yf, d) = parse("a: {<<: 3}\n");
        assert!(yf.is_none() && format!("{d:?}").contains("must be a map"));
    }

    #[test]
    fn scalar_keys_become_names() {
        let (yf, _) = parse("output: {format: csv, null: NULL, true: 1, 2024: x}\n");
        let v = yf.unwrap().value;
        assert!(v["output"]["null"].is_null());
        assert_eq!(v["output"]["true"], 1);
        assert_eq!(v["output"]["2024"], "x");
        let (yf, _) = parse("output: {null: \"NULL\"}\n");
        assert_eq!(yf.unwrap().value["output"]["null"], "NULL");
    }

    #[test]
    fn complex_keys_are_an_error() {
        let (yf, d) = parse("? [a, b]\n: 1\n");
        assert!(yf.is_none());
        assert!(format!("{d:?}").contains("keys must be names"));
    }
}
