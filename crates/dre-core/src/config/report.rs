//! The shape of a report YAML file under `reports/` (one fragment of a report: a report may be
//! split over several files, merged by name).
//!
//! Values the loader checks itself, to say what's wrong in DRE's words, are [`Loose`]. Outputs and
//! Sets are read where they're used.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::de::{Located, Loose, Map, Maybe, OneOf, UnknownKeys};

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
    #[serde(default)]
    #[schemars(schema_with = "queries")]
    pub queries: Option<Located<Loose<Vec<QueryItem>>>>,
    /// One output, or a list of outputs formatted from the same run of the queries (file outputs are delivered before messages). A list replaces the inherited output; a map changes it.
    #[serde(default)]
    #[schemars(schema_with = "output_or_list")]
    pub output: Option<IgnoredAny>,
    /// The connection (in `profiles.yml`) the queries run on, unless a query's own `profile:` or a source's says otherwise. Default: the folder's `+profile`, then `default_profile`. Can't be combined with `sets`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Located<Loose<String>>>,
    /// The Sets the report can run as, by name (declared in `sets.yml`) or declared here.
    pub sets: Option<Located<Loose<Vec<SetItem>>>>,
    /// The Set a plain `dre run` uses. Must be one of `sets`.
    pub default_set: Option<Located<Loose<String>>>,
    /// Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps.
    pub vars: Option<Located<Loose<JsonMap>>>,
    /// The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC.
    pub timezone: Option<Located<Loose<String>>>,
    /// The locale this report's number filters format for (`de-DE`). Default: the folder's `+locale`, then the project's `locale`, then `en`.
    pub locale: Option<Located<Loose<String>>>,
    /// The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place.
    #[serde(default)]
    #[schemars(schema_with = "super::project::plugins")]
    pub plugins: Option<IgnoredAny>,
    /// dbt-style source declarations (see the sources schema). May be written in any project YAML file.
    #[serde(default)]
    #[schemars(schema_with = "super::project::sources_ref")]
    pub sources: Option<IgnoredAny>,
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

fn output_or_list(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let output = g.subschema_for::<Output>();
    schemars::json_schema!({
        "oneOf": [output, {"type": "array", "minItems": 1, "items": output}]
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

/// How a report's result is written and where it goes. Besides the keys below, each format takes its own options (for example `delimiter` for `delimited`, `columns` for `fixed_width`, `text` for `message`); they are documented with the formats.
// Outputs are layered (the built-in default, `default_output`, each folder's `+output`, the
// report's and a Set's `output`) and read once merged.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "output", extend("additionalProperties" = true))]
pub struct Output {
    /// Names the output so other outputs and Set overrides can refer to it (`outputs.<name>` in a message, `attach:`). Also the default file name (`<name>.<ext>`). Unique within the report.
    #[schemars(regex(pattern = "^[A-Za-z_][A-Za-z0-9_]*$"))]
    pub name: Option<Loose<String>>,
    /// The output format: `message` (built in: a short headline rendered from the results), or `csv`, `delimited`, `fixed_width`, `parquet` or `xlsx` (each a plugin). Default: `csv`, or the project's `default_output`.
    pub format: Option<Loose<String>>,
    /// Which of the report's queries this output formats. Default: all of them. Each query runs once, whatever the number of outputs.
    #[schemars(length(min = 1))]
    pub queries: Option<Loose<Vec<String>>>,
    /// A Jinja expression over the results (`results.<query>.value < 0`); when it's false, the output is skipped and recorded as `skipped`.
    #[serde(default)]
    #[schemars(schema_with = "string_or_bool")]
    pub when: Option<Loose<OneOf<String, bool>>>,
    /// Where to deliver the file: one destination, or a list to deliver to several in one run. Default: the file stays in the target path.
    #[serde(default)]
    #[schemars(schema_with = "destination_or_list")]
    pub destination: Option<Loose<OneOf<Destination, Vec<Loose<Destination>>>>>,
    pub template: Option<Loose<Template>>,
    /// File extension for text formats (e.g. `aba`), instead of the format's own. `""` or `false` means none. Not for xlsx.
    #[serde(default)]
    #[schemars(schema_with = "extension")]
    pub extension: Maybe<Loose<Option<OneOf<String, bool>>>>,
    /// The format's own options.
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub options: Map<serde_json::Value>,
}

/// Where a file is delivered: the name of a destination profile in `profiles.yml`, an optional `path`, and the options of that destination's plugin (e.g. `to`, `subject` and `body` for email). The built-in profile `local` copies the file to a local path.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "destination", extend("additionalProperties" = true))]
pub struct Destination {
    /// The destination profile in `profiles.yml` (under `destinations:`) to deliver with. `local` is built in. It uses its entry for the run (`--target`, `DRE_TARGET`, else the profile's own `target:`, else `dev`); a missing entry is an error, and an entry `{deliver: false}` delivers nowhere. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    #[schemars(required)]
    pub profile: Option<Loose<String>>,
    /// Where to put the file: a path, or a URL such as `s3://bucket/key`, depending on the destination. Rendered with Jinja, so it can use `var()`, `run.*`, macros and `destination.*` (this destination's settings).
    pub path: Option<Loose<String>>,
    /// On a message output's entry, for a destination that takes messages and files (`slack`, `email`): other outputs of the report whose files go with the message.
    #[serde(default)]
    #[schemars(schema_with = "attach")]
    pub attach: Option<Loose<OneOf<String, Vec<String>>>>,
    /// The destination plugin's options.
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub options: Map<serde_json::Value>,
}

/// Fills a branded Excel workbook instead of creating a new one (xlsx only).
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "template", deny_unknown_fields)]
pub struct Template {
    /// Path of the `.xlsx` template, relative to the project.
    #[schemars(required)]
    pub file: Option<Loose<String>>,
    /// Where each query's data goes in the template. Default: none, so the template is copied as it is.
    pub bindings: Option<Loose<Vec<Loose<TemplateBinding>>>>,
}

/// One block of data in an xlsx template: a table of a query's result, or a single cell.
#[derive(Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields, inline)]
pub struct TemplateBinding {
    /// The template sheet to write into.
    #[schemars(required)]
    pub sheet: Option<Loose<String>>,
    /// The query whose result goes here. Must be one of the Binding's queries.
    pub query: Option<Loose<String>>,
    /// Which result set of the query to use, counting from 1. Default: the last.
    #[schemars(range(min = 1))]
    pub result_index: Option<Loose<u64>>,
    /// The top-left cell of a table block. A table block needs a `query`.
    #[schemars(regex(pattern = "^[A-Za-z]{1,3}[0-9]+$"))]
    pub anchor: Option<Loose<String>>,
    /// Whether to write the column names above the data in a table block.
    pub header: Option<Loose<bool>>,
    /// The columns of the query to write in a table block, in order. Default: all.
    pub columns: Option<Loose<Vec<String>>>,
    /// Makes this a single-cell binding: the cell to write. Needs exactly one of `value`, or `query` with `column`.
    #[schemars(regex(pattern = "^[A-Za-z]{1,3}[0-9]+$"))]
    pub cell: Option<Loose<String>>,
    /// A fixed value (rendered with Jinja) for a single-cell binding.
    pub value: Option<Loose<String>>,
    /// The column of the query's first row to write into a single-cell binding.
    pub column: Option<Loose<String>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

fn string_or_bool(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": ["string", "boolean"]})
}

fn extension(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // `null` is a value here (no extension), not schemars' "optional".
    schemars::json_schema!({"type": ["string", "boolean", "null"], "x-dre-null": true})
}

fn destination_or_list(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let destination = g.subschema_for::<Destination>();
    schemars::json_schema!({
        "oneOf": [destination, {"type": "array", "minItems": 1, "items": destination}]
    })
}

fn attach(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "oneOf": [
            {"type": "string"},
            {"type": "array", "items": {"type": "string"}, "minItems": 1}
        ]
    })
}

fn queries(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let entry = g.subschema_for::<QueryEntry>();
    schemars::json_schema!({
        "type": "array",
        "items": {
            "oneOf": [
                {"type": "string", "description": "The name of a `.sql` file under `reports/`, without extension."},
                entry
            ]
        }
    })
}
