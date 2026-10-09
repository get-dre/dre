//! The shape of `dre_project.yml` and its folder config (`reports:`).
//!
//! These types are what's read, and they generate `project.schema.json`; their doc comments are
//! its descriptions. Values the loader checks itself, to say what's wrong in DRE's words, are
//! [`Loose`]. Outputs (`default_output`, `+output`) and the sections other files share (`plugins`,
//! `sources`) are read where they're used.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::de::{Located, Loose, Map, UnknownKeys};
use crate::dates::{WeekNumbering, WeekStart};

type JsonMap = serde_json::Map<String, serde_json::Value>;
/// `format_options:`: options by format name.
pub type FormatOptions = Located<Loose<Option<Map<Loose<JsonMap>>>>>;
/// `dispatch:`: macro namespaces and their search order.
pub type DispatchList = Located<Loose<Vec<Loose<Dispatch>>>>;

/// The project file, `dre_project.yml`, at the project root.
#[derive(Deserialize, JsonSchema)]
#[schemars(title = "DRE project", deny_unknown_fields)]
pub struct ProjectFile {
    /// The project's name. Required.
    #[schemars(required, length(min = 1))]
    pub name: Option<Located<Loose<String>>>,
    /// The connection (in `profiles.yml`) reports use when nothing else names one. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub default_profile: Option<Located<Loose<String>>>,
    /// The output every report starts from (the built-in default is `format: csv`). A report's own `output` is merged on top.
    #[schemars(schema_with = "output_ref")]
    #[serde(default)]
    pub default_output: Option<IgnoredAny>,
    /// Default options per output format, under every output of that format. A report's own keys win.
    #[schemars(schema_with = "format_options")]
    #[serde(default)]
    pub format_options: Option<FormatOptions>,
    /// The Set used when a report has Sets and none is chosen.
    pub default_set: Option<Located<Loose<String>>>,
    /// The lowest level of `var()`: folders, reports, Sets, schedules and `--var` override these.
    #[schemars(schema_with = "any_map")]
    #[serde(default)]
    pub vars: Option<Located<Loose<Option<JsonMap>>>>,
    /// `run_query()` refuses results larger than this.
    #[schemars(range(min = 1), extend("default" = 10000))]
    pub run_query_max_rows: Option<Located<Loose<u64>>>,
    /// Lookups over this many rows are loaded into a temp table instead of inlined into the SQL.
    #[schemars(range(min = 0), extend("default" = 200))]
    pub lookup_inline_max_rows: Option<Located<Loose<u64>>>,
    /// Which macro packages `dispatch()` searches, and in what order.
    pub dispatch: Option<DispatchList>,
    /// Whether `DRE_SECRET_*` values are masked as `*****` in the console, logs, JSON events, `run_results.json` and `target/compiled/`.
    #[schemars(extend("default" = true))]
    pub mask_secrets: Option<Located<Loose<bool>>>,
    /// The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC.
    pub timezone: Option<Located<Loose<String>>>,
    /// The locale the number filters (`number`, `percent`, `currency`, `compact`, `signed`) format for, such as `de-DE` or `fr`: decimal and group separators, and where the currency symbol and percent sign go. Default: `en`.
    pub locale: Option<Located<Loose<String>>>,
    /// The first day of the week for `run.date` week arithmetic.
    #[schemars(extend("default" = "monday"))]
    pub week_start: Option<Located<Loose<WeekStart>>>,
    /// How weeks are numbered: ISO 8601 or US style.
    #[schemars(extend("default" = "iso"))]
    pub week_numbering: Option<Located<Loose<WeekNumbering>>>,
    /// Folder config: settings for the report folders, by folder name, nested to match the folders under `reports/`.
    pub reports: Option<Located<Loose<Folder>>>,
    /// Where DRE writes its generated files (compiled SQL, run outputs, the manifest). Default: `target/` in the project. `--target-path` and `DRE_TARGET_PATH` override it.
    pub target_path: Option<Located<Loose<String>>>,
    /// The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place.
    #[schemars(schema_with = "plugins")]
    #[serde(default)]
    pub plugins: Option<IgnoredAny>,
    /// dbt-style source declarations (see the sources schema). May be written in any project YAML file.
    #[schemars(schema_with = "sources_ref")]
    #[serde(default)]
    pub sources: Option<IgnoredAny>,
    /// Removed in DRE 0.2.1; read only to say so.
    #[schemars(skip)]
    pub target: Option<Located<IgnoredAny>>,
    /// Where plugins were declared before packages; read only to say so.
    #[schemars(skip)]
    pub destinations: Option<Located<IgnoredAny>>,
    #[schemars(skip)]
    pub formats: Option<Located<IgnoredAny>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// One macro namespace.
#[derive(Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields, inline)]
pub struct Dispatch {
    /// The macro package the dispatching macro belongs to, e.g. `dre_utils`.
    #[schemars(required)]
    pub macro_namespace: Option<Loose<String>>,
    /// Packages searched for the variant, first match wins: this project's name and installed packages.
    #[schemars(required, length(min = 1))]
    pub search_order: Option<Loose<Vec<Loose<String>>>>,
}

/// Settings for every report in this folder and the folders below it. Keys starting with `+` are settings; any other key is a subfolder.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "folder", extend("additionalProperties" = {"$ref": "#/$defs/folder"}))]
pub struct Folder {
    /// Tags added to every report in the folder.
    #[serde(rename = "+tags")]
    pub tags: Option<Located<Loose<Vec<String>>>>,
    /// Output settings every report in the folder starts from; a report's own keys win.
    #[serde(rename = "+output")]
    #[serde(default)]
    #[schemars(schema_with = "output_ref")]
    pub output: Option<Located<Loose<JsonMap>>>,
    /// The connection for reports in the folder. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    #[serde(rename = "+profile")]
    pub profile: Option<Located<Loose<String>>>,
    /// Removed: schedules live in schedules.yml. Read only to say so.
    #[serde(rename = "+schedule")]
    #[schemars(skip)]
    pub schedule: Option<Located<IgnoredAny>>,
    /// Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps.
    #[serde(rename = "+vars")]
    #[serde(default)]
    #[schemars(schema_with = "any_map")]
    pub vars: Option<Located<Loose<JsonMap>>>,
    /// The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC.
    #[serde(rename = "+timezone")]
    pub timezone: Option<Located<Loose<String>>>,
    /// The `locale:` for reports in this folder; a report's own wins. See `locale` in dre_project.yml.
    #[serde(rename = "+locale")]
    pub locale: Option<Located<Loose<String>>>,
    /// Subfolders by name, and `+` keys that aren't settings.
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub rest: Map<Located<Loose<Folder>>>,
}

impl Folder {
    /// Whether it sets anything itself (any `+` key, known or not), rather than only holding
    /// subfolders.
    pub fn sets_anything(&self) -> bool {
        self.tags.is_some()
            || self.output.is_some()
            || self.profile.is_some()
            || self.schedule.is_some()
            || self.vars.is_some()
            || self.timezone.is_some()
            || self.locale.is_some()
            || self.rest.iter().any(|(k, _)| k.value.starts_with('+'))
    }
}

fn output_ref(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"$ref": "report.schema.json#/$defs/output"})
}

fn sources_ref(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"$ref": "sources.schema.json#/properties/sources"})
}

fn any_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": true})
}

fn format_options(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": {"type": "object"}})
}

fn plugins(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "oneOf": [
            {
                "type": "array",
                "items": {
                    "description": "One plugin package: just its name, a `name: \"<version>\"` pair, or a map with `name` and where to get it.",
                    "oneOf": [
                        {"type": "string", "description": "A package name, e.g. `duckdb`. Any version."},
                        {
                            "type": "object",
                            "description": "A package name mapped to a version constraint, e.g. `duckdb: \">=1.0\"`.",
                            "additionalProperties": true,
                            "minProperties": 1,
                            "maxProperties": 1
                        },
                        {
                            "type": "object",
                            "description": "A package with its source.",
                            "properties": {
                                "name": {"type": "string", "description": "The package name: lowercase letters, digits and `_`."},
                                "version": {"type": "string", "description": "A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`."},
                                "github": {"type": "string", "description": "Install from the releases of this GitHub repository, `owner/repo`."},
                                "local": {"type": "string", "description": "Use the package folder at this path as it is."},
                                "registry": {"type": "string", "description": "Install from this registry index (a URL or a path) instead of the default one."}
                            },
                            "required": ["name"],
                            "additionalProperties": false
                        }
                    ]
                }
            },
            {"type": "object", "description": "Package names mapped to version constraints.", "additionalProperties": true},
            {"type": "null"}
        ],
        "x-doc-type": "list of plugin packages: a name, `name: \"<version>\"`, or a map (see below)"
    })
}
