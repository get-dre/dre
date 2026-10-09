//! The shape of a lookup's config, `lookups/<name>.yml`.

use schemars::JsonSchema;
use serde::Deserialize;

use super::de::{Located, Loose, Map, UnknownKeys};

/// The optional config of a lookup, `lookups/<name>.yml` next to a csv, xlsx, xls, json or jsonl file. (A `.yml` lookup that holds the rows themselves can also set `rows`.)
#[derive(Deserialize, JsonSchema)]
#[schemars(title = "DRE lookup config", deny_unknown_fields)]
pub struct LookupFile {
    /// Types for columns; without it every value is text.
    pub columns: Option<Located<Loose<Map<Loose<ColumnType>>>>>,
    /// For xlsx and xls lookups: the sheet to read. Default: the first.
    pub sheet: Option<Located<Loose<String>>>,
    /// How the lookup reaches the database: inlined into the SQL, loaded into a temp table, or chosen by size (`auto`).
    #[schemars(extend("default" = "auto"))]
    pub load: Option<Located<Loose<LoadName>>>,
    /// For a `.yml` lookup that holds its own data: the rows.
    #[serde(default)]
    #[schemars(schema_with = "rows")]
    pub rows: Option<Located<serde_json::Value>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// One of `string`, `integer`, `number`, `boolean`, `date`.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum ColumnType {
    String,
    Integer,
    Number,
    Boolean,
    Date,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum LoadName {
    Auto,
    Inline,
    TempTable,
}

fn rows(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "array", "items": {"type": "object"}})
}
