//! The shape of a report YAML file under `reports/` (one fragment of a report: a report may be
//! split over several files, merged by name).
//!
//! Values the loader checks itself, to say what's wrong in DRE's words, are [`Loose`]. Outputs and
//! Sets are read where they're used.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::de::{Located, Loose, OneOf, UnknownKeys};

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// One `queries:` item: a query's name, or a query with settings.
pub type QueryItem = Located<Loose<OneOf<String, QueryEntry>>>;

/// A report: SQL queries plus an output, in a YAML file under `reports/`.
#[derive(Default, Deserialize, JsonSchema)]
#[schemars(title = "DRE report", deny_unknown_fields)]
pub struct ReportFile {
    /// The report's name. Default: the name of the folder the YAML file is in. Report names are unique across the project.
    #[schemars(length(min = 1))]
    pub name: Option<Located<Loose<String>>>,
    /// Tags to select the report with, `-s tag:<tag>`.
    pub tags: Option<Located<Loose<Vec<String>>>>,
    /// The `.sql` files whose results make the report's tabs, in the order they run, on one database session.
    pub queries: Option<Located<Loose<Vec<QueryItem>>>>,
    /// The connection (in `profiles.yml`) the queries run on, unless a query's own `profile:` or a source's says otherwise. Default: the folder's `+profile`, then `default_profile`. Can't be combined with `sets`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Located<Loose<String>>>,
    /// The Set a plain `dre run` uses. Must be one of `sets`.
    pub default_set: Option<Located<Loose<String>>>,
    /// Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps.
    pub vars: Option<Located<Loose<JsonMap>>>,
    /// The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC.
    pub timezone: Option<Located<Loose<String>>>,
    /// The locale this report's number filters format for (`de-DE`). Default: the folder's `+locale`, then the project's `locale`, then `en`.
    pub locale: Option<Located<Loose<String>>>,
    /// The Sets the report can run as, by name (declared in `sets.yml`) or declared here.
    pub sets: Option<Located<Loose<Vec<SetItem>>>>,
    /// Removed: schedules live in schedules.yml. Read only to say so.
    #[schemars(skip)]
    pub schedule: Option<Located<IgnoredAny>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// A query with settings, instead of just its name.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "queryEntry", deny_unknown_fields)]
pub struct QueryEntry {
    /// The name of a `.sql` file under `reports/`, without folder or extension.
    #[schemars(required)]
    pub query: Option<Loose<String>>,
    /// The connection this query runs on, over the report's, Set's, folder's and project's. Must agree with the `profile` of any source it uses. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Loose<String>>,
    /// `false` runs the file only for what it does (temp tables, `SET`s) and discards any result, so it gets no tab.
    #[schemars(extend("default" = true))]
    pub tab: Option<Loose<bool>>,
    /// The tab (sheet) name. Default: the file's name. Not allowed with `tab: false`.
    pub tab_name: Option<Loose<String>>,
    /// Where the data starts on the sheet (xlsx only). Default: `A1`.
    #[schemars(regex(pattern = "^[A-Za-z]{1,3}[0-9]+$"))]
    pub anchor: Option<Loose<String>>,
    /// Whether to write the column names as the first row (xlsx only). Default: the output's `header`.
    pub header: Option<Loose<bool>>,
    /// Per-column settings for this tab, by column name (xlsx only).
    #[serde(default)]
    #[schemars(schema_with = "columns")]
    pub columns: Option<serde_json::Value>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

fn columns(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "additionalProperties": {
            "type": "object",
            "description": "Settings for one column of an xlsx tab.",
            "properties": {
                "format": {"type": "string", "description": "The Excel number format of the column, e.g. `#,##0.00` or `dd/mm/yyyy`. See the xlsx column formats in the plugins reference."},
                "formula": {"type": "string", "description": "An Excel formula for each row of this column; `{name}` stands for that column's cell on the same row, e.g. `=ROUND({qty}*{unit_price},2)`. The SQL selects a placeholder column where the formula goes."},
                "total": {"type": "string", "description": "Puts a total under the column: one of `sum`, `count`, `average`, `min`, `max`, or a formula such as `=SUM({net:*})`."}
            },
            "additionalProperties": false
        },
        "propertyNames": {"minLength": 1}
    })
}

/// One `sets:` item: a Set's name (declared in `sets.yml`), or a Set declared here.
pub type SetItem = Located<Loose<OneOf<String, SetEntry>>>;

/// A Set declared in the report: a named variant of it.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "setEntry", deny_unknown_fields)]
pub struct SetEntry {
    /// The Set's name. A report can also name Sets declared in `sets.yml`.
    #[schemars(required)]
    pub name: Option<Loose<String>>,
    /// The connection this Set runs on. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Located<Loose<String>>>,
    /// Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps.
    #[serde(default)]
    #[schemars(schema_with = "any_map")]
    pub vars: Option<Loose<JsonMap>>,
    /// Queries to leave out of this Set, by name.
    pub exclude: Option<Located<Loose<Vec<String>>>>,
    /// Replaces the report's queries for this Set.
    #[serde(default)]
    #[schemars(schema_with = "set_queries")]
    pub queries: Option<Located<Loose<Vec<SetQueryItem>>>>,
    /// Renames tabs for this Set: query name to tab name.
    #[serde(default)]
    #[schemars(schema_with = "tab_names")]
    pub tab_names: Option<Loose<JsonMap>>,
    /// Output settings for this Set: a map changes the inherited output (with several, the one its `name:` names); a list replaces them all.
    #[serde(default)]
    #[schemars(schema_with = "output_or_list")]
    pub output: Option<serde_json::Value>,
    /// The locale for this Set's number filters (`fr-FR`), above the report's.
    pub locale: Option<Loose<String>>,
    /// Removed: schedules live in schedules.yml. Read only to say so.
    #[schemars(skip)]
    pub schedule: Option<IgnoredAny>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// One item of a Set's `queries:`: a query's name, or a query with settings.
pub type SetQueryItem = Loose<OneOf<String, SetQuery>>;

/// A query of a Set with its own settings.
#[derive(Deserialize)]
pub struct SetQuery {
    pub query: Option<Loose<String>>,
    pub tab: Option<Loose<bool>>,
    pub tab_name: Option<Loose<String>>,
}

fn any_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": true})
}

fn tab_names(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": {"type": "string"}})
}

fn output_or_list(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "oneOf": [
            {"$ref": "#/$defs/output"},
            {"type": "array", "minItems": 1, "items": {"$ref": "#/$defs/output"}}
        ]
    })
}

fn set_queries(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "description": "A query of this Set, by name, or with its own settings.",
            "oneOf": [
                {"type": "string"},
                {
                    "type": "object",
                    "description": "A query with settings.",
                    "properties": {
                        "query": {"type": "string", "description": "The query's name."},
                        "tab": {"type": "boolean", "description": "Whether the query makes a tab."},
                        "tab_name": {"type": "string", "description": "The tab name."}
                    },
                    "required": ["query"],
                    "additionalProperties": true
                }
            ]
        }
    })
}
