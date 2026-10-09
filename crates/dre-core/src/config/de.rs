//! Typed config read from a [`Node`] tree with serde, keeping each value's location.
//!
//! Config structs derive `Deserialize` (and `JsonSchema`, which generates `docs/schemas/`) and are
//! read with [`from_node`]. Three wrappers carry what plain serde loses:
//!
//! - [`Located<T>`]: the value with the line and column it was written at.
//! - [`Loose<T>`]: the value, or, when it has the wrong shape, what was found and where, so the
//!   loader reports DRE's own message and carries on with the rest of the file instead of
//!   stopping at the first problem.
//! - [`OneOf<A, B>`]: an `A`, else a `B` (a query given by name or as a map), keeping both
//!   readings' locations, which serde's untagged enums lose.
//! - [`UnknownKeys`]: in a struct field named [`UNKNOWN`], the keys the struct doesn't define,
//!   with their lines. A struct without that field ignores unknown keys. The field can also be a
//!   [`Map`], to read the other keys' values (folder config: folder names next to `+` keys).

use std::fmt;
use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};

use super::node::{Key, Kind, Node};

/// The field name that collects a struct's unknown keys: `#[serde(rename = "$unknown", default)]`.
pub const UNKNOWN: &str = "$unknown";
const LOCATED: &str = "$dre::Located";
const LOOSE: &str = "$dre::Loose";
const ONE_OF: &str = "$dre::OneOf";

/// A config value of type `T` from `node`. Fails only where no [`Loose`] catches the problem.
pub fn from_node<'a, T: Deserialize<'a>>(node: &'a Node) -> Result<T, Error> {
    T::deserialize(NodeDe(node, node.line))
}

/// A problem reading typed config: the message, and the line of the value it's about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error {
            line: None,
            message: msg.to_string(),
        }
    }
}

impl Error {
    fn at(mut self, line: usize) -> Self {
        if self.line.is_none() && line > 0 {
            self.line = Some(line);
        }
        self
    }
}

/// A value and where it was written: for a map entry, the line of its key (where a diagnostic
/// about it points); for a list item, the item's own line.
#[derive(Debug, Clone, PartialEq)]
pub struct Located<T> {
    pub value: T,
    pub line: usize,
    pub column: usize,
}

impl<T> Located<T> {
    /// The same location with another value.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Located<U> {
        Located {
            value: f(self.value),
            line: self.line,
            column: self.column,
        }
    }

    /// The line, `None` when unknown.
    pub fn line(&self) -> Option<usize> {
        (self.line > 0).then_some(self.line)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Located<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Located<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a located value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let line = seq.next_element()?.unwrap_or(0);
                let column = seq.next_element()?.unwrap_or(0);
                let value = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("a located value without a value"))?;
                Ok(Located { value, line, column })
            }
        }
        d.deserialize_newtype_struct(LOCATED, V(PhantomData))
    }
}

impl<T: JsonSchema> JsonSchema for Located<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(g)
    }
    fn inline_schema() -> bool {
        T::inline_schema()
    }
}

/// A value of type `T`, or what was found instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Loose<T> {
    Ok(T),
    Bad(Found),
}

/// A value that didn't have the expected shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// What it was: `a map`, `a list`, `a string`, ... (see [`Kind::name`]).
    pub kind: &'static str,
    pub line: usize,
    /// serde's description of the problem, for messages without a DRE wording.
    pub problem: String,
}

impl<T> Loose<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Loose<U> {
        match self {
            Loose::Ok(v) => Loose::Ok(f(v)),
            Loose::Bad(b) => Loose::Bad(b),
        }
    }

    pub fn ok(&self) -> Option<&T> {
        match self {
            Loose::Ok(v) => Some(v),
            Loose::Bad(_) => None,
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Loose<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Loose<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                match seq.next_element::<T>() {
                    Ok(Some(v)) => Ok(Loose::Ok(v)),
                    Ok(None) => Err(de::Error::custom("a loose value without a value")),
                    Err(e) => {
                        let line = seq.next_element::<usize>()?.unwrap_or(0);
                        let kind = seq.next_element::<String>()?.unwrap_or_default();
                        Ok(Loose::Bad(Found {
                            kind: kind_name(&kind),
                            line,
                            problem: e.to_string(),
                        }))
                    }
                }
            }
        }
        d.deserialize_newtype_struct(LOOSE, V(PhantomData))
    }
}

fn kind_name(name: &str) -> &'static str {
    ["nothing", "a boolean", "a number", "a string", "a list", "a map"]
        .into_iter()
        .find(|k| *k == name)
        .unwrap_or("a value")
}

impl<T: JsonSchema> JsonSchema for Loose<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(g)
    }
    fn inline_schema() -> bool {
        T::inline_schema()
    }
}

/// A key that may be absent, unlike `Option`, which also reads `key: null` as absent. Use with
/// `#[serde(default)]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Maybe<T> {
    #[default]
    Absent,
    Given(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Maybe<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Maybe::Given)
    }
}

impl<T: JsonSchema> JsonSchema for Maybe<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(g)
    }
    fn inline_schema() -> bool {
        T::inline_schema()
    }
}

/// An `A`, else a `B`, read from the same value.
#[derive(Debug, Clone, PartialEq)]
pub enum OneOf<A, B> {
    A(A),
    B(B),
}

impl<'de, A: Deserialize<'de>, B: Deserialize<'de>> Deserialize<'de> for OneOf<A, B> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<A, B>(PhantomData<(A, B)>);
        impl<'de, A: Deserialize<'de>, B: Deserialize<'de>> Visitor<'de> for V<A, B> {
            type Value = OneOf<A, B>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("one of two shapes")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                if let Ok(Some(a)) = seq.next_element::<A>() {
                    return Ok(OneOf::A(a));
                }
                match seq.next_element::<B>()? {
                    Some(b) => Ok(OneOf::B(b)),
                    None => Err(de::Error::custom("one of two shapes without a value")),
                }
            }
        }
        d.deserialize_newtype_struct(ONE_OF, V(PhantomData))
    }
}

impl<A: JsonSchema, B: JsonSchema> JsonSchema for OneOf<A, B> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("OneOf_{}_{}", A::schema_name(), B::schema_name()).into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // `OneOf<A, OneOf<B, C>>` is one list of three alternatives.
        let mut alternatives = Vec::new();
        for s in [g.subschema_for::<A>(), g.subschema_for::<B>()] {
            match s.as_object() {
                Some(o) if o.len() == 1 && o.contains_key("oneOf") => {
                    alternatives.extend(o["oneOf"].as_array().into_iter().flatten().cloned());
                }
                _ => alternatives.push(s.to_value()),
            }
        }
        schemars::json_schema!({"oneOf": alternatives})
    }
    fn inline_schema() -> bool {
        true
    }
}

/// The keys of a map that its struct doesn't define, in the file's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnknownKeys(pub Vec<Key>);

impl<'de> Deserialize<'de> for UnknownKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = UnknownKeys;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unknown keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<UnknownKeys, A::Error> {
                let mut keys = Vec::new();
                while let Some((k, _)) = m.next_entry::<Located<String>, de::IgnoredAny>()? {
                    keys.push(Key {
                        name: k.value,
                        line: k.line,
                    });
                }
                Ok(UnknownKeys(keys))
            }
        }
        d.deserialize_map(V)
    }
}

/// A map in the file's order, each key with its line: `Map<V>` for a YAML map of names.
#[derive(Debug, Clone, PartialEq)]
pub struct Map<V>(pub Vec<(Located<String>, V)>);

impl<V> Default for Map<V> {
    fn default() -> Self {
        Map(Vec::new())
    }
}

impl<V> Map<V> {
    pub fn iter(&self) -> impl Iterator<Item = (&Located<String>, &V)> {
        self.0.iter().map(|(k, v)| (k, v))
    }
    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k.value == key).map(|(_, v)| v)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Map<V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Vis<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for Vis<V> {
            type Value = Map<V>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map")
            }
            fn visit_unit<E>(self) -> Result<Map<V>, E> {
                Ok(Map(Vec::new()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Map<V>, A::Error> {
                let mut entries = Vec::new();
                while let Some(e) = m.next_entry()? {
                    entries.push(e);
                }
                Ok(Map(entries))
            }
        }
        d.deserialize_map(Vis(PhantomData))
    }
}

impl<V: JsonSchema> JsonSchema for Map<V> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Map_of_{}", V::schema_name()).into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "object", "additionalProperties": g.subschema_for::<V>()})
    }
    fn inline_schema() -> bool {
        true
    }
}

/// Reads a map key: a string, or a [`Located`] string with the key's line.
struct KeyDe<'a>(&'a Key);

impl<'de> Deserializer<'de> for KeyDe<'de> {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_borrowed_str(&self.0.name)
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        if name == LOCATED {
            v.visit_seq(KeySeq { key: self.0, at: 0 })
        } else {
            v.visit_newtype_struct(self)
        }
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

struct KeySeq<'a> {
    key: &'a Key,
    at: usize,
}

impl<'a> SeqAccess<'a> for KeySeq<'a> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'a>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.at += 1;
        match self.at {
            1 => seed.deserialize(self.key.line.into_deserializer()).map(Some),
            2 => seed.deserialize(0usize.into_deserializer()).map(Some),
            3 => seed.deserialize(KeyDe(self.key)).map(Some),
            _ => Ok(None),
        }
    }
}

/// Reads typed values from a node; `.1` is the line its entry was written at (its key's line in
/// a map).
#[derive(Clone, Copy)]
struct NodeDe<'a>(&'a Node, usize);

impl<'a> NodeDe<'a> {
    fn err(&self, msg: impl fmt::Display) -> Error {
        Error {
            line: (self.1 > 0).then_some(self.1),
            message: msg.to_string(),
        }
    }

    fn mismatch(&self, expected: &str) -> Error {
        self.err(format!("expected {expected}, found {}", self.0.kind.name()))
    }
}

macro_rules! via_any {
    ($($m:ident)*) => {$(
        fn $m<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
            self.deserialize_any(v)
        }
    )*};
}

impl<'a> Deserializer<'a> for NodeDe<'a> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        let line = self.0.line;
        match &self.0.kind {
            Kind::Null => v.visit_unit(),
            Kind::Bool(b) => v.visit_bool(*b),
            Kind::Int(i) => v.visit_i64(*i),
            Kind::UInt(u) => v.visit_u64(*u),
            Kind::Float(f) => v.visit_f64(*f),
            Kind::Str(s) => v.visit_borrowed_str(s),
            Kind::Seq(items) => v.visit_seq(Items(items.iter())),
            Kind::Map(entries) => v.visit_map(Entries {
                entries: entries.iter(),
                value: None,
                fields: None,
                unknown: Vec::new(),
                unknown_done: true,
            }),
        }
        .map_err(|e| e.at(line))
    }

    via_any!(deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_f32
        deserialize_f64 deserialize_char deserialize_bytes deserialize_byte_buf
        deserialize_unit deserialize_identifier deserialize_ignored_any);

    fn deserialize_str<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        match &self.0.kind {
            Kind::Str(s) => v.visit_borrowed_str(s),
            _ => Err(self.mismatch("a string")),
        }
    }

    fn deserialize_string<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }

    fn deserialize_option<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        match self.0.kind {
            Kind::Null => v.visit_none(),
            _ => v.visit_some(self),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'a>>(self, _: &'static str, v: V) -> Result<V::Value, Error> {
        self.deserialize_unit(v)
    }

    fn deserialize_newtype_struct<V: Visitor<'a>>(self, name: &'static str, v: V) -> Result<V::Value, Error> {
        match name {
            LOCATED => v.visit_seq(LocatedSeq {
                node: self.0,
                line: self.1,
                at: 0,
            }),
            LOOSE => v.visit_seq(LooseSeq {
                node: self.0,
                line: self.1,
                at: 0,
            }),
            ONE_OF => v.visit_seq(OneOfSeq {
                node: self.0,
                line: self.1,
                at: 0,
            }),
            _ => v.visit_newtype_struct(self),
        }
    }

    fn deserialize_seq<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        match &self.0.kind {
            Kind::Seq(items) => v.visit_seq(Items(items.iter())).map_err(|e| e.at(self.0.line)),
            _ => Err(self.mismatch("a list")),
        }
    }

    fn deserialize_tuple<V: Visitor<'a>>(self, _: usize, v: V) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_tuple_struct<V: Visitor<'a>>(
        self,
        _: &'static str,
        _: usize,
        v: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_map<V: Visitor<'a>>(self, v: V) -> Result<V::Value, Error> {
        match &self.0.kind {
            Kind::Map(_) => self.deserialize_any(v),
            _ => Err(self.mismatch("a map")),
        }
    }

    fn deserialize_struct<V: Visitor<'a>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        let Kind::Map(entries) = &self.0.kind else {
            return Err(self.mismatch("a map"));
        };
        let collects = fields.contains(&UNKNOWN);
        v.visit_map(Entries {
            entries: entries.iter(),
            value: None,
            fields: Some(fields),
            unknown: Vec::new(),
            unknown_done: !collects,
        })
        .map_err(|e| e.at(self.0.line))
    }

    fn deserialize_enum<V: Visitor<'a>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        match &self.0.kind {
            Kind::Str(s) => v.visit_enum(s.as_str().into_deserializer()),
            _ => Err(self.mismatch("a name")),
        }
    }
}

struct Items<'a>(std::slice::Iter<'a, Node>);

impl<'a> SeqAccess<'a> for Items<'a> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'a>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.0
            .next()
            .map(|n| seed.deserialize(NodeDe(n, n.line)))
            .transpose()
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

struct Entries<'a> {
    entries: std::slice::Iter<'a, (Key, Node)>,
    value: Option<(&'a Node, usize)>,
    /// A struct's fields: other keys are skipped (and collected when it has [`UNKNOWN`]).
    fields: Option<&'static [&'static str]>,
    unknown: Vec<&'a (Key, Node)>,
    unknown_done: bool,
}

enum Pending<'a> {
    Node(&'a Node, usize),
    Unknown(Vec<&'a (Key, Node)>),
}

impl<'a> MapAccess<'a> for Entries<'a> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'a>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        for entry in self.entries.by_ref() {
            let (k, v) = entry;
            if let Some(fields) = self.fields
                && (!fields.contains(&k.name.as_str()) || k.name == UNKNOWN)
            {
                self.unknown.push(entry);
                continue;
            }
            self.value = Some((v, k.line));
            return seed.deserialize(KeyDe(k)).map(Some);
        }
        if !self.unknown_done {
            self.unknown_done = true;
            self.value = None;
            return seed.deserialize(UNKNOWN.into_deserializer()).map(Some);
        }
        Ok(None)
    }

    fn next_value_seed<V: DeserializeSeed<'a>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let pending = match self.value.take() {
            Some((n, line)) => Pending::Node(n, line),
            None => Pending::Unknown(std::mem::take(&mut self.unknown)),
        };
        match pending {
            Pending::Node(n, line) => seed.deserialize(NodeDe(n, line)),
            Pending::Unknown(entries) => seed.deserialize(Rest(entries)),
        }
    }
}

/// The entries a struct doesn't define, for its [`UNKNOWN`] field: a map of them.
struct Rest<'a>(Vec<&'a (Key, Node)>);

impl<'de> Deserializer<'de> for Rest<'de> {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_map(RestEntries {
            entries: self.0.into_iter(),
            value: None,
        })
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

struct RestEntries<'a> {
    entries: std::vec::IntoIter<&'a (Key, Node)>,
    value: Option<&'a (Key, Node)>,
}

impl<'de> MapAccess<'de> for RestEntries<'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        let Some(entry) = self.entries.next() else {
            return Ok(None);
        };
        self.value = Some(entry);
        seed.deserialize(KeyDe(&entry.0)).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (k, v) = self.value.take().expect("a value follows its key");
        seed.deserialize(NodeDe(v, k.line))
    }
}

/// `[A, B]` for [`OneOf`]: the same value, read twice.
struct OneOfSeq<'a> {
    node: &'a Node,
    line: usize,
    at: usize,
}

impl<'a> SeqAccess<'a> for OneOfSeq<'a> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'a>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.at += 1;
        match self.at {
            1 | 2 => seed.deserialize(NodeDe(self.node, self.line)).map(Some),
            _ => Ok(None),
        }
    }
}

/// `[line, column, value]` for [`Located`].
struct LocatedSeq<'a> {
    node: &'a Node,
    line: usize,
    at: usize,
}

impl<'a> SeqAccess<'a> for LocatedSeq<'a> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'a>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.at += 1;
        match self.at {
            1 => seed.deserialize(self.line.into_deserializer()).map(Some),
            2 => seed.deserialize(self.node.column.into_deserializer()).map(Some),
            3 => seed.deserialize(NodeDe(self.node, self.line)).map(Some),
            _ => Ok(None),
        }
    }
}

/// `[value, line, kind]` for [`Loose`]: the line and kind are read only when the value fails.
struct LooseSeq<'a> {
    node: &'a Node,
    line: usize,
    at: usize,
}

impl<'a> SeqAccess<'a> for LooseSeq<'a> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'a>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.at += 1;
        match self.at {
            1 => seed.deserialize(NodeDe(self.node, self.line)).map(Some),
            2 => seed.deserialize(self.line.into_deserializer()).map(Some),
            3 => seed
                .deserialize(self.node.kind.name().into_deserializer())
                .map(Some),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;
    use crate::config::node::parse;

    #[derive(Debug, Deserialize)]
    struct Profile {
        target: Option<Located<Loose<String>>>,
        targets: Option<Loose<BTreeMap<String, Located<serde_json::Value>>>>,
        #[serde(rename = "$unknown", default)]
        unknown: UnknownKeys,
    }

    #[test]
    fn values_keep_their_lines_and_unknown_keys_are_collected() {
        let n = parse("target: prod\nwat: 1\ntargets:\n  dev: {type: duckdb}\nalso: 2\n").unwrap();
        let p: Profile = from_node(&n).unwrap();
        let t = p.target.unwrap();
        assert_eq!((t.value.ok().map(String::as_str), t.line), (Some("prod"), 1));
        let targets = p.targets.unwrap();
        let dev = &targets.ok().unwrap()["dev"];
        assert_eq!(dev.line, 4);
        assert_eq!(dev.value["type"], "duckdb");
        let unknown: Vec<_> = p.unknown.0.iter().map(|k| (k.name.as_str(), k.line)).collect();
        assert_eq!(unknown, [("wat", 2), ("also", 5)]);
    }

    #[test]
    fn a_wrong_shape_is_kept_with_its_line_and_the_rest_still_reads() {
        let n = parse("target: [a]\ntargets: x\n").unwrap();
        let p: Profile = from_node(&n).unwrap();
        let Loose::Bad(f) = p.target.unwrap().value else {
            panic!()
        };
        assert_eq!((f.kind, f.line), ("a list", 1));
        let Loose::Bad(f) = p.targets.unwrap() else {
            panic!()
        };
        assert_eq!((f.kind, f.line), ("a string", 2));
    }

    #[test]
    fn the_unknown_field_can_read_the_other_keys_values() {
        #[derive(Debug, Deserialize)]
        struct Folder {
            #[serde(rename = "+tags", default)]
            tags: Vec<String>,
            #[serde(rename = "$unknown", default)]
            folders: Map<Folder>,
        }
        let n = parse("+tags: [a]\nsales:\n  +tags: [b]\n  eu: {}\n").unwrap();
        let f: Folder = from_node(&n).unwrap();
        let (name, sales) = f.folders.iter().next().unwrap();
        assert_eq!(
            (name.value.as_str(), name.line, &sales.tags[..]),
            ("sales", 2, &["b".to_string()][..])
        );
        assert_eq!(sales.folders.iter().next().unwrap().0.value, "eu");
        assert_eq!(f.tags, ["a"]);
    }

    #[test]
    fn one_of_reads_either_shape_and_keeps_lines() {
        #[derive(Debug, Deserialize)]
        struct Entry {
            query: Located<String>,
            #[serde(rename = "$unknown", default)]
            unknown: UnknownKeys,
        }
        let n = parse("- a\n- {query: b, x: 1}\n- [c]\n").unwrap();
        let items: Vec<Loose<OneOf<String, Entry>>> = from_node(&n).unwrap();
        assert!(
            matches!(&items[0], Loose::Ok(OneOf::A(s)) if s == "a"),
            "{:?}",
            items[0]
        );
        let Loose::Ok(OneOf::B(e)) = &items[1] else {
            panic!()
        };
        assert_eq!(
            (e.query.value.as_str(), e.query.line, e.unknown.0[0].name.as_str()),
            ("b", 2, "x")
        );
        assert!(matches!(&items[2], Loose::Bad(f) if f.kind == "a list" && f.line == 3));
    }

    #[test]
    fn maps_keep_the_file_order_and_each_keys_line() {
        let n = parse("b: 1\na: 2\n").unwrap();
        let m: Map<u32> = from_node(&n).unwrap();
        let got: Vec<_> = m.iter().map(|(k, v)| (k.value.as_str(), k.line, *v)).collect();
        assert_eq!(got, [("b", 1, 1), ("a", 2, 2)]);
    }

    #[test]
    fn errors_outside_loose_values_have_a_line() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct S {
            n: u32,
        }
        let n = parse("n: lots\n").unwrap();
        let e = from_node::<S>(&n).unwrap_err();
        assert_eq!(e.line, Some(1));
    }

    #[test]
    fn a_struct_without_the_unknown_field_ignores_other_keys() {
        #[derive(Debug, Deserialize)]
        struct S {
            a: u32,
        }
        let n = parse("a: 1\nb: 2\n").unwrap();
        assert_eq!(from_node::<S>(&n).unwrap().a, 1);
    }
}
