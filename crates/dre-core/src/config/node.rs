//! A parsed YAML document as a tree of nodes, each with the line and column it was written at.
//!
//! YAML is parsed by `serde-saphyr`, YAML 1.2 with strict booleans: only `true` and `false` are
//! booleans, so `yes`, `no`, `on` and `off` stay strings. Duplicate keys are errors; merge keys
//! (`<<: *base`) are expanded. Typed config is then read from the tree (see [`super::de`]), so every
//! diagnostic can point at the exact line of the key or value it's about.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_saphyr::Spanned;

/// A YAML value and where it was written (1-based; 0 when unknown).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: Kind,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Seq(Vec<Node>),
    /// In the file's order. Keys are strings: a scalar key (`2024:`) keeps its spelling.
    Map(Vec<(Key, Node)>),
}

/// A map key and its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub name: String,
    pub line: usize,
}

/// A YAML syntax error, with its line when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: Option<usize>,
    pub message: String,
}

impl Kind {
    /// How a value of this kind is named in diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Null => "nothing",
            Kind::Bool(_) => "a boolean",
            Kind::Int(_) | Kind::UInt(_) | Kind::Float(_) => "a number",
            Kind::Str(_) => "a string",
            Kind::Seq(_) => "a list",
            Kind::Map(_) => "a map",
        }
    }
}

impl Node {
    pub fn null() -> Node {
        Node {
            kind: Kind::Null,
            line: 0,
            column: 0,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(Key, Node)]> {
        match &self.kind {
            Kind::Map(m) => Some(m),
            _ => None,
        }
    }

    /// A key's value in a map node.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?.iter().find(|(k, _)| k.name == key).map(|(_, v)| v)
    }

    /// A key's entry (key and value) in a map node.
    pub fn entry(&self, key: &str) -> Option<&(Key, Node)> {
        self.as_map()?.iter().find(|(k, _)| k.name == key)
    }

    /// The node as JSON, for values handed on unvalidated (a plugin's fields). Keys are strings;
    /// numbers that don't fit JSON's become strings.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match &self.kind {
            Kind::Null => J::Null,
            Kind::Bool(b) => J::Bool(*b),
            Kind::Int(i) => J::from(*i),
            Kind::UInt(u) => J::from(*u),
            Kind::Float(f) => {
                serde_json::Number::from_f64(*f).map_or_else(|| J::String(f.to_string()), J::Number)
            }
            Kind::Str(s) => J::String(s.clone()),
            Kind::Seq(s) => J::Array(s.iter().map(Node::to_json).collect()),
            Kind::Map(m) => J::Object(m.iter().map(|(k, v)| (k.name.clone(), v.to_json())).collect()),
        }
    }
}

/// Parse a YAML document. An empty document is a null node.
pub fn parse(text: &str) -> Result<Node, SyntaxError> {
    let mut options = serde_saphyr::options::Options::default();
    options.strict_booleans = true;
    options.duplicate_keys = serde_saphyr::options::DuplicateKeyPolicy::Error;
    options.with_snippet = false;
    if text.trim().is_empty() {
        return Ok(Node::null());
    }
    match serde_saphyr::from_str_with_options::<Spanned<Raw>>(text, options) {
        Ok(raw) => Ok(raw.into()),
        Err(e) => Err(syntax_error(&e)),
    }
}

fn syntax_error(e: &serde_saphyr::Error) -> SyntaxError {
    static LOCATION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:error: )?(?:line \d+ column \d+: )?(.*?)(?: at line \d+, column \d+)?$")
            .unwrap()
    });
    let line = e.location().map(|l| l.line() as usize).filter(|l| *l > 0);
    let text = e.to_string();
    let first = text.lines().next().unwrap_or_default();
    // DRE prints the line itself, and serde-saphyr's hint names its own options.
    let message = LOCATION
        .captures(first)
        .map_or(first, |c| c.get(1).map_or(first, |m| m.as_str()));
    let message = message.replace(", set DuplicateKeyPolicy in Options if acceptable", "");
    SyntaxError { line, message }
}

/// The tree as serde-saphyr hands it over: every value with its location.
enum Raw {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Seq(Vec<Spanned<Raw>>),
    Map(Vec<(Spanned<String>, Spanned<Raw>)>),
}

impl From<Spanned<Raw>> for Node {
    fn from(s: Spanned<Raw>) -> Node {
        let kind = match s.value {
            Raw::Null => Kind::Null,
            Raw::Bool(b) => Kind::Bool(b),
            Raw::Int(i) => Kind::Int(i),
            Raw::UInt(u) => Kind::UInt(u),
            Raw::Float(f) => Kind::Float(f),
            Raw::Str(t) => Kind::Str(t),
            Raw::Seq(items) => Kind::Seq(items.into_iter().map(Node::from).collect()),
            Raw::Map(entries) => Kind::Map(
                entries
                    .into_iter()
                    .map(|(k, v)| {
                        let key = Key {
                            name: k.value,
                            line: k.referenced.line() as usize,
                        };
                        (key, Node::from(v))
                    })
                    .collect(),
            ),
        };
        Node {
            kind,
            line: s.referenced.line() as usize,
            column: s.referenced.column() as usize,
        }
    }
}

impl<'de> Deserialize<'de> for Raw {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Raw;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a YAML value")
            }
            fn visit_unit<E>(self) -> Result<Raw, E> {
                Ok(Raw::Null)
            }
            fn visit_none<E>(self) -> Result<Raw, E> {
                Ok(Raw::Null)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Raw, D::Error> {
                Raw::deserialize(d)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Raw, E> {
                Ok(Raw::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Raw, E> {
                Ok(Raw::Int(v))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Raw, E> {
                Ok(i64::try_from(v).map_or(Raw::UInt(v), Raw::Int))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Raw, E> {
                Ok(Raw::Float(v))
            }
            fn visit_str<E>(self, v: &str) -> Result<Raw, E> {
                Ok(Raw::Str(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> Result<Raw, E> {
                Ok(Raw::Str(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Raw, A::Error> {
                let mut items = Vec::new();
                while let Some(x) = a.next_element()? {
                    items.push(x);
                }
                Ok(Raw::Seq(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Raw, A::Error> {
                let mut entries = Vec::new();
                while let Some(e) = a.next_entry()? {
                    entries.push(e);
                }
                Ok(Raw::Map(entries))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_keep_their_lines() {
        let n = parse("a:\n  b: 1\n  c: [x, y]\n").unwrap();
        let a = n.get("a").unwrap();
        assert_eq!(n.entry("a").unwrap().0.line, 1);
        assert_eq!(a.entry("c").unwrap().0.line, 3);
        assert_eq!(a.get("b").unwrap().kind, Kind::Int(1));
        assert_eq!(a.get("b").unwrap().line, 2);
    }

    #[test]
    fn only_true_and_false_are_booleans() {
        let n = parse("a: yes\nb: no\nc: on\nd: true\n").unwrap();
        assert_eq!(n.get("a").unwrap().as_str(), Some("yes"));
        assert_eq!(n.get("c").unwrap().as_str(), Some("on"));
        assert_eq!(n.get("d").unwrap().kind, Kind::Bool(true));
    }

    #[test]
    fn merge_keys_are_expanded_and_written_keys_win() {
        let n = parse("base: &b {type: duckdb, path: x}\ndev: {<<: *b, path: y}\n").unwrap();
        let dev = n.get("dev").unwrap();
        assert_eq!(dev.get("type").unwrap().as_str(), Some("duckdb"));
        assert_eq!(dev.get("path").unwrap().as_str(), Some("y"));
    }

    #[test]
    fn duplicate_keys_are_errors_with_their_line() {
        let e = parse("a: 1\na: 2\n").unwrap_err();
        assert_eq!(e.line, Some(2));
        assert_eq!(e.message, "duplicate mapping key: a");
    }

    #[test]
    fn syntax_errors_have_a_line() {
        let e = parse("a:\n  b: [1\n").unwrap_err();
        assert_eq!(e.line, Some(2));
        assert!(!e.message.starts_with("line"), "{}", e.message);
    }

    #[test]
    fn scalar_keys_are_strings_and_empty_files_are_null() {
        assert_eq!(
            parse("2024: x\n").unwrap().get("2024").unwrap().as_str(),
            Some("x")
        );
        assert_eq!(parse("\n# nothing\n").unwrap().kind, Kind::Null);
    }

    #[test]
    fn jinja_in_quoted_values_is_a_string() {
        let n = parse("pw: \"{{ env_var('X') }}\"\n").unwrap();
        assert_eq!(n.get("pw").unwrap().as_str(), Some("{{ env_var('X') }}"));
    }
}
