//! The shape of dbt-style source declarations: a top-level `sources:` in any project YAML file.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::de::{Located, Loose, Map, OneOf, Text, UnknownKeys};

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// dbt-style source declarations: the tables a project reads, with DRE's `profile:` for the connection they live on. In any project YAML file under a top-level `sources:` key (`sources/` is the conventional folder); dbt's `version: 2` is accepted beside it. Use a table in SQL with `{{ source('<source>', '<table>') }}`.
#[derive(Deserialize, JsonSchema)]
#[schemars(title = "DRE sources", deny_unknown_fields)]
pub struct SourcesFile {
    /// The sources this file declares.
    #[schemars(required)]
    pub sources: Option<Located<Loose<Option<Vec<SourceItem>>>>>,
    /// dbt's file version (`2`); accepted and ignored.
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub version: Option<IgnoredAny>,
}

/// One source of `sources:`.
pub type SourceItem = Located<Loose<Source>>;
/// One table of a source's `tables:`.
pub type TableItem = Located<Loose<Table>>;

/// A dbt key DRE accepts but doesn't use yet.
type Dbt = Option<IgnoredAny>;

/// A source: tables in one schema of one system.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "source", deny_unknown_fields)]
pub struct Source {
    /// The source's name, the first argument of `source()`. Unique in the project.
    #[schemars(required, with = "String")]
    pub name: Option<Loose<String>>,
    /// What the source is.
    pub description: Option<Loose<Text>>,
    /// The database (catalog). Set: `source()` renders `database.schema.table`; unset: `schema.table`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub database: Option<Loose<Text>>,
    /// The schema; the source's name unless set. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub schema: Option<Loose<Text>>,
    /// DRE's addition: the connection (in `profiles.yml`) the source lives on. A query using the source runs there; it overrides the report's, Set's, folder's and project's default. Unset: the source runs wherever its query runs. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Loose<Text>>,
    pub quoting: Option<Loose<Quoting>>,
    pub tags: Option<Loose<Tags>>,
    pub meta: Option<Loose<Meta>>,
    /// The source's tables.
    pub tables: Option<Loose<Option<Vec<TableItem>>>>,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub loader: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub loaded_at_field: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub loaded_at_query: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub config: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub overrides: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub freshness: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub docs: Dbt,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// One table of the source.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "table", deny_unknown_fields)]
pub struct Table {
    /// The name `source()` uses.
    #[schemars(required, with = "String")]
    pub name: Option<Loose<String>>,
    /// The real table name, when it differs from `name`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub identifier: Option<Loose<Text>>,
    /// What the table holds.
    pub description: Option<Loose<Text>>,
    pub quoting: Option<Loose<Quoting>>,
    pub tags: Option<Loose<Tags>>,
    pub meta: Option<Loose<Meta>>,
    /// Declared columns, checked by `dre validate --live`.
    pub columns: Option<Loose<Option<Vec<Loose<Column>>>>>,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub loaded_at_field: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub loaded_at_query: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub tests: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub data_tests: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub freshness: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub external: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub config: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub docs: Dbt,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// A column of the table. `dre validate --live` checks that it exists and, with `data_type`, that its type matches loosely.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "column", deny_unknown_fields)]
pub struct Column {
    /// The column's name (compared case-insensitively).
    #[schemars(required, with = "String")]
    pub name: Option<Loose<String>>,
    /// What the column holds.
    pub description: Option<Loose<Text>>,
    /// The column's SQL type, e.g. `bigint`, `varchar`, `timestamp`; compared loosely with what the database returns.
    pub data_type: Option<Loose<Text>>,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub meta: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub tags: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub quote: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub tests: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub data_tests: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub constraints: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub config: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub docs: Dbt,
    /// A dbt key DRE accepts but doesn't use yet (noted by `dre validate`).
    #[serde(default)]
    #[schemars(schema_with = "anything")]
    pub granularity: Dbt,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// Which parts `source()` quotes, with the connection's identifier quote character. Unset parts inherit (table from source), else aren't quoted.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "quoting", deny_unknown_fields)]
pub struct Quoting {
    /// Quote the database.
    #[serde(default)]
    #[schemars(with = "bool")]
    pub database: Option<Loose<bool>>,
    /// Quote the schema.
    #[serde(default)]
    #[schemars(with = "bool")]
    pub schema: Option<Loose<bool>>,
    /// Quote the table name.
    #[serde(default)]
    #[schemars(with = "bool")]
    pub identifier: Option<Loose<bool>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: Map<serde_json::Value>,
}

/// Tags, for documentation.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(rename = "tags")]
pub struct Tags(pub OneOf<String, Vec<String>>);

/// Free-form metadata, kept in the manifest.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(rename = "meta")]
pub struct Meta(#[schemars(schema_with = "any_map")] pub JsonMap);

fn anything(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({})
}

fn any_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": true})
}

impl Source {
    /// The dbt keys it sets that DRE doesn't use yet.
    pub fn dbt_keys(&self) -> Vec<&'static str> {
        [
            ("loader", &self.loader),
            ("loaded_at_field", &self.loaded_at_field),
            ("loaded_at_query", &self.loaded_at_query),
            ("config", &self.config),
            ("overrides", &self.overrides),
            ("freshness", &self.freshness),
            ("docs", &self.docs),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.is_some().then_some(k))
        .collect()
    }
}

impl Table {
    /// The dbt keys it sets that DRE doesn't use yet.
    pub fn dbt_keys(&self) -> Vec<&'static str> {
        [
            ("loaded_at_field", &self.loaded_at_field),
            ("loaded_at_query", &self.loaded_at_query),
            ("tests", &self.tests),
            ("data_tests", &self.data_tests),
            ("freshness", &self.freshness),
            ("external", &self.external),
            ("config", &self.config),
            ("docs", &self.docs),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.is_some().then_some(k))
        .collect()
    }
}

impl Column {
    /// The dbt keys it sets that DRE doesn't use yet.
    pub fn dbt_keys(&self) -> Vec<&'static str> {
        [
            ("meta", &self.meta),
            ("tags", &self.tags),
            ("quote", &self.quote),
            ("tests", &self.tests),
            ("data_tests", &self.data_tests),
            ("constraints", &self.constraints),
            ("config", &self.config),
            ("docs", &self.docs),
            ("granularity", &self.granularity),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.is_some().then_some(k))
        .collect()
    }
}
