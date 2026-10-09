//! Loading a DRE project directory into a resolved, validated project.
//!
//! Every YAML file in the project is parsed and classified by its shape, not its name. Report
//! config merges by report name from anywhere, then resolves with one precedence rule:
//! Binding > report YAML > folder config (deepest wins) > project defaults > built-in defaults.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json};
use serde_yaml_ng::{Mapping, Value};

use crate::dates::{WeekNumbering, WeekStart};
use crate::diag::Diagnostics;
use crate::lookups::{self, DEFAULT_INLINE_MAX_ROWS, LOOKUPS_DIR, Lookup};
use crate::packages::{self, DispatchOrder, Package};

/// Names DRE puts in every Jinja context; a package can't take one.
const RESERVED_NAMES: &[&str] = &[
    "run",
    "var",
    "env_var",
    "run_query",
    "columns",
    "ref",
    "lookup",
    "dispatch",
    "target",
    "profile",
    "date",
    "datetime",
    "date_range",
    "month_of",
    "quarter_of",
    "year_of",
    "week_of",
    "period",
    "raise_error",
    "source",
    "connection",
    "destination",
];
use crate::config;
use crate::config::de::{self, Loose};
use crate::config::project::ProjectFile;
use crate::config::report::{Output as OutputConfig, QueryItem, ReportFile, SetEntry, SetItem};
use crate::profiles::{LOCAL_TYPE, Profiles, Role};
use crate::yaml::YamlFile;
use crate::{constraints, options, preflight, schedule, selector, sqlsplit};

pub const PROJECT_FILE: &str = "dre_project.yml";
pub const REPORTS_DIR: &str = "reports";
pub const MACROS_DIR: &str = "macros";
pub const TARGET_DIR: &str = "target";
pub const LOGS_DIR: &str = "logs";
pub use crate::plugins::DEPS_DIR;
pub const DEFAULT_RUN_QUERY_MAX_ROWS: u64 = 10_000;

pub const REPORT_KEYS: &[&str] = &[
    "name",
    "tags",
    "queries",
    "output",
    "profile",
    "sets",
    "default_set",
    "schedule",
    "vars",
    "timezone",
    "locale",
];
/// Declares the project's plugin packages, in any project YAML file.
const PLUGINS_KEY: &str = "plugins";
pub const PLUGIN_KEYS: &[&str] = &[PLUGINS_KEY];
/// Where plugins were declared before packages; now an error pointing at `plugins:`. (`sources:`
/// is the dbt-style table declarations now; a list of plugin names there gets the same hint.)
const OLD_PLUGIN_KEYS: &[&str] = &["destinations", "formats"];
/// dbt-style source declarations, in any project YAML file.
pub const SOURCES_KEY: &str = "sources";
pub const PROJECT_KEYS: &[&str] = &[
    "name",
    "default_profile",
    "default_output",
    "format_options",
    "default_set",
    "vars",
    "run_query_max_rows",
    "lookup_inline_max_rows",
    "dispatch",
    "mask_secrets",
    "timezone",
    "locale",
    "week_start",
    "week_numbering",
    "reports",
    SOURCES_KEY,
    crate::target::KEY,
];
/// Keys of a schedules.yml entry besides its timing (`schedule::SCHEDULE_KEYS`).
pub const SCHEDULE_ENTRY_KEYS: &[&str] = &[
    "name", "select", "report", "set", "vars", "timezone", "enabled", "timing",
];
/// The timings file's name. Other YAML files of timings are recognised by their shape.
pub const TIMINGS_FILE: &str = "timings.yml";
pub const FOLDER_CONFIG_KEYS: &[&str] = &[
    "+tags",
    "+output",
    "+profile",
    "+schedule",
    "+vars",
    "+timezone",
    "+locale",
];
pub const SET_ENTRY_KEYS: &[&str] = &[
    "name",
    "profile",
    "vars",
    "exclude",
    "queries",
    "tab_names",
    "output",
    "schedule",
    "locale",
];
pub const QUERY_ENTRY_KEYS: &[&str] = &[
    "query", "profile", "tab", "tab_name", "anchor", "header", "columns",
];
/// The shared output keys that don't belong to one format: they survive a layer changing `format`.
const FORMAT_INDEPENDENT_KEYS: &[&str] = &["name", "queries", "when", "destination", "template"];
/// The keys of a destination entry core reads; every other key is the plugin's.
pub const DESTINATION_KEYS: &[&str] = &["profile", "path", "attach"];
pub const OUTPUT_SHARED_KEYS: &[&str] = &[
    "name",
    "format",
    "queries",
    "when",
    "destination",
    "template",
    "extension",
];
/// Keys of one `output.template.bindings` entry.
pub const TEMPLATE_BINDING_KEYS: &[&str] = &[
    "sheet",
    "query",
    "result_index",
    "anchor",
    "header",
    "columns",
    "cell",
    "value",
    "column",
];
/// Keys of a `plugins:` entry written as a map.
pub const PLUGIN_ENTRY_KEYS: &[&str] = &["name", "version", "github", "local", "registry"];

// ---------------------------------------------------------------------------------------------
// The resolved project: the stable contract every later consumer uses.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub name: String,
    #[serde(skip)]
    pub root: PathBuf,
    /// The folder DRE writes its generated files to (compiled SQL, run outputs, schema
    /// snapshots, `run_results.json`, the manifest): `target/` in the root by default.
    #[serde(skip)]
    pub target_dir: PathBuf,
    /// Where the target path was set, for messages.
    #[serde(skip)]
    pub target_source: crate::target::Source,
    /// The run's target (environment): `--target`, `DRE_TARGET`, else `dev`; `target.name` in
    /// templates. Each profile's entry is [`crate::profiles::Profiles::target_of`].
    #[serde(skip)]
    pub target_name: String,
    /// Where `target_name` came from.
    #[serde(skip)]
    pub target_from: crate::profiles::TargetSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// Its line in dre_project.yml, for messages.
    #[serde(skip)]
    pub default_profile_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_set: Option<String>,
    pub vars: JsonMap<String, Json>,
    pub run_query_max_rows: u64,
    /// `format_options:`: per format, defaults under every output of that format.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub format_options: BTreeMap<String, JsonMap<String, Json>>,
    pub reports: Vec<Report>,
    pub sets: BTreeMap<String, SetDef>,
    /// The declared plugin packages.
    pub plugins: Vec<PluginRequirement>,
    /// Every plugin the project uses, checked against the declared packages once they're
    /// installed ([`crate::plugins::check_uses`]).
    #[serde(skip)]
    pub plugin_uses: Vec<PluginUse>,
    /// Some package's declarations conflict, so it's missing from `plugins`.
    #[serde(skip)]
    pub plugins_incomplete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleEntry>,
    /// Named timings from timings.yml, which schedules use with `timing:`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub timings: BTreeMap<String, Timing>,
    /// Macro files under `macros/`, relative to the root.
    pub macros: Vec<PathBuf>,
    /// Macro packages, each called through its name (`{{ dre_utils.x() }}`).
    pub packages: Vec<Package>,
    /// Mask `DRE_SECRET_*` values in logs and records (default true).
    pub mask_secrets: bool,
    /// The project's default `timezone:` (IANA name); runs default to UTC without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// The project's `locale:` for the number filters; `en` without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// `week_start:` and `week_numbering:`, for the calendar functions.
    #[serde(skip)]
    pub week_start: WeekStart,
    #[serde(skip)]
    pub week_numbering: WeekNumbering,
    /// `dispatch:` search orders, by macro namespace.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dispatch: DispatchOrder,
    /// Every uniquely named `.sql` file under `reports/`, by basename: what `ref()` resolves.
    #[serde(skip)]
    pub sql: BTreeMap<String, PathBuf>,
    /// Lookups under `lookups/`, which `ref()` also resolves.
    #[serde(skip)]
    pub lookups: BTreeMap<String, Lookup>,
    /// A lookup with more rows than this is loaded into a temp table rather than inlined.
    pub lookup_inline_max_rows: u64,
    /// Every folder under `reports/`, as path segments.
    #[serde(skip)]
    pub folders: Vec<Vec<String>>,
    /// The project's own files, relative to the root with forward slashes, sorted: every YAML
    /// file (not a root `profiles.yml`) and everything under `reports/`, `macros/` and
    /// `lookups/`.
    #[serde(skip)]
    pub files: Vec<PathBuf>,
    /// Templates the load's parse pass couldn't render, by report: they make the report invalid
    /// in the manifest. Its own run, compile or validate reports them.
    #[serde(skip)]
    pub parse_errors: BTreeMap<String, Vec<crate::Diagnostic>>,
    /// What the load's parse pass rendered with: the target, `--var`, the run date and timezone.
    #[serde(skip)]
    pub inputs: crate::parse::Inputs,
    /// Declared sources (dbt's `sources:`), by name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, SourceDef>,
    #[serde(skip)]
    pub profiles: Profiles,
}

impl Project {
    pub fn report(&self, name: &str) -> Option<&Report> {
        self.reports.iter().find(|r| r.name == name)
    }

    /// The run's target and where it came from.
    pub fn run_target(&self) -> crate::profiles::RunTarget {
        crate::profiles::RunTarget {
            name: self.target_name.clone(),
            from: self.target_from,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub name: String,
    pub managed: bool,
    /// The defining YAML (managed) or `.sql` (unmanaged), relative to the root.
    pub file: PathBuf,
    /// Folder under `reports/` holding `file`, as path segments.
    pub folder: Vec<String>,
    pub tags: Vec<String>,
    pub queries: Vec<QueryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_set: Option<String>,
    /// The report's `timezone:`, else its folder config's `+timezone`, else the project's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Whether the report declares `sets:` (a Binding per Set) or runs as a single Binding.
    pub has_sets: bool,
    pub bindings: Vec<Binding>,
    /// Report-level resolution with no Set applied: the starting point for an ad hoc `--set`.
    #[serde(skip)]
    pub base: Binding,
}

impl Report {
    pub fn binding(&self, set: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.set.as_deref() == Some(set))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryEntry {
    pub query: String,
    /// The `.sql` file, relative to the root.
    pub path: PathBuf,
    /// The query's own connection (`profile:` on its entry), as written: it may hold Jinja.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Whether this query's result becomes a tab (a sheet, or a file for single-table formats).
    /// One .sql file makes at most one tab: its last statement's result. `tab: false` runs the
    /// file only for its effects (temp views, `SET`s) and discards any result.
    #[serde(skip_serializing_if = "is_true")]
    pub tab: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    /// xlsx: per result column, how to show it (`{format: "#,##0.00"}`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub columns: BTreeMap<String, ColumnOptions>,
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The message for a `tab_name` list: one .sql file makes one tab.
fn one_tab_per_file(name: &str) -> String {
    format!(
        "`tab_name` of `{name}` is a list, but one .sql file makes one tab; put each tab's query in its own .sql file and list each in `queries:`"
    )
}

/// A Report paired with a Set (or the report's single default Binding): everything a run needs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Binding {
    /// The Set name; `None` for a report without `sets:`.
    pub set: Option<String>,
    /// The inherited connection (Set, report, folder `+profile`, `default_profile`), as
    /// written: it may hold Jinja. A query's own `profile:` or a source's overrides it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Fully merged vars: project < folders < report < Set registry < inline Binding.
    pub vars: JsonMap<String, Json>,
    pub queries: Vec<QueryEntry>,
    /// The `locale:` for the number filters: Set, report, folder `+locale`, project; `None`: `en`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// The Binding's outputs, in declared order (`output:` as a list, or one map). Each formats
    /// its own subset of `queries` from the same run.
    pub outputs: Vec<Output>,
    /// Names of every `schedules.yml` entry that runs this Binding.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<String>,
    /// The parse pass for the load's inputs: each query's connection and sources. `None` until
    /// the project is loaded, and for an ad hoc Binding.
    #[serde(skip)]
    pub parsed: Option<std::sync::Arc<crate::parse::ParsedBinding>>,
    /// Where the inherited `profile` is written, for messages about its value.
    #[serde(skip)]
    pub profile_at: Option<ProfileAt>,
}

/// Where a `profile:` value is written: its file, line and key, as messages name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileAt {
    pub file: PathBuf,
    pub line: Option<usize>,
    /// E.g. "`default_profile`", "`+profile` of folder `finance`".
    pub key: String,
}

impl Binding {
    /// Directory name for this Binding under `target/`.
    pub fn dir_name(&self) -> &str {
        self.set.as_deref().unwrap_or("default")
    }

    /// Every output's destinations, in order: the order of [`crate::parse::ParsedBinding::destinations`].
    pub fn destinations(&self) -> impl Iterator<Item = &Destination> {
        self.outputs.iter().flat_map(|o| o.destinations.iter())
    }

    /// The output named `name`.
    pub fn output(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name.as_deref() == Some(name))
    }
}

/// The built-in format that renders query results into a short headline.
pub const MESSAGE_FORMAT: &str = "message";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Output {
    /// `name:`, so other outputs can refer to it; the default file name when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub format: String,
    /// `queries:`: which of the Binding's queries this output formats; `None`: all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    /// `when:`: a Jinja expression; the output is skipped when it's false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Format options: every key except the shared ones ([`OUTPUT_SHARED_KEYS`]).
    pub options: JsonMap<String, Json>,
    /// Where the output is delivered, in order. Empty: it stays in `target/`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<Destination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<Template>,
    /// `extension:` replaces the format's file extension in the default file name
    /// (`<report>.<extension>`); `Some("")` means no extension. `None`: the format's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

impl Output {
    /// Whether this output formats `query`'s result.
    pub fn feeds(&self, query: &str) -> bool {
        self.queries.as_ref().is_none_or(|q| q.iter().any(|n| n == query))
    }

    pub fn is_message(&self) -> bool {
        self.format == MESSAGE_FORMAT
    }

    /// How messages name it: "output `x`", or "output 2" when unnamed.
    pub fn label(&self, index: usize) -> String {
        output_label(self.name.as_deref(), index)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Destination {
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Plugin options: every key other than `profile`, `path` and `attach`, passed to the plugin
    /// after rendering.
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub options: JsonMap<String, Json>,
    /// `attach:` on a message output's entry: other outputs whose files go with the message.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attach: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Template {
    pub file: String,
    pub bindings: Vec<TemplateBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TemplateBinding {
    pub sheet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub vars: JsonMap<String, Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Where it's declared, for messages.
    #[serde(skip)]
    pub file: PathBuf,
    #[serde(skip)]
    pub line: Option<usize>,
}

/// A dbt-style source: tables in one schema of one system.
#[derive(Debug, Clone, Serialize)]
pub struct SourceDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// As written; `database`, `schema`, `profile` and table `identifier`s may hold Jinja.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// DRE's addition: the connection the source lives on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub quoting: QuotingDef,
    pub tags: Vec<String>,
    pub meta: JsonMap<String, Json>,
    pub tables: Vec<SourceTableDef>,
    /// Where it's declared, for messages.
    #[serde(skip)]
    pub file: PathBuf,
    #[serde(skip)]
    pub line: Option<usize>,
}

impl SourceDef {
    pub fn table(&self, name: &str) -> Option<&SourceTableDef> {
        self.tables.iter().find(|t| t.name == name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceTableDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The table's own `quoting`, over the source's.
    pub quoting: QuotingDef,
    pub tags: Vec<String>,
    pub meta: JsonMap<String, Json>,
    pub columns: Vec<SourceColumn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceColumn {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
}

/// `quoting:` as written: an unset part inherits (table from source), else isn't quoted.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct QuotingDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<bool>,
}

impl QuotingDef {
    /// `self` over `under`, unset parts false.
    pub fn over(&self, under: &QuotingDef) -> crate::render::Quoting {
        crate::render::Quoting {
            database: self.database.or(under.database).unwrap_or(false),
            schema: self.schema.or(under.schema).unwrap_or(false),
            identifier: self.identifier.or(under.identifier).unwrap_or(false),
        }
    }
}

/// Source keys DRE reads.
pub const SOURCE_KEYS: &[&str] = &[
    "name",
    "description",
    "database",
    "schema",
    "quoting",
    "tags",
    "meta",
    "tables",
    "profile",
];
pub const SOURCE_TABLE_KEYS: &[&str] = &[
    "name",
    "identifier",
    "description",
    "quoting",
    "tags",
    "meta",
    "columns",
];
pub const SOURCE_COLUMN_KEYS: &[&str] = &["name", "description", "data_type"];
/// dbt source keys DRE accepts but doesn't use yet.
pub const DBT_SOURCE_KEYS: &[&str] = &[
    "loader",
    "loaded_at_field",
    "loaded_at_query",
    "config",
    "overrides",
    "freshness",
    "docs",
];
pub const DBT_SOURCE_TABLE_KEYS: &[&str] = &[
    "loaded_at_field",
    "loaded_at_query",
    "tests",
    "data_tests",
    "freshness",
    "external",
    "config",
    "docs",
];
pub const DBT_SOURCE_COLUMN_KEYS: &[&str] = &[
    "meta",
    "tags",
    "quote",
    "tests",
    "data_tests",
    "constraints",
    "config",
    "docs",
    "granularity",
];

/// A plugin's kind; the same type the protocol uses.
pub use dre_protocol::Kind as PluginKind;
pub use dre_protocol::PluginId;
pub use dre_protocol::msg::ColumnOptions;

/// A declared plugin package.
#[derive(Debug, Clone, Serialize)]
pub struct PluginRequirement {
    /// The package's name.
    pub name: String,
    /// The intersection of every declared constraint.
    pub version: String,
    /// Files declaring it, relative to the root.
    pub declared_in: Vec<PathBuf>,
    /// Where it's installed from.
    #[serde(skip_serializing_if = "PluginSource::is_default")]
    pub source: PluginSource,
}

/// A plugin the project uses: a profile's `type:` or an output's `format:`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginUse {
    pub plugin: PluginId,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    /// What uses it, for messages: "`type: s3` used by destination profile `reports`".
    pub what: String,
}

/// Where a plugin comes from: `dependencies.yml`'s `registry:`, `github:` or `local:`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    /// The default registry (`DRE_REGISTRY_URL`, else DRE's own).
    #[default]
    Default,
    /// Another registry index: a URL or a file path.
    Registry(String),
    /// The GitHub Releases of `owner/repo`.
    Github(String),
    /// An executable on disk, relative to the project root. Used in place, never installed.
    Local(String),
}

impl PluginSource {
    pub fn is_default(&self) -> bool {
        *self == PluginSource::Default
    }

    /// How `dre.lock` records it; `None` for the default registry.
    pub fn lock_key(&self) -> Option<String> {
        match self {
            PluginSource::Default => None,
            PluginSource::Registry(u) => Some(format!("registry:{u}")),
            PluginSource::Github(r) => Some(format!("github:{r}")),
            PluginSource::Local(p) => Some(format!("local:{p}")),
        }
    }
}

impl PluginRequirement {
    pub fn req(&self) -> semver::VersionReq {
        semver::VersionReq::parse(&self.version).unwrap_or(semver::VersionReq::STAR)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    /// The resolved timing: `cron`/`every`/`rrule`, `starting`, `at`, `except`, `also`.
    pub schedule: JsonMap<String, Json>,
    /// The `timings.yml` entry the timing comes from, when it's a shared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    /// `enabled: false` pauses the schedule: it keeps its name but never fires.
    pub enabled: bool,
    /// Layered into `var()` when run with `--schedule <name>`, above the Binding's own vars.
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub vars: JsonMap<String, Json>,
    /// The run's timezone under `--schedule <name>`, above the report's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Line in its schedules.yml, for messages.
    #[serde(skip)]
    pub location: (PathBuf, Option<usize>),
}

/// A timings.yml entry: a timing any schedule can use by name.
#[derive(Debug, Clone, Serialize)]
pub struct Timing {
    /// `cron`/`every`/`rrule`, `starting`, `at`, `except`, `also`.
    pub schedule: JsonMap<String, Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Line in its timings.yml, for messages.
    #[serde(skip)]
    pub location: (PathBuf, Option<usize>),
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// `--profiles-dir`; falls back to `DRE_PROFILES_DIR`, the project directory, then `~/.dre`.
    pub profiles_dir: Option<PathBuf>,
    /// `--target`, above `DRE_TARGET`: the run's target, and every profile's entry.
    pub target: Option<String>,
    /// `DRE_RUN_DATE`, `DRE_RUN_AT` and `--timezone`/`DRE_TIMEZONE`: the parse pass renders
    /// `run.*` as the run will.
    pub date: Option<chrono::NaiveDate>,
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timezone: Option<String>,
    /// `--var name=value`, the top of every `var()` chain.
    pub vars: BTreeMap<String, String>,
    /// `--target-path`, above `DRE_TARGET_PATH` and `target_path:`.
    pub target_path: Option<String>,
}

/// Parse and validate the project at `root`. Returns the resolved project when it could be
/// built at all, plus every diagnostic found along the way.
pub fn load(root: &Path, opts: &LoadOptions) -> (Option<Project>, Diagnostics) {
    let mut l = Loader {
        root: root.to_path_buf(),
        target: crate::target::TargetPath::default_for(root),
        target_inside: Some(PathBuf::from(TARGET_DIR)),
        opts: opts.clone(),
        diags: Diagnostics::default(),
        format_options: Mapping::new(),
    };
    let project = l.run();
    (project, l.diags)
}

struct Loader {
    root: PathBuf,
    /// The resolved target folder.
    target: crate::target::TargetPath,
    /// The target folder relative to the root, when it's inside the project: discovery skips it.
    target_inside: Option<PathBuf>,
    opts: LoadOptions,
    diags: Diagnostics,
    /// `format_options:` from the project file: per format, defaults under every output of it.
    format_options: Mapping,
}

/// One report YAML fragment, before merging.
struct Fragment {
    file: Rc<YamlFile>,
    name: String,
    explicit_name: bool,
    folder: Vec<String>,
    map: Mapping,
    /// The keys DRE reads with typed config.
    typed: ReportFile,
}

/// The line of a top-level key in a YAML file.
fn key_line(yf: &YamlFile, key: &str) -> Option<usize> {
    yf.node.entry(key).map(|(k, _)| k.line).filter(|l| *l > 0)
}

/// Folder-level config from `reports:` in `dre_project.yml`.
#[derive(Default, Clone)]
struct FolderCfg {
    tags: Vec<String>,
    output: Option<Mapping>,
    profile: Option<(String, Option<usize>)>,
    vars: Option<Mapping>,
    timezone: Option<String>,
    locale: Option<String>,
}

struct Discovered {
    yaml: Vec<PathBuf>,
    /// `.sql` under `reports/`, relative paths.
    sql: Vec<PathBuf>,
    macros: Vec<PathBuf>,
    /// Files under `lookups/`, relative paths.
    lookups: Vec<PathBuf>,
    folders: Vec<Vec<String>>,
    /// Every project source file, relative: see [`Project::sources`].
    sources: Vec<PathBuf>,
}

/// Where a report-level value came from, for error messages.
struct Located<T> {
    value: T,
    file: Rc<YamlFile>,
}

impl Loader {
    fn rel(&self, p: &Path) -> PathBuf {
        crate::slash(p.strip_prefix(&self.root).unwrap_or(p))
    }

    fn run(&mut self) -> Option<Project> {
        let pfile = self.root.join(PROJECT_FILE);
        if !pfile.is_file() {
            self.diags.error(
                "project-file-missing",
                Some(PathBuf::from(PROJECT_FILE)),
                None,
                format!(
                    "no {PROJECT_FILE} found in {}",
                    crate::slash(&self.root).display()
                ),
            );
            return None;
        }
        let pyaml = Rc::new(YamlFile::load(
            &pfile,
            PathBuf::from(PROJECT_FILE),
            &mut self.diags,
        )?);
        let mut pfile_typed = match de::from_node::<Loose<ProjectFile>>(&pyaml.node) {
            Ok(Loose::Ok(p)) => p,
            Ok(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-project",
                    Some(pyaml.display.clone()),
                    None,
                    format!("{PROJECT_FILE} must be a map"),
                );
                return None;
            }
            Err(e) => {
                self.diags
                    .error("invalid-project", Some(pyaml.display.clone()), e.line, e.message);
                return None;
            }
        };
        let target_line = pfile_typed.target_path.as_ref().and_then(de::Located::line);
        let project_target = match &pfile_typed.target_path {
            None => None,
            Some(de::Located {
                value: Loose::Ok(t), ..
            }) => Some(t.as_str()),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    Some(PathBuf::from(PROJECT_FILE)),
                    target_line,
                    "`target_path` must be a path",
                );
                return None;
            }
        };
        match crate::target::resolve(&self.root, self.opts.target_path.as_deref(), project_target) {
            Ok(t) => {
                self.target_inside = crate::target::inside(&self.root, &t.dir);
                self.target = t;
            }
            Err(e) => {
                let from_file = self.opts.target_path.is_none()
                    && std::env::var(crate::target::ENV).map_or(true, |v| v.is_empty());
                self.diags.error(
                    "invalid-target-path",
                    from_file.then(|| PathBuf::from(PROJECT_FILE)),
                    from_file.then_some(target_line).flatten(),
                    e,
                );
                return None;
            }
        }
        let folders_cfg = pfile_typed.reports.take();
        let default_profile_line = pfile_typed.default_profile.as_ref().and_then(de::Located::line);
        let mut project = self.parse_project_file(&pyaml, pfile_typed)?;

        let found = self.discover();
        project.folders = found.folders.clone();
        project.macros = found.macros.clone();
        project.files = found.sources.clone();
        let declared = packages::declared(&self.root, &mut self.diags);
        project.packages = packages::resolve(&self.root, &declared, &mut self.diags);
        self.check_macro_namespaces(&project);

        let (profiles_dir, found_by) =
            crate::profiles::locate(self.opts.profiles_dir.as_deref(), Some(&self.root));
        project.profiles = Profiles::load(&profiles_dir, found_by, &mut self.diags);
        project.profiles.run = project.run_target();

        // Folder config needs the folder list to warn about folders that don't exist.
        let folder_cfg = self.parse_folder_config(&pyaml.display, folders_cfg, &project.folders);
        let mut used = Usage::default();
        if let Some(p) = &project.default_profile {
            used.connection(p, Some(pyaml.display.clone()), default_profile_line);
        }
        for cfg in folder_cfg.values() {
            if let Some((p, line)) = &cfg.profile {
                used.connection(p, Some(pyaml.display.clone()), *line);
            }
        }

        // Classify every YAML file by shape.
        let mut fragments = Vec::new();
        let mut plugin_decls: Vec<(Rc<YamlFile>, Mapping)> = Vec::new();
        let mut set_files = Vec::new();
        let mut schedule_files = Vec::new();
        let mut timing_files = Vec::new();
        let mut source_decls: Vec<Rc<YamlFile>> = Vec::new();
        plugin_decls.push((pyaml.clone(), pick(&pyaml.value, PLUGIN_KEYS)));
        if pyaml.value.get(SOURCES_KEY).is_some() {
            source_decls.push(pyaml.clone());
        }
        for path in &found.yaml {
            let display = self.rel(path);
            let Some(yf) = YamlFile::load(path, display.clone(), &mut self.diags) else {
                continue;
            };
            let yf = Rc::new(yf);
            if yf.value.get(SOURCES_KEY).is_some() {
                source_decls.push(yf.clone());
            }
            self.classify(
                yf,
                &mut fragments,
                &mut plugin_decls,
                &mut set_files,
                &mut schedule_files,
                &mut timing_files,
            );
        }

        project.sources = self.parse_sources(&source_decls);
        project.sets = self.parse_sets(&set_files);
        let sql_index = self.index_sql(&found.sql);
        project.sql = sql_index
            .iter()
            .filter(|(_, paths)| paths.len() == 1)
            .map(|(name, paths)| (name.clone(), paths[0].clone()))
            .collect();
        // Queries resolve by bare basename; whatever no report YAML references is unmanaged.
        let mut referenced: BTreeSet<String> = fragments
            .iter()
            .filter_map(|f| f.map.get("queries").and_then(Value::as_sequence))
            .flatten()
            .filter_map(entry_name)
            .collect();
        project.lookups = lookups::discover(&self.root, &found.lookups, &mut self.diags);
        for (name, l) in &project.lookups {
            if let Some(sql) = sql_index.get(name) {
                self.diags.error(
                    "duplicate-ref-name",
                    Some(l.file.clone()),
                    None,
                    format!(
                        "lookup `{name}` has the same name as {}; `ref()` names must be unique",
                        sql[0].display()
                    ),
                );
            }
            if let Err(e) = lookups::read(&self.root, l) {
                self.diags.error("invalid-lookup", Some(l.file.clone()), None, e);
            }
        }
        // A file used through `ref()` is shared SQL, not an unmanaged report.
        referenced.extend(self.check_refs(&found.sql, &found.macros, &sql_index, &project.lookups));
        let mut reports = self.merge_reports(fragments);

        let mut built = Vec::new();
        for r in reports.iter_mut() {
            let queries = self.resolve_queries(r, &sql_index, &mut referenced);
            built.push((queries, r));
        }
        let mut resolved: Vec<Report> = Vec::new();
        for (queries, raw) in built {
            if let Some(rep) = self.resolve_managed(raw, queries, &project, &folder_cfg, &mut used) {
                resolved.push(rep);
            }
        }
        let managed_names: BTreeMap<String, PathBuf> = resolved
            .iter()
            .map(|r| (r.name.clone(), r.file.clone()))
            .collect();
        for (name, path) in &sql_index {
            if referenced.contains(name) || path.len() != 1 {
                continue;
            }
            let path = &path[0];
            if let Some(other) = managed_names.get(name) {
                self.diags.error(
                    "duplicate-report-name",
                    Some(path.clone()),
                    None,
                    format!(
                        "unmanaged report `{name}` has the same name as the report declared in {}; report names must be unique project-wide",
                        other.display()
                    ),
                );
                continue;
            }
            resolved.push(self.resolve_unmanaged(name, path, &project, &folder_cfg, &mut used));
        }
        resolved.sort_by(|a, b| a.name.cmp(&b.name));
        project.reports = resolved;

        // Pre-flight first: its errors say a template's problem better than a failed parse does.
        self.preflight(&project, &used);
        self.parse_pass(&mut project, &mut used);
        self.check_profiles(&project, &used);
        self.check_plugins(&plugin_decls, &mut project, &used);
        let broken_timings;
        (project.timings, broken_timings) = self.parse_timings(&timing_files);
        project.schedules = self.parse_schedules(&schedule_files, &project, &broken_timings);
        self.apply_schedules(&mut project);
        self.check_template_files(&project);

        Some(project)
    }

    // -- dre_project.yml ----------------------------------------------------------------------

    fn parse_project_file(&mut self, yf: &Rc<YamlFile>, pf: ProjectFile) -> Option<Project> {
        let file = Some(yf.display.clone());
        for (k, line) in [("destinations", &pf.destinations), ("formats", &pf.formats)] {
            if line.is_some() {
                self.old_plugin_key(yf, k);
            }
        }
        if let Some(t) = &pf.target {
            self.diags.error(
                "removed-key",
                file.clone(),
                t.line(),
                "`target` in dre_project.yml was removed in DRE 0.2.1: give each profile its default with `target:` in profiles.yml, or choose the run's target with DRE_TARGET or --target",
            );
        }
        for k in &pf.unknown.0 {
            self.diags.error(
                "unknown-key",
                file.clone(),
                Some(k.line),
                format!("unknown key `{}`", k.name),
            );
        }
        let name = match pf.name {
            Some(de::Located {
                value: Loose::Ok(s), ..
            }) if !s.trim().is_empty() => Some(s),
            Some(n) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    n.line(),
                    "`name` must be a non-empty string",
                );
                None
            }
            None => {
                self.diags.error(
                    "missing-field",
                    file.clone(),
                    None,
                    "missing required field `name`",
                );
                None
            }
        };
        let default_profile_line = pf.default_profile.as_ref().and_then(de::Located::line);
        let default_profile = self.typed_string(&yf.display, pf.default_profile, "default_profile");
        let run_target = crate::profiles::RunTarget::resolve(self.opts.target.as_deref());
        let default_set = self.typed_string(&yf.display, pf.default_set, "default_set");
        let vars = match pf.vars {
            None
            | Some(de::Located {
                value: Loose::Ok(None),
                ..
            }) => JsonMap::new(),
            Some(de::Located {
                value: Loose::Ok(Some(m)),
                ..
            }) => m,
            Some(v) => {
                self.diags
                    .error("invalid-field", file.clone(), v.line(), "`vars` must be a map");
                JsonMap::new()
            }
        };
        let run_query_max_rows = match pf.run_query_max_rows {
            None => DEFAULT_RUN_QUERY_MAX_ROWS,
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) if n > 0 => n,
            Some(v) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    v.line(),
                    "`run_query_max_rows` must be a positive whole number",
                );
                DEFAULT_RUN_QUERY_MAX_ROWS
            }
        };
        let dispatch = self.parse_dispatch(&yf.display, pf.dispatch);
        match pf.format_options {
            None | Some(de::Located { value: Loose::Ok(None), .. }) => {}
            Some(de::Located { value: Loose::Ok(Some(f)), .. }) if f.iter().all(|(_, v)| v.ok().is_some()) => {
                self.format_options = f
                    .0
                    .into_iter()
                    .filter_map(|(k, v)| match v {
                        Loose::Ok(m) => Some((Value::String(k.value), Value::Mapping(json_to_yaml(m)))),
                        Loose::Bad(_) => None,
                    })
                    .collect();
            }
            Some(f) => self.diags.error(
                "invalid-field",
                file.clone(),
                f.line(),
                "`format_options` must map format names to their options, e.g. `delimited: {delimiter: \"|\"}`",
            ),
        }
        let mask_secrets = match pf.mask_secrets {
            None => true,
            Some(de::Located {
                value: Loose::Ok(b), ..
            }) => b,
            Some(v) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    v.line(),
                    "`mask_secrets` must be true or false",
                );
                true
            }
        };
        let timezone = pf.timezone.and_then(|v| {
            let line = v.line();
            self.timezone_str(v.value.ok().map(String::as_str), &yf.display, line, "`timezone`")
        });
        let locale = pf.locale.and_then(|v| {
            let line = v.line();
            self.locale_str(v.value.ok().map(String::as_str), &yf.display, line, "`locale`")
        });
        let week_start = match pf.week_start {
            None => WeekStart::Monday,
            Some(de::Located {
                value: Loose::Ok(w), ..
            }) => w,
            Some(v) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    v.line(),
                    "`week_start` must be `monday` or `sunday`",
                );
                WeekStart::Monday
            }
        };
        let week_numbering = match pf.week_numbering {
            None => WeekNumbering::Iso,
            Some(de::Located {
                value: Loose::Ok(w), ..
            }) => w,
            Some(v) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    v.line(),
                    "`week_numbering` must be `iso` or `us`",
                );
                WeekNumbering::Iso
            }
        };
        let lookup_inline_max_rows = match pf.lookup_inline_max_rows {
            None => DEFAULT_INLINE_MAX_ROWS,
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) => n,
            Some(v) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    v.line(),
                    "`lookup_inline_max_rows` must be a whole number",
                );
                DEFAULT_INLINE_MAX_ROWS
            }
        };
        Some(Project {
            name: name?,
            root: self.root.clone(),
            target_dir: self.target.dir.clone(),
            target_source: self.target.source,
            target_name: run_target.name,
            target_from: run_target.from,
            default_profile_line,
            default_profile,
            default_set,
            vars,
            run_query_max_rows,
            reports: Vec::new(),
            sets: BTreeMap::new(),
            plugins: Vec::new(),
            plugin_uses: Vec::new(),
            plugins_incomplete: false,
            schedules: Vec::new(),
            timings: BTreeMap::new(),
            macros: Vec::new(),
            packages: Vec::new(),
            mask_secrets,
            timezone,
            locale,
            week_start,
            week_numbering,
            dispatch,
            sql: BTreeMap::new(),
            lookups: BTreeMap::new(),
            lookup_inline_max_rows,
            format_options: self
                .format_options
                .iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), yaml_map_to_json(v.as_mapping()?))))
                .collect(),
            folders: Vec::new(),
            files: Vec::new(),
            inputs: crate::parse::Inputs::default(),
            parse_errors: BTreeMap::new(),
            sources: BTreeMap::new(),
            profiles: Profiles::default(),
        })
    }

    /// An optional string key of a typed file: its value, else an error.
    fn typed_string(
        &mut self,
        file: &Path,
        v: Option<de::Located<Loose<String>>>,
        key: &str,
    ) -> Option<String> {
        match v? {
            de::Located {
                value: Loose::Ok(s), ..
            } => Some(s),
            v => {
                self.diags.error(
                    "invalid-field",
                    Some(file.to_path_buf()),
                    v.line(),
                    format!("`{key}` must be a string"),
                );
                None
            }
        }
    }

    /// A `timezone:` value: an IANA name, else an error at `line` (`v` is `None` when it isn't a
    /// string).
    fn timezone_str(
        &mut self,
        v: Option<&str>,
        file: &Path,
        line: Option<usize>,
        what: &str,
    ) -> Option<String> {
        let msg = match v {
            Some(s) => match crate::dates::parse_tz(s) {
                Ok(_) => return Some(s.to_string()),
                Err(e) => format!("{what}: {e}"),
            },
            None => format!("{what} must be a string, an IANA timezone name such as `Australia/Sydney`"),
        };
        self.diags
            .error("invalid-timezone", Some(file.to_path_buf()), line, msg);
        None
    }

    /// A `locale:` value: a tag the number filters know (`de-DE`), else an error at `line` (`v`
    /// is `None` when it isn't a string).
    fn locale_str(
        &mut self,
        v: Option<&str>,
        file: &Path,
        line: Option<usize>,
        what: &str,
    ) -> Option<String> {
        let msg = match v {
            Some(s) => match crate::numbers::Locale::parse(s) {
                Ok(_) => return Some(s.to_string()),
                Err(e) => format!("{what}: {e}"),
            },
            None => format!("{what} must be a string, a locale such as `de-DE`"),
        };
        self.diags
            .error("invalid-locale", Some(file.to_path_buf()), line, msg);
        None
    }

    /// `dispatch: [{macro_namespace: dre_utils, search_order: [my_project, dre_utils]}]`.
    fn parse_dispatch(&mut self, file: &Path, v: Option<config::project::DispatchList>) -> DispatchOrder {
        let mut out = DispatchOrder::new();
        let Some(v) = v else { return out };
        let line = v.line();
        let Loose::Ok(list) = v.value else {
            self.diags.error(
                "invalid-field",
                Some(file.to_path_buf()),
                line,
                "`dispatch` must be a list of `{macro_namespace, search_order}`",
            );
            return out;
        };
        for e in list {
            let (ns, order) = match e {
                Loose::Ok(e) => {
                    let ns = e.macro_namespace.and_then(|n| n.ok().cloned());
                    let order: Option<Vec<String>> = e
                        .search_order
                        .and_then(|s| s.ok().map(|s| s.iter().filter_map(|x| x.ok().cloned()).collect()));
                    (ns, order)
                }
                Loose::Bad(_) => (None, None),
            };
            match (ns, order) {
                (Some(ns), Some(order)) if !order.is_empty() => {
                    out.insert(ns, order);
                }
                _ => self.diags.error(
                    "invalid-field",
                    Some(file.to_path_buf()),
                    line,
                    "each `dispatch` entry needs `macro_namespace` and a non-empty `search_order` list",
                ),
            }
        }
        out
    }

    /// Package names are Jinja variables: they can't clash with each other's macros, the
    /// project's own macros, or DRE's functions.
    fn check_macro_namespaces(&mut self, project: &Project) {
        let own: BTreeSet<String> = project
            .macros
            .iter()
            .filter_map(|m| std::fs::read_to_string(self.root.join(m)).ok())
            .flat_map(|src| preflight::macro_defs(&src).into_iter().map(|d| d.name))
            .collect();
        for p in &project.packages {
            let clash = if RESERVED_NAMES.contains(&p.name.as_str()) {
                Some("a DRE function or variable".to_string())
            } else if own.contains(&p.name) {
                Some("a macro in macros/".to_string())
            } else if p.name == project.name {
                Some("this project".to_string())
            } else {
                None
            };
            if let Some(c) = clash {
                self.diags.error(
                    "package-name-clash",
                    None,
                    None,
                    format!(
                        "package `{}` has the same name as {c}; macros couldn't be called through it",
                        p.name
                    ),
                );
            }
        }
        for (ns, order) in &project.dispatch {
            for n in order {
                if n != &project.name && !project.packages.iter().any(|p| &p.name == n) {
                    self.diags.error(
                        "invalid-field",
                        Some(PathBuf::from(PROJECT_FILE)),
                        None,
                        format!("`dispatch` for `{ns}` searches `{n}`, which is neither this project nor an installed package"),
                    );
                }
            }
        }
    }

    fn parse_folder_config(
        &mut self,
        file: &Path,
        tree: Option<de::Located<Loose<config::project::Folder>>>,
        folders: &[Vec<String>],
    ) -> BTreeMap<Vec<String>, FolderCfg> {
        let mut out = BTreeMap::new();
        let Some(tree) = tree else {
            return out;
        };
        let display = Some(file.to_path_buf());
        let mut stack = vec![(Vec::<String>::new(), tree)];
        while let Some((path, node)) = stack.pop() {
            let line = node.line();
            let Loose::Ok(f) = node.value else {
                self.diags.error(
                    "invalid-folder-config",
                    display.clone(),
                    line,
                    format!("folder config for `{}` must be a map", dotted(&path)),
                );
                continue;
            };
            let any = f.sets_anything();
            let mut cfg = FolderCfg::default();
            let bad = |s: &mut Self, k: &str, kline: Option<usize>, what: &str| {
                s.diags.error(
                    "invalid-field",
                    display.clone(),
                    kline,
                    format!("`{k}` must be {what}"),
                )
            };
            if let Some(v) = f.tags {
                match v.value {
                    Loose::Ok(t) => cfg.tags = t,
                    Loose::Bad(_) => bad(self, "+tags", v.line(), "a list of strings"),
                }
            }
            if let Some(v) = f.output {
                match v.value {
                    Loose::Ok(o) => cfg.output = Some(json_to_yaml(o)),
                    Loose::Bad(_) => bad(self, "+output", v.line(), "a map"),
                }
            }
            if let Some(v) = f.profile {
                let kline = v.line();
                match v.value {
                    Loose::Ok(p) => cfg.profile = Some((p, kline)),
                    Loose::Bad(_) => bad(self, "+profile", kline, "a string"),
                }
            }
            if let Some(v) = f.schedule {
                let ctx = format!("folder `{}`: `+schedule`", dotted(&path));
                self.moved_to_schedules(file, v.line(), &ctx);
            }
            if let Some(v) = f.timezone {
                let kline = v.line();
                cfg.timezone =
                    self.timezone_str(v.value.ok().map(String::as_str), file, kline, "`+timezone`");
            }
            if let Some(v) = f.locale {
                let kline = v.line();
                cfg.locale = self.locale_str(v.value.ok().map(String::as_str), file, kline, "`+locale`");
            }
            if let Some(v) = f.vars {
                match v.value {
                    Loose::Ok(s) => cfg.vars = Some(json_to_yaml(s)),
                    Loose::Bad(_) => bad(self, "+vars", v.line(), "a map"),
                }
            }
            for (k, v) in f.rest.0 {
                if k.value.starts_with('+') {
                    self.diags.error(
                        "unknown-key",
                        display.clone(),
                        k.line(),
                        format!(
                            "unknown folder config `{}`; folder config keys are {}",
                            k.value,
                            FOLDER_CONFIG_KEYS.join(", ")
                        ),
                    );
                } else {
                    let mut p = path.clone();
                    p.push(k.value);
                    stack.push((p, v));
                }
            }
            if any && !path.is_empty() {
                if !folders.contains(&path) {
                    self.diags.warning(
                        "unknown-folder",
                        display.clone(),
                        line,
                        format!(
                            "folder config for `{}` matches no folder under reports/",
                            dotted(&path)
                        ),
                    );
                }
                out.insert(path, cfg);
            } else if any {
                out.insert(path, cfg);
            }
        }
        out
    }

    // -- discovery ----------------------------------------------------------------------------

    fn discover(&mut self) -> Discovered {
        let mut d = Discovered {
            yaml: Vec::new(),
            sql: Vec::new(),
            macros: Vec::new(),
            lookups: Vec::new(),
            folders: Vec::new(),
            sources: Vec::new(),
        };
        let target_inside = self.target_inside.clone();
        let root = self.root.clone();
        let walker = walkdir::WalkDir::new(&self.root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                // Generated or installed, never part of the project's own sources.
                e.depth() == 0
                    || !(name.starts_with('.')
                        || target_inside
                            .as_deref()
                            .is_some_and(|t| e.path().strip_prefix(&root).is_ok_and(|r| r == t))
                        || (e.depth() == 1 && [TARGET_DIR, DEPS_DIR, LOGS_DIR].contains(&name.as_ref())))
            });
        for e in walker.filter_map(Result::ok) {
            let rel = self.rel(e.path());
            let in_reports = rel.starts_with(REPORTS_DIR);
            if e.file_type().is_dir() {
                if in_reports && rel != Path::new(REPORTS_DIR) {
                    d.folders.push(folder_segments(&rel));
                }
                continue;
            }
            let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("");
            let yaml = matches!(ext, "yml" | "yaml") && rel != Path::new(crate::profiles::PROFILES_FILE);
            if yaml || in_reports || rel.starts_with(MACROS_DIR) || rel.starts_with(LOOKUPS_DIR) {
                d.sources.push(rel.clone());
            }
            if rel.starts_with(LOOKUPS_DIR) {
                d.lookups.push(rel);
                continue;
            }
            match ext {
                // A profiles.yml at the root holds connections, not project config.
                "yml" | "yaml"
                    if rel != Path::new(PROJECT_FILE) && rel != Path::new(crate::profiles::PROFILES_FILE) =>
                {
                    d.yaml.push(e.path().to_path_buf())
                }
                "sql" if rel.starts_with(MACROS_DIR) => d.macros.push(rel),
                "sql" if in_reports => d.sql.push(rel),
                _ => {}
            }
        }
        d
    }

    fn classify(
        &mut self,
        yf: Rc<YamlFile>,
        fragments: &mut Vec<Fragment>,
        plugins: &mut Vec<(Rc<YamlFile>, Mapping)>,
        sets: &mut Vec<Rc<YamlFile>>,
        schedules: &mut Vec<Rc<YamlFile>>,
        timings: &mut Vec<Rc<YamlFile>>,
    ) {
        let in_reports = yf.display.starts_with(REPORTS_DIR);
        let timings_file = yf.display.file_name() == Some(std::ffi::OsStr::new(TIMINGS_FILE));
        match &yf.value {
            Value::Null => {}
            Value::Sequence(items)
                if !items.is_empty()
                    && items
                        .iter()
                        .all(|i| ["name", "select", "report"].iter().any(|k| i.get(k).is_some())) =>
            {
                schedules.push(yf.clone());
            }
            Value::Mapping(m) => {
                let pl = pick(&yf.value, PLUGIN_KEYS);
                if !pl.is_empty() {
                    plugins.push((yf.clone(), pl));
                }
                // `packages:` belongs to the root dependency files, read by `packages::declared`.
                let dependency_file = packages::DEPENDENCY_FILES
                    .iter()
                    .any(|f| yf.display == Path::new(f));
                if !dependency_file && m.contains_key("packages") {
                    self.diags.error(
                        "misplaced-packages",
                        Some(yf.display.clone()),
                        yf.line_of("packages", None),
                        "`packages:` goes in dependencies.yml or packages.yml at the project root",
                    );
                }
                if dependency_file {
                    for k in m.keys().filter_map(Value::as_str) {
                        if OLD_PLUGIN_KEYS.contains(&k) {
                            self.old_plugin_key(&yf, k);
                        }
                    }
                }
                let has_sources = m.contains_key(SOURCES_KEY);
                let rest: Mapping = m
                    .iter()
                    .filter(|(k, _)| !is_one_of(k, PLUGIN_KEYS) && k.as_str() != Some("packages"))
                    .filter(|(k, _)| k.as_str() != Some(SOURCES_KEY))
                    // dbt's `version: 2` at the top of a sources file.
                    .filter(|(k, _)| !(has_sources && k.as_str() == Some("version")))
                    .filter(|(k, _)| !(dependency_file && is_one_of(k, OLD_PLUGIN_KEYS)))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if rest.is_empty() {
                    return;
                }
                let looks_like_report = rest.contains_key("queries") || rest.contains_key("name");
                if !in_reports && (timings_file || is_timing_registry(&rest)) {
                    timings.push(yf.clone());
                } else if in_reports || looks_like_report {
                    self.report_fragment(yf.clone(), rest, fragments);
                } else if is_set_registry(&rest) {
                    sets.push(yf.clone());
                } else {
                    self.diags.warning(
                        "unrecognized-yaml",
                        Some(yf.display.clone()),
                        None,
                        "not recognised as report, Set, plugin or schedule config; ignored",
                    );
                }
            }
            _ => self.diags.warning(
                "unrecognized-yaml",
                Some(yf.display.clone()),
                None,
                "not recognised as report, Set, plugin or schedule config; ignored",
            ),
        }
    }

    fn report_fragment(&mut self, yf: Rc<YamlFile>, map: Mapping, out: &mut Vec<Fragment>) {
        for k in map.keys().filter_map(Value::as_str) {
            if !REPORT_KEYS.contains(&k) {
                self.diags.error(
                    "unknown-key",
                    Some(yf.display.clone()),
                    key_line(&yf, k),
                    format!("unknown report key `{k}`"),
                );
            }
        }
        let typed = match de::from_node::<Loose<ReportFile>>(&yf.node) {
            Ok(Loose::Ok(r)) => r,
            Ok(Loose::Bad(_)) => return,
            Err(e) => {
                self.diags
                    .error("invalid-report", Some(yf.display.clone()), e.line, e.message);
                return;
            }
        };
        let folder = folder_segments(yf.display.parent().unwrap_or(Path::new("")));
        let (name, explicit_name) = match &typed.name {
            Some(de::Located {
                value: Loose::Ok(s), ..
            }) if !s.is_empty() => (s.clone(), true),
            Some(n) => {
                self.diags.error(
                    "invalid-field",
                    Some(yf.display.clone()),
                    n.line(),
                    "`name` must be a non-empty string",
                );
                return;
            }
            None => match folder.last() {
                Some(f) => (f.clone(), false),
                None => {
                    self.diags.error(
                        "missing-field",
                        Some(yf.display.clone()),
                        None,
                        "a report outside a report folder needs an explicit `name:`",
                    );
                    return;
                }
            },
        };
        out.push(Fragment {
            file: yf,
            name,
            explicit_name,
            folder,
            map,
            typed,
        });
    }

    // -- sources ------------------------------------------------------------------------------

    /// `sources:` from every project YAML file, in dbt's shape plus DRE's `profile:`. dbt keys
    /// DRE doesn't use yet are noted once per file; any other key is an error.
    fn parse_sources(&mut self, files: &[Rc<YamlFile>]) -> BTreeMap<String, SourceDef> {
        let mut out: BTreeMap<String, SourceDef> = BTreeMap::new();
        for yf in files {
            let file = Some(yf.display.clone());
            let top = key_line(yf, SOURCES_KEY);
            let plugin_names = match yf.node.get(SOURCES_KEY).map(|n| &n.kind) {
                Some(crate::config::node::Kind::Seq(items)) => items.iter().all(|i| i.as_str().is_some()),
                _ => false,
            };
            if plugin_names {
                // DRE 0.0.x declared source plugins here.
                self.diags.error(
                    "moved-plugin-declaration",
                    file,
                    top,
                    "`sources:` declares tables now (dbt's format); list plugin packages under `plugins:` instead (e.g. `plugins: [duckdb]`)",
                );
                continue;
            }
            let items = match de::from_node::<Loose<config::sources::SourcesFile>>(&yf.node) {
                Ok(Loose::Ok(f)) => f.sources.map(|s| s.value),
                _ => None,
            };
            let items = match items {
                Some(Loose::Ok(Some(items))) => items,
                Some(Loose::Ok(None)) => continue,
                _ => {
                    self.diags.error(
                        "invalid-source",
                        file,
                        top,
                        "`sources` must be a list of sources, as in dbt: `- name: sales` with `tables:`",
                    );
                    continue;
                }
            };
            let mut unsupported: BTreeSet<String> = BTreeSet::new();
            for item in items {
                if let Some(src) = self.source_def(yf, item, &mut unsupported) {
                    if let Some(prev) = out.get(&src.name) {
                        self.diags.error(
                            "duplicate-source",
                            file.clone(),
                            src.line,
                            format!(
                                "source `{}` is also declared in {}; source names must be unique",
                                src.name,
                                prev.file.display()
                            ),
                        );
                        continue;
                    }
                    out.insert(src.name.clone(), src);
                }
            }
            if !unsupported.is_empty() {
                self.diags.warning(
                    "source-key-not-supported",
                    file,
                    top,
                    format!(
                        "dbt source keys DRE doesn't support yet are ignored: {}",
                        unsupported.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                );
            }
        }
        out
    }

    fn source_def(
        &mut self,
        yf: &YamlFile,
        item: de::Located<Loose<config::sources::Source>>,
        unsupported: &mut BTreeSet<String>,
    ) -> Option<SourceDef> {
        let file = Some(yf.display.clone());
        let top = key_line(yf, SOURCES_KEY);
        let item_line = item.line().or(top);
        let Loose::Ok(m) = item.value else {
            self.diags.error(
                "invalid-source",
                file,
                top,
                "each source must be a map with `name:` and `tables:`",
            );
            return None;
        };
        let Some(name) = m
            .name
            .as_ref()
            .and_then(Loose::ok)
            .filter(|n| !n.is_empty())
            .cloned()
        else {
            self.diags
                .error("invalid-source", file, top, "every source needs a `name`");
            return None;
        };
        let line = item_line;
        let ctx = format!("source `{name}`");
        let mut ok = self.source_keys(
            &yf.display,
            &m.unknown,
            &m.dbt_keys(),
            &ctx,
            SOURCE_KEYS,
            "source",
            unsupported,
        );
        let mut text = |s: &mut Self, k: &str, v: &Option<Loose<de::Text>>| -> Option<String> {
            match v {
                None => None,
                Some(Loose::Ok(t)) => Some(t.0.clone()),
                Some(Loose::Bad(_)) => {
                    s.diags.error(
                        "invalid-source",
                        file.clone(),
                        line,
                        format!("{ctx}: `{k}` must be a string"),
                    );
                    ok = false;
                    None
                }
            }
        };
        let description = text(self, "description", &m.description);
        let database = text(self, "database", &m.database);
        let schema = text(self, "schema", &m.schema);
        let profile = text(self, "profile", &m.profile);
        let quoting = self.quoting(&yf.display, m.quoting, &ctx, line);
        let tags = self.source_tags(&yf.display, m.tags, &ctx, line);
        let meta = self.source_meta(&yf.display, m.meta, &ctx, line);
        let mut tables = Vec::new();
        match m.tables {
            None | Some(Loose::Ok(None)) => {}
            Some(Loose::Ok(Some(ts))) => {
                // Duplicates by name, whether or not each entry is otherwise valid.
                let mut names = BTreeSet::new();
                for t in &ts {
                    let Some(n) = t.value.ok().and_then(|t| t.name.as_ref()).and_then(Loose::ok) else {
                        continue;
                    };
                    if !names.insert(n.clone()) {
                        self.diags.error(
                            "duplicate-source-table",
                            file.clone(),
                            t.line().or(line),
                            format!("{ctx} declares table `{n}` twice"),
                        );
                        ok = false;
                    }
                }
                for t in ts {
                    if let Some(t) = self.source_table(yf, &name, t, line, unsupported)
                        && !tables.iter().any(|x: &SourceTableDef| x.name == t.name)
                    {
                        tables.push(t);
                    }
                }
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-source",
                    file.clone(),
                    line,
                    format!("{ctx}: `tables` must be a list"),
                );
                ok = false;
            }
        }
        ok.then(|| SourceDef {
            name: name.to_string(),
            description,
            database,
            schema,
            profile,
            quoting,
            tags,
            meta,
            tables,
            file: yf.display.clone(),
            line,
        })
    }

    fn source_table(
        &mut self,
        yf: &YamlFile,
        source: &str,
        item: de::Located<Loose<config::sources::Table>>,
        source_line: Option<usize>,
        unsupported: &mut BTreeSet<String>,
    ) -> Option<SourceTableDef> {
        let file = Some(yf.display.clone());
        let item_line = item.line();
        let Loose::Ok(m) = item.value else {
            self.diags.error(
                "invalid-source",
                file,
                source_line,
                format!("source `{source}`: each table must be a map with a `name`"),
            );
            return None;
        };
        let Some(name) = m
            .name
            .as_ref()
            .and_then(Loose::ok)
            .filter(|n| !n.is_empty())
            .cloned()
        else {
            self.diags.error(
                "invalid-source",
                file,
                source_line,
                format!("source `{source}`: every table needs a `name`"),
            );
            return None;
        };
        let line = item_line.or(source_line);
        let ctx = format!("source `{source}`, table `{name}`");
        let mut ok = self.source_keys(
            &yf.display,
            &m.unknown,
            &m.dbt_keys(),
            &ctx,
            SOURCE_TABLE_KEYS,
            "table",
            unsupported,
        );
        let mut text = |s: &mut Self, k: &str, v: &Option<Loose<de::Text>>| -> Option<String> {
            match v {
                None => None,
                Some(Loose::Ok(t)) => Some(t.0.clone()),
                Some(Loose::Bad(_)) => {
                    s.diags.error(
                        "invalid-source",
                        file.clone(),
                        line,
                        format!("{ctx}: `{k}` must be a string"),
                    );
                    ok = false;
                    None
                }
            }
        };
        let identifier = text(self, "identifier", &m.identifier);
        let description = text(self, "description", &m.description);
        let quoting = self.quoting(&yf.display, m.quoting, &ctx, line);
        let tags = self.source_tags(&yf.display, m.tags, &ctx, line);
        let meta = self.source_meta(&yf.display, m.meta, &ctx, line);
        let mut columns: Vec<SourceColumn> = Vec::new();
        match m.columns {
            None | Some(Loose::Ok(None)) => {}
            Some(Loose::Ok(Some(cs))) => {
                for c in cs {
                    let Loose::Ok(cm) = c else {
                        self.diags.error(
                            "invalid-source",
                            file.clone(),
                            line,
                            format!("{ctx}: each column must be a map with a `name`"),
                        );
                        ok = false;
                        continue;
                    };
                    let Some(cname) = cm
                        .name
                        .as_ref()
                        .and_then(Loose::ok)
                        .filter(|n| !n.is_empty())
                        .cloned()
                    else {
                        self.diags.error(
                            "invalid-source",
                            file.clone(),
                            line,
                            format!("{ctx}: every column needs a `name`"),
                        );
                        ok = false;
                        continue;
                    };
                    let cctx = format!("{ctx}, column `{cname}`");
                    ok &= self.source_keys(
                        &yf.display,
                        &cm.unknown,
                        &cm.dbt_keys(),
                        &cctx,
                        SOURCE_COLUMN_KEYS,
                        "column",
                        unsupported,
                    );
                    if columns.iter().any(|c| c.name.eq_ignore_ascii_case(&cname)) {
                        self.diags.error(
                            "duplicate-source-column",
                            file.clone(),
                            line,
                            format!("{ctx} declares column `{cname}` twice"),
                        );
                        ok = false;
                        continue;
                    }
                    let s = |v: &Option<Loose<de::Text>>| v.as_ref().and_then(Loose::ok).map(|t| t.0.clone());
                    columns.push(SourceColumn {
                        name: cname,
                        description: s(&cm.description),
                        data_type: s(&cm.data_type),
                    });
                }
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-source",
                    file.clone(),
                    line,
                    format!("{ctx}: `columns` must be a list"),
                );
                ok = false;
            }
        }
        ok.then(|| SourceTableDef {
            name,
            identifier,
            description,
            quoting,
            tags,
            meta,
            columns,
        })
    }

    /// The keys of one source, table or column map that aren't DRE's: dbt's are collected into
    /// `unsupported`, anything else is an error. Returns false on an error.
    #[allow(clippy::too_many_arguments)]
    fn source_keys(
        &mut self,
        file: &Path,
        unknown: &de::UnknownKeys,
        dbt: &[&str],
        ctx: &str,
        known: &[&str],
        level: &str,
        unsupported: &mut BTreeSet<String>,
    ) -> bool {
        for k in dbt {
            unsupported.insert(format!("{level} `{k}`"));
        }
        for k in &unknown.0 {
            self.diags.error(
                "unknown-key",
                Some(file.to_path_buf()),
                Some(k.line),
                format!(
                    "{ctx}: unknown key `{}`; a {level} takes {}",
                    k.name,
                    known.join(", ")
                ),
            );
        }
        unknown.0.is_empty()
    }

    fn quoting(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Quoting>>,
        ctx: &str,
        line: Option<usize>,
    ) -> QuotingDef {
        let mut q = QuotingDef::default();
        let Some(v) = v else { return q };
        let bad = |s: &mut Self, msg: String| {
            s.diags.error(
                "invalid-source",
                Some(file.to_path_buf()),
                line,
                format!("{ctx}: {msg}"),
            )
        };
        let Loose::Ok(qm) = v else {
            bad(
                self,
                "`quoting` must be a map of `database`, `schema` and `identifier` to true or false".into(),
            );
            return q;
        };
        for (k, v, slot) in [
            ("database", qm.database, &mut q.database),
            ("schema", qm.schema, &mut q.schema),
            ("identifier", qm.identifier, &mut q.identifier),
        ] {
            match v {
                None => {}
                Some(Loose::Ok(b)) => *slot = Some(b),
                Some(Loose::Bad(_)) => bad(self, format!("`quoting.{k}` must be true or false")),
            }
        }
        for (k, v) in qm.unknown.0 {
            if v.is_boolean() {
                bad(
                    self,
                    format!(
                        "unknown `quoting` key `{}`; use `database`, `schema` or `identifier`",
                        k.value
                    ),
                );
            } else {
                bad(self, format!("`quoting.{}` must be true or false", k.value));
            }
        }
        q
    }

    fn source_tags(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Tags>>,
        ctx: &str,
        line: Option<usize>,
    ) -> Vec<String> {
        match v {
            None => Vec::new(),
            Some(Loose::Ok(config::sources::Tags(de::OneOf::A(t)))) => vec![t],
            Some(Loose::Ok(config::sources::Tags(de::OneOf::B(l)))) => l,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-source",
                    Some(file.to_path_buf()),
                    line,
                    format!("{ctx}: `tags` must be a list of strings"),
                );
                Vec::new()
            }
        }
    }

    fn source_meta(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Meta>>,
        ctx: &str,
        line: Option<usize>,
    ) -> JsonMap<String, Json> {
        match v {
            None => JsonMap::new(),
            Some(Loose::Ok(m)) => m.0,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-source",
                    Some(file.to_path_buf()),
                    line,
                    format!("{ctx}: `meta` must be a map"),
                );
                JsonMap::new()
            }
        }
    }

    // -- the parse pass -----------------------------------------------------------------------

    /// Render every Binding without a database ([`crate::parse`]): each query's connection and
    /// sources, for selection, the manifest, `dre ls` and validate. Problems go to diagnostics,
    /// except for files that already have an error (the same cause, said better).
    fn parse_pass(&mut self, project: &mut Project, used: &mut Usage) {
        let inputs = crate::parse::Inputs {
            target: project.target_name.clone(),
            cli_vars: self.opts.vars.clone(),
            date: self.opts.date,
            scheduled_at: self.opts.scheduled_at,
            timezone: self.opts.timezone.clone(),
            schedule: None,
            started_at: None,
        };
        project.inputs = inputs.clone();
        let failed: BTreeSet<PathBuf> = self
            .diags
            .iter()
            .filter(|d| d.severity == crate::Severity::Error)
            .filter_map(|d| d.file.clone())
            .collect();
        let mut results = Vec::new();
        for (ri, r) in project.reports.iter().enumerate() {
            for (bi, b) in r.bindings.iter().enumerate() {
                let parsed = crate::parse::binding(project, r, b, &b.vars, &inputs);
                results.push((ri, bi, parsed));
            }
        }
        for (ri, bi, parsed) in results {
            for p in &parsed.errors {
                if failed.contains(&p.file) {
                    continue;
                }
                // A template that doesn't render may depend on this run's `--var`s: it makes
                // its own report invalid (and fails when that report runs or compiles), but
                // doesn't stop the rest of the project.
                if p.code == crate::parse::PARSE_FAILED {
                    let msg = crate::Diagnostic {
                        severity: crate::Severity::Error,
                        code: p.code,
                        message: p.message.clone(),
                        file: Some(p.file.clone()),
                        line: p.line,
                        plugin: None,
                    };
                    let errs = project
                        .parse_errors
                        .entry(project.reports[ri].name.clone())
                        .or_default();
                    if !errs.contains(&msg) {
                        errs.push(msg);
                    }
                    continue;
                }
                self.diags
                    .error(p.code, Some(p.file.clone()), p.line, p.message.clone());
            }
            for p in &parsed.warnings {
                self.diags
                    .warning(p.code, Some(p.file.clone()), p.line, p.message.clone());
            }
            let file = Some(project.reports[ri].file.clone());
            if let Some(i) = &parsed.inherited {
                used.connection(i, file.clone(), None);
            }
            for q in &parsed.queries {
                if let Some(c) = &q.connection {
                    used.connection(c, file.clone(), None);
                }
            }
            for d in parsed.destinations.iter().flatten() {
                used.destination(d, file.clone(), None);
            }
            project.reports[ri].bindings[bi].parsed = Some(std::sync::Arc::new(parsed));
        }
    }

    // -- sets.yml -----------------------------------------------------------------------------

    fn parse_sets(&mut self, files: &[Rc<YamlFile>]) -> BTreeMap<String, SetDef> {
        let mut out = BTreeMap::new();
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(sets)) = de::from_node::<Loose<config::schedule::SetsFile>>(&yf.node) else {
                continue;
            };
            for (k, v) in sets.0.0 {
                let line = k.line();
                let name = k.value;
                if let Some(prev) = seen.get(&name) {
                    self.diags.error(
                        "duplicate-set",
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}` is also declared in {}", prev.display()),
                    );
                    continue;
                }
                seen.insert(name.clone(), yf.display.clone());
                // Anything but a map is a Set with the report's defaults.
                let v = match v {
                    Loose::Ok(Some(v)) => Some(v),
                    _ => None,
                };
                let profile = v
                    .as_ref()
                    .and_then(|v| v.profile.as_ref())
                    .and_then(Loose::ok)
                    .cloned();
                if v.as_ref().is_some_and(|v| v.profile.is_some()) && profile.is_none() {
                    self.diags.error(
                        "invalid-field",
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}`: `profile` must be a string"),
                    );
                }
                let locale = match v.as_ref().and_then(|v| v.locale.as_ref()) {
                    None => None,
                    Some(l) => self.locale_str(
                        l.ok().map(String::as_str),
                        &yf.display,
                        line,
                        &format!("Set `{name}`: `locale`"),
                    ),
                };
                let vars = match v.as_ref().and_then(|v| v.vars.as_ref()) {
                    Some(Loose::Ok(m)) => m.clone(),
                    None => JsonMap::new(),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            "invalid-field",
                            Some(yf.display.clone()),
                            line,
                            format!("Set `{name}`: `vars` must be a map"),
                        );
                        JsonMap::new()
                    }
                };
                out.insert(
                    name,
                    SetDef {
                        profile,
                        vars,
                        locale,
                        file: yf.display.clone(),
                        line,
                    },
                );
            }
        }
        out
    }

    // -- .sql index ---------------------------------------------------------------------------

    /// Literal `ref('name')` calls in SQL and macro files: each must name a project `.sql` file.
    /// Returns the names referenced.
    fn check_refs(
        &mut self,
        sql: &[PathBuf],
        macros: &[PathBuf],
        index: &BTreeMap<String, Vec<PathBuf>>,
        lookups: &BTreeMap<String, Lookup>,
    ) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        // name → the names its file refs, for cycle detection.
        let mut graph: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        for file in sql.iter().chain(macros) {
            let Ok(src) = std::fs::read_to_string(self.root.join(file)) else {
                continue;
            };
            let is_sql = !file.starts_with(MACROS_DIR);
            for (name, line) in preflight::refs(&src) {
                if index.contains_key(&name) {
                    if is_sql {
                        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
                        graph.entry(stem).or_default().push((name.clone(), line));
                    }
                    names.insert(name);
                } else if !lookups.contains_key(&name) {
                    self.diags.error(
                        "unknown-ref",
                        Some(file.clone()),
                        Some(line),
                        format!("`ref('{name}')`: there's no `{name}.sql` under reports/ and no lookup `{name}` under lookups/"),
                    );
                }
            }
        }
        // Report each cycle once, at the ref that closes it.
        let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
        for start in graph.keys() {
            let mut path = vec![start.clone()];
            let mut stack = vec![graph[start].iter()];
            while let Some(it) = stack.last_mut() {
                let Some((next, line)) = it.next() else {
                    stack.pop();
                    path.pop();
                    continue;
                };
                if let Some(i) = path.iter().position(|n| n == next) {
                    let mut cycle = path[i..].to_vec();
                    let mut key = cycle.clone();
                    key.sort();
                    if reported.insert(key) {
                        cycle.push(next.clone());
                        let file = index[path.last().unwrap()][0].clone();
                        self.diags.error(
                            "ref-cycle",
                            Some(file),
                            Some(*line),
                            format!("`ref()` cycle: {}", cycle.join(" → ")),
                        );
                    }
                    continue;
                }
                if let Some(edges) = graph.get(next) {
                    path.push(next.clone());
                    stack.push(edges.iter());
                }
            }
        }
        names
    }

    fn index_sql(&mut self, sql: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
        let mut index: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for p in sql {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            index.entry(stem).or_default().push(p.clone());
        }
        for (name, paths) in &index {
            if paths.len() > 1 {
                self.diags.error(
                    "duplicate-sql-name",
                    Some(paths[1].clone()),
                    None,
                    format!(
                        "`{name}.sql` exists more than once: {}; .sql basenames must be unique project-wide",
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        index
    }

    // -- merging report fragments ---------------------------------------------------------------

    fn merge_reports(&mut self, fragments: Vec<Fragment>) -> Vec<RawReport> {
        let mut by_name: BTreeMap<String, Vec<Fragment>> = BTreeMap::new();
        for f in fragments {
            by_name.entry(f.name.clone()).or_default().push(f);
        }
        let mut out = Vec::new();
        for (name, frags) in by_name {
            let definers: Vec<usize> = (0..frags.len())
                .filter(|i| frags[*i].map.contains_key("queries"))
                .collect();
            if definers.len() > 1 {
                let second = &frags[definers[1]].file;
                self.diags.error(
                    "duplicate-report-name",
                    Some(second.display.clone()),
                    key_line(second, "queries"),
                    format!(
                        "report `{name}` is declared in both {} and {}; report names must be unique project-wide",
                        frags[definers[0]].file.display.display(),
                        second.display.display()
                    ),
                );
                continue;
            }
            let Some(&def) = definers.first() else {
                for f in &frags {
                    let msg = if f.explicit_name {
                        format!(
                            "report `{name}` has no `queries`; a config fragment must name a report that declares `queries:`"
                        )
                    } else {
                        format!("report `{name}` has no `queries`")
                    };
                    self.diags
                        .error("missing-queries", Some(f.file.display.clone()), None, msg);
                }
                continue;
            };
            let mut keys: BTreeMap<String, Located<Value>> = BTreeMap::new();
            let mut owner: BTreeMap<String, usize> = BTreeMap::new();
            let mut lines: BTreeMap<String, Option<usize>> = BTreeMap::new();
            for (i, f) in frags.iter().enumerate() {
                for (k, v) in &f.map {
                    let Some(k) = k.as_str() else { continue };
                    if k == "name" || !REPORT_KEYS.contains(&k) {
                        continue;
                    }
                    if let Some(prev) = keys.get(k) {
                        self.diags.error(
                            "conflicting-declaration",
                            Some(f.file.display.clone()),
                            key_line(&f.file, k),
                            format!(
                                "`{k}` for report `{name}` is declared in both {} and {}",
                                prev.file.display.display(),
                                f.file.display.display()
                            ),
                        );
                        continue;
                    }
                    keys.insert(
                        k.to_string(),
                        Located {
                            value: v.clone(),
                            file: f.file.clone(),
                        },
                    );
                    owner.insert(k.to_string(), i);
                    lines.insert(k.to_string(), key_line(&f.file, k));
                }
            }
            let file = frags[def].file.clone();
            let folder = frags[def].folder.clone();
            let mut typed = ReportFile::default();
            for (i, f) in frags.into_iter().enumerate() {
                let owns = |k: &str| owner.get(k) == Some(&i);
                let ReportFile {
                    name: _,
                    output: _,
                    plugins: _,
                    sources: _,
                    tags,
                    queries,
                    profile,
                    default_set,
                    vars,
                    timezone,
                    locale,
                    sets,
                    schedule,
                    unknown: _,
                } = f.typed;
                if owns("tags") {
                    typed.tags = tags;
                }
                if owns("queries") {
                    typed.queries = queries;
                }
                if owns("profile") {
                    typed.profile = profile;
                }
                if owns("default_set") {
                    typed.default_set = default_set;
                }
                if owns("vars") {
                    typed.vars = vars;
                }
                if owns("timezone") {
                    typed.timezone = timezone;
                }
                if owns("locale") {
                    typed.locale = locale;
                }
                if owns("sets") {
                    typed.sets = sets;
                }
                if owns("schedule") {
                    typed.schedule = schedule;
                }
            }
            out.push(RawReport {
                name,
                file,
                folder,
                keys,
                lines,
                typed,
            });
        }
        out
    }

    fn resolve_queries(
        &mut self,
        r: &RawReport,
        index: &BTreeMap<String, Vec<PathBuf>>,
        referenced: &mut BTreeSet<String>,
    ) -> Vec<QueryEntry> {
        let Some(q) = &r.typed.queries else {
            return Vec::new();
        };
        let file = r
            .keys
            .get("queries")
            .map_or_else(|| r.file.display.clone(), |k| k.file.display.clone());
        let Loose::Ok(items) = &q.value else {
            self.diags.error(
                "invalid-field",
                Some(file),
                q.line(),
                format!("report `{}`: `queries` must be a list", r.name),
            );
            return Vec::new();
        };
        if items.is_empty() {
            self.diags.error(
                "missing-queries",
                Some(file.clone()),
                q.line(),
                format!("report `{}` has an empty `queries` list", r.name),
            );
        }
        let mut out = Vec::new();
        for item in items {
            if let Some(e) = self.query_entry(&r.name, item, &file, index) {
                referenced.insert(e.query.clone());
                out.push(e);
            } else if let Some(n) = query_item_name(item) {
                referenced.insert(n.to_string());
            }
        }
        out
    }

    fn query_entry(
        &mut self,
        report: &str,
        item: &QueryItem,
        file: &Path,
        index: &BTreeMap<String, Vec<PathBuf>>,
    ) -> Option<QueryEntry> {
        let file = Some(file.to_path_buf());
        let line = item.line();
        let (name, m) = match &item.value {
            Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
            Loose::Ok(de::OneOf::B(m)) => match &m.query {
                Some(Loose::Ok(s)) => (s.clone(), Some(m)),
                _ => {
                    self.diags.error(
                        "invalid-field",
                        file,
                        line,
                        format!("report `{report}`: a `queries` map entry needs a `query:` name"),
                    );
                    return None;
                }
            },
            Loose::Bad(_) => {
                self.diags.error(
                    "invalid-field",
                    file,
                    line,
                    format!("report `{report}`: `queries` entries must be names or maps"),
                );
                return None;
            }
        };
        if name.ends_with(".sql") || name.contains('/') || name.contains('\\') {
            self.diags.error(
                "invalid-query-name",
                file,
                line,
                format!(
                    "report `{report}`: query `{name}` must be a bare .sql basename, with no path and no extension"
                ),
            );
            return None;
        }
        let mut e = QueryEntry {
            query: name.clone(),
            path: PathBuf::new(),
            profile: None,
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
            columns: BTreeMap::new(),
        };
        if let Some(m) = m {
            for k in &m.unknown.0 {
                self.diags.error(
                    "unknown-key",
                    file.clone(),
                    Some(k.line),
                    format!("report `{report}`: unknown key `{}` on query `{name}`", k.name),
                );
            }
            match &m.profile {
                None => {}
                Some(Loose::Ok(p)) if !p.trim().is_empty() => e.profile = Some(p.clone()),
                Some(_) => self.diags.error(
                    "invalid-field",
                    file.clone(),
                    line,
                    format!("report `{report}`: `profile` of `{name}` must be a connection name"),
                ),
            }
            e.tab_name = match &m.tab_name {
                None => None,
                Some(Loose::Ok(s)) => Some(s.clone()),
                Some(Loose::Bad(f)) if f.kind == "a list" => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: {}", one_tab_per_file(&name)),
                    );
                    None
                }
                Some(Loose::Bad(_)) => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: `tab_name` of `{name}` must be a string"),
                    );
                    None
                }
            };
            match &m.tab {
                None => {}
                Some(Loose::Ok(b)) => e.tab = *b,
                Some(Loose::Bad(_)) => self.diags.error(
                    "invalid-field",
                    file.clone(),
                    line,
                    format!("report `{report}`: `tab` of `{name}` must be true or false"),
                ),
            }
            if !e.tab && e.tab_name.is_some() {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    line,
                    format!("report `{report}`: `{name}` has `tab: false`, so its `tab_name` would never be used; remove one of them"),
                );
            }
            if let Some(a) = &m.anchor {
                match a {
                    Loose::Ok(s) if options::is_cell(s) => e.anchor = Some(s.clone()),
                    _ => self.diags.error(
                        "invalid-cell",
                        file.clone(),
                        line,
                        format!("report `{report}`: `anchor` of `{name}` must be a cell reference like `A1`"),
                    ),
                }
            }
            if let Some(h) = &m.header {
                match h {
                    Loose::Ok(b) => e.header = Some(*b),
                    Loose::Bad(_) => self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: `header` of `{name}` must be true or false"),
                    ),
                }
            }
            if let Some(c) = &m.columns {
                let (columns, errs) = dre_protocol::options::parse_columns(c);
                for err in errs {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: query `{name}`: {err}"),
                    );
                }
                e.columns = columns;
            }
        }
        match index.get(&name) {
            Some(paths) => {
                e.path = paths[0].clone();
                Some(e)
            }
            None => {
                self.diags.error(
                    "unknown-query",
                    file,
                    line,
                    format!("report `{report}`: query `{name}` doesn't match any .sql file under reports/"),
                );
                None
            }
        }
    }

    // -- resolving a managed report -------------------------------------------------------------

    fn resolve_managed(
        &mut self,
        r: &RawReport,
        queries: Vec<QueryEntry>,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Option<Report> {
        let name = r.name.clone();
        let layers = folder_layers(folders, &r.folder);
        let key = |k: &str| r.keys.get(k);
        let located = |k: &str| {
            r.keys
                .get(k)
                .map(|l| (l.file.display.clone(), r.lines.get(k).copied().flatten()))
        };
        let typed = &r.typed;

        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        if let Some(t) = &typed.tags {
            match &t.value {
                Loose::Ok(l) => tags.extend(l.iter().cloned()),
                Loose::Bad(_) => {
                    let (f, l) = located("tags").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `tags` must be a list of strings"),
                    );
                }
            }
        }
        dedup(&mut tags);

        if key("profile").is_some() && key("sets").is_some() {
            let (f, l) = located("sets").unwrap();
            self.diags.error(
                "profile-and-sets",
                Some(f),
                l,
                format!("report `{name}` declares both `profile:` and `sets:`; use one or the other"),
            );
        }

        let report_profile = match &typed.profile {
            Some(p) => match &p.value {
                Loose::Ok(s) => {
                    let (f, l) = located("profile").unwrap();
                    used.connection(s, Some(f), l);
                    Some(s.to_string())
                }
                Loose::Bad(_) => {
                    let (f, l) = located("profile").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `profile` must be a string"),
                    );
                    None
                }
            },
            None => None,
        };
        let folder_profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()));
        let report_timezone = match &typed.timezone {
            Some(t) => {
                let (f, l) = located("timezone").unwrap();
                self.timezone_str(
                    t.value.ok().map(String::as_str),
                    &f,
                    l,
                    &format!("report `{name}`: `timezone`"),
                )
            }
            None => None,
        };
        let timezone = report_timezone
            .or_else(|| layers.iter().rev().find_map(|l| l.timezone.clone()))
            .or_else(|| project.timezone.clone());
        let report_locale = match &typed.locale {
            Some(t) => {
                let (f, l) = located("locale").unwrap();
                self.locale_str(
                    t.value.ok().map(String::as_str),
                    &f,
                    l,
                    &format!("report `{name}`: `locale`"),
                )
            }
            None => None,
        };
        let locale = report_locale
            .or_else(|| layers.iter().rev().find_map(|l| l.locale.clone()))
            .or_else(|| project.locale.clone());
        let profile_at = if report_profile.is_some() {
            located("profile").map(|(file, line)| ProfileAt {
                file,
                line,
                key: "`profile`".into(),
            })
        } else {
            inherited_profile_at(&layers, project)
        };
        let base_profile = report_profile
            .or(folder_profile)
            .or(project.default_profile.clone());

        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(yaml_map_to_json(v));
            }
        }
        if let Some(v) = &typed.vars {
            match &v.value {
                Loose::Ok(m) => vars.extend(m.clone()),
                Loose::Bad(_) => {
                    let (f, l) = located("vars").unwrap();
                    self.diags.error(
                        "invalid-field",
                        Some(f),
                        l,
                        format!("report `{name}`: `vars` must be a map"),
                    );
                }
            }
        }

        let mut output = builtin_output();
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                "invalid-field",
                Some(r.file.display.clone()),
                None,
                format!("report `{name}`: folder config: {p}"),
            );
        }
        let mut outputs = vec![output];
        if let Some(o) = key("output") {
            for p in layer_outputs(&mut outputs, &o.value) {
                let (f, l) = located("output").unwrap();
                self.diags
                    .error("invalid-field", Some(f), l, format!("report `{name}`: {p}"));
            }
        }

        if typed.schedule.is_some() {
            let (f, l) = located("schedule").unwrap();
            self.moved_to_schedules(&f, l, &format!("report `{name}`: `schedule`"));
        }

        let default_set = typed.default_set.as_ref().and_then(|d| d.value.ok().cloned());

        let base = BindingBase {
            profile: base_profile,
            profile_at,
            vars,
            outputs,
            locale,
        };
        let has_sets = key("sets").is_some();
        let report_base = self.silent_binding(&name, &base, &queries, &r.file.display);
        let mut bindings = Vec::new();
        if let (Some(s), Some(at)) = (&typed.sets, key("sets")) {
            bindings = self.resolve_sets(&name, s, &at.file.display, &queries, &base, project, used);
            let declared: Vec<&str> = bindings.iter().filter_map(|b| b.set.as_deref()).collect();
            if let Some(d) = &default_set
                && !declared.contains(&d.as_str())
            {
                let (f, l) = located("default_set").unwrap();
                self.diags.error(
                    "unknown-default-set",
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` `{d}` isn't one of the report's `sets:`"),
                );
            }
        } else {
            if default_set.is_some() {
                let (f, l) = located("default_set").unwrap();
                self.diags.error(
                    "unknown-default-set",
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` needs a `sets:` list"),
                );
            }
            let b = self.finish_binding(
                &name,
                None,
                &base,
                None,
                queries.clone(),
                r.file.display.clone(),
                used,
            );
            bindings.push(b);
        }

        Some(Report {
            name,
            managed: true,
            file: r.file.display.clone(),
            folder: r.folder.clone(),
            tags,
            queries,
            default_set,
            timezone,
            has_sets,
            bindings,
            base: report_base,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_sets(
        &mut self,
        report: &str,
        sets: &de::Located<Loose<Vec<SetItem>>>,
        display: &Path,
        queries: &[QueryEntry],
        base: &BindingBase,
        project: &Project,
        used: &mut Usage,
    ) -> Vec<Binding> {
        let file = Some(display.to_path_buf());
        let sets_line = sets.line();
        let Loose::Ok(items) = &sets.value else {
            self.diags.error(
                "invalid-field",
                file,
                sets_line,
                format!("report `{report}`: `sets` must be a list"),
            );
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for item in items {
            let line = item.line();
            let (name, inline): (String, Option<&SetEntry>) = match &item.value {
                Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
                Loose::Ok(de::OneOf::B(m)) => match &m.name {
                    Some(Loose::Ok(n)) => (n.clone(), Some(m)),
                    _ => {
                        self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("report `{report}`: a `sets:` map entry needs a `name:`"),
                        );
                        continue;
                    }
                },
                Loose::Bad(_) => {
                    self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("report `{report}`: `sets:` entries must be names or maps"),
                    );
                    continue;
                }
            };
            if !seen.insert(name.clone()) {
                self.diags.error(
                    "duplicate-set",
                    file.clone(),
                    line,
                    format!("report `{report}` lists Set `{name}` more than once"),
                );
                continue;
            }
            let registry = project.sets.get(&name);
            if inline.is_none() && registry.is_none() {
                self.diags.error(
                    "unknown-set",
                    file.clone(),
                    line,
                    format!("report `{report}`: Set `{name}` isn't declared in sets.yml"),
                );
                continue;
            }
            let mut b = BindingBase {
                profile: base.profile.clone(),
                profile_at: base.profile_at.clone(),
                vars: base.vars.clone(),
                outputs: base.outputs.clone(),
                locale: base.locale.clone(),
            };
            if let Some(reg) = registry {
                if let Some(p) = &reg.profile {
                    b.profile = Some(p.clone());
                    b.profile_at = Some(ProfileAt {
                        file: reg.file.clone(),
                        line: reg.line,
                        key: format!("`profile` of Set `{name}`"),
                    });
                    used.connection(p, None, None);
                }
                b.vars.extend(reg.vars.clone());
                if reg.locale.is_some() {
                    b.locale = reg.locale.clone();
                }
            }
            let mut qs = queries.to_vec();
            let mut tab_names: Option<Mapping> = None;
            if let Some(m) = inline {
                let ctx = format!("report `{report}`, Set `{name}`");
                for k in &m.unknown.0 {
                    self.diags.error(
                        "unknown-key",
                        file.clone(),
                        Some(k.line),
                        format!("{ctx}: unknown key `{}`", k.name),
                    );
                }
                if let Some(p) = &m.profile {
                    let pline = p.line();
                    match &p.value {
                        Loose::Ok(p) => {
                            b.profile = Some(p.to_string());
                            b.profile_at = Some(ProfileAt {
                                file: display.to_path_buf(),
                                line: pline,
                                key: format!("`profile` of Set `{name}`"),
                            });
                            used.connection(p, file.clone(), pline);
                        }
                        Loose::Bad(_) => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            pline,
                            format!("{ctx}: `profile` must be a string"),
                        ),
                    }
                }
                match &m.vars {
                    Some(Loose::Ok(v)) => b.vars.extend(v.clone()),
                    None => {}
                    Some(Loose::Bad(_)) => self.diags.error(
                        "invalid-field",
                        file.clone(),
                        line,
                        format!("{ctx}: `vars` must be a map"),
                    ),
                }
                if let Some(o) = &m.output {
                    let o = json_to_yaml_value(o);
                    for p in layer_outputs(&mut b.outputs, &o) {
                        self.diags
                            .error("invalid-field", file.clone(), line, format!("{ctx}: {p}"));
                    }
                }
                if m.schedule.is_some() {
                    self.moved_to_schedules(display, line, &format!("{ctx}: `schedule`"));
                }
                if let Some(l) = &m.locale
                    && let Some(l) = self.locale_str(
                        l.ok().map(String::as_str),
                        display,
                        line,
                        &format!("{ctx}: `locale`"),
                    )
                {
                    b.locale = Some(l);
                }
                let listed: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
                match (&m.exclude, &m.queries) {
                    (Some(_), Some(_)) => self.diags.error(
                        "exclude-and-queries",
                        file.clone(),
                        line,
                        format!("{ctx}: use either `exclude:` or `queries:`, not both"),
                    ),
                    (Some(ex), None) => match &ex.value {
                        Loose::Ok(names) => {
                            for q in names {
                                if !listed.contains(&q.as_str()) {
                                    self.diags.error(
                                        "unknown-query",
                                        file.clone(),
                                        ex.line(),
                                        format!("{ctx}: `exclude` names `{q}`, which isn't in the report's `queries:`"),
                                    );
                                }
                            }
                            qs.retain(|q| !names.contains(&q.query));
                        }
                        Loose::Bad(_) => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `exclude` must be a list of query names"),
                        ),
                    },
                    (None, Some(ov)) => match &ov.value {
                        Loose::Ok(items) => {
                            let mut sub = Vec::new();
                            for it in items {
                                let (qn, settings) = match it {
                                    Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
                                    Loose::Ok(de::OneOf::B(e)) => {
                                        match e.query.as_ref().and_then(Loose::ok) {
                                            Some(q) => (q.clone(), Some(e)),
                                            None => continue,
                                        }
                                    }
                                    Loose::Bad(_) => continue,
                                };
                                match queries.iter().find(|q| q.query == qn) {
                                    Some(q) => {
                                        let mut q = q.clone();
                                        if let Some(e) = settings {
                                            match &e.tab_name {
                                                None => {}
                                                Some(Loose::Ok(s)) => q.tab_name = Some(s.clone()),
                                                Some(Loose::Bad(_)) => self.diags.error(
                                                    "invalid-field",
                                                    file.clone(),
                                                    line,
                                                    format!("{ctx}: {}", one_tab_per_file(&qn)),
                                                ),
                                            }
                                            if let Some(Loose::Ok(b)) = &e.tab {
                                                q.tab = *b;
                                            }
                                        }
                                        sub.push(q);
                                    }
                                    None => self.diags.error(
                                        "unknown-query",
                                        file.clone(),
                                        ov.line(),
                                        format!("{ctx}: `queries` names `{qn}`, which isn't in the report's `queries:`"),
                                    ),
                                }
                            }
                            qs = sub;
                        }
                        Loose::Bad(_) => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `queries` must be a list"),
                        ),
                    },
                    (None, None) => {}
                }
                tab_names = match &m.tab_names {
                    None => None,
                    Some(Loose::Ok(t)) => Some(json_to_yaml(t.clone())),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            "invalid-field",
                            file.clone(),
                            line,
                            format!("{ctx}: `tab_names` must be a map of query name to tab name"),
                        );
                        None
                    }
                };
            }
            let bind = self.finish_binding(
                report,
                Some(name),
                &b,
                tab_names.as_ref(),
                qs,
                display.to_path_buf(),
                used,
            );
            out.push(bind);
        }
        out
    }

    /// Resolve a Binding without recording diagnostics or usage (they're reported elsewhere).
    fn silent_binding(
        &mut self,
        report: &str,
        base: &BindingBase,
        queries: &[QueryEntry],
        file: &Path,
    ) -> Binding {
        let saved = std::mem::take(&mut self.diags);
        let mut scratch = Usage::default();
        let b = self.finish_binding(
            report,
            None,
            base,
            None,
            queries.to_vec(),
            file.to_path_buf(),
            &mut scratch,
        );
        self.diags = saved;
        b
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_binding(
        &mut self,
        report: &str,
        set: Option<String>,
        base: &BindingBase,
        tab_names: Option<&Mapping>,
        mut queries: Vec<QueryEntry>,
        file: PathBuf,
        used: &mut Usage,
    ) -> Binding {
        let ctx = match &set {
            Some(s) => format!("report `{report}`, Set `{s}`"),
            None => format!("report `{report}`"),
        };
        if let Some(t) = tab_names {
            for (k, v) in t {
                let Some(q) = k.as_str() else { continue };
                match queries.iter_mut().find(|e| e.query == q) {
                    Some(e) => match v {
                        Value::String(s) => e.tab_name = Some(s.clone()),
                        _ => self.diags.error(
                            "invalid-field",
                            Some(file.clone()),
                            None,
                            format!("{ctx}: {}", one_tab_per_file(q)),
                        ),
                    },
                    None => self.diags.error(
                        "unknown-query",
                        Some(file.clone()),
                        None,
                        format!("{ctx}: `tab_names` names `{q}`, which isn't one of this Binding's queries"),
                    ),
                }
            }
        }
        let outputs = self.typed_outputs(&base.outputs, &ctx, &file, &queries, used);
        if let Some(p) = &base.profile {
            used.connection(p, None, None);
        }
        Binding {
            set,
            profile: base.profile.clone(),
            vars: base.vars.clone(),
            queries: std::mem::take(&mut queries),
            locale: base.locale.clone(),
            outputs,
            schedules: Vec::new(),
            parsed: None,
            profile_at: base.profile_at.clone(),
        }
    }

    /// Every merged output, typed, plus the checks across them: unique names, `queries:` naming
    /// the Binding's queries, xlsx-only `columns`, and queries that feed no output.
    fn typed_outputs(
        &mut self,
        maps: &[Mapping],
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
        used: &mut Usage,
    ) -> Vec<Output> {
        let at = Some(file.to_path_buf());
        let several = maps.len() > 1;
        let mut outputs = Vec::new();
        for (i, m) in maps.iter().enumerate() {
            let octx = if several {
                format!(
                    "{ctx}, {}",
                    output_label(m.get("name").and_then(Value::as_str), i)
                )
            } else {
                ctx.to_string()
            };
            let subset: Vec<QueryEntry> = match m.get("queries") {
                None | Some(Value::Null) => queries.to_vec(),
                Some(v) => {
                    match string_list(v) {
                        Some(names) => {
                            for n in names.iter().filter(|n| !queries.iter().any(|q| &&q.query == n)) {
                                self.diags.error(
                                "unknown-query",
                                at.clone(),
                                None,
                                format!("{octx}: `queries` names `{n}`, which isn't one of this Binding's queries"),
                            );
                            }
                            queries
                                .iter()
                                .filter(|q| names.contains(&q.query))
                                .cloned()
                                .collect()
                        }
                        None => {
                            self.diags.error(
                                "invalid-field",
                                at.clone(),
                                None,
                                format!("{octx}: `queries` must be a list of query names"),
                            );
                            queries.to_vec()
                        }
                    }
                }
            };
            outputs.push(self.typed_output(m, &octx, file, &subset, used));
        }
        let mut seen = BTreeSet::new();
        for o in &outputs {
            if let Some(n) = &o.name
                && !seen.insert(n.clone())
            {
                self.diags.error(
                    "duplicate-output",
                    at.clone(),
                    None,
                    format!("{ctx}: output `{n}` is declared twice; output names must be unique"),
                );
            }
        }
        // Two unnamed outputs of one format would write the same default file.
        let mut exts = BTreeSet::new();
        for o in outputs.iter().filter(|o| o.name.is_none()) {
            let ext = o.extension.clone().unwrap_or_else(|| o.format.clone());
            if !exts.insert(ext) {
                self.diags.error(
                    "duplicate-output",
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: two unnamed `{}` outputs would write the same file; give each output a `name:`",
                        o.format
                    ),
                );
            }
        }
        for q in queries.iter().filter(|q| !q.columns.is_empty()) {
            let formats: Vec<&str> = outputs
                .iter()
                .filter(|o| o.feeds(&q.query))
                .map(|o| o.format.as_str())
                .collect();
            if !formats.is_empty() && !formats.contains(&"xlsx") {
                self.diags.error(
                    "invalid-field",
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: `columns` on query `{}` only applies to the xlsx format",
                        q.query
                    ),
                );
            }
        }
        for (i, o) in outputs.iter().enumerate() {
            let octx = if several {
                format!("{ctx}, {}", o.label(i))
            } else {
                ctx.to_string()
            };
            self.check_output_templates(o, &outputs, &octx, file, queries);
            for d in o.destinations.iter().filter(|d| !d.attach.is_empty()) {
                for a in &d.attach {
                    let problem = match outputs.iter().find(|x| x.name.as_deref() == Some(a.as_str())) {
                        _ if !o.is_message() => Some("`attach` only applies to a message output".to_string()),
                        None => Some(format!("no output of this report is named `{a}`")),
                        Some(x) if std::ptr::eq(x, o) => Some(format!("`{a}` is this output itself")),
                        Some(x) if x.is_message() => Some(format!(
                            "`{a}` is a message; attach file outputs (its `.md` is linked with `outputs.{a}.location`)"
                        )),
                        Some(_) => None,
                    };
                    if let Some(p) = problem {
                        self.diags.error(
                            "invalid-destination-option",
                            at.clone(),
                            None,
                            format!("{octx}: destination `{}`: `attach: {a}`: {p}", d.profile),
                        );
                    }
                }
            }
        }
        for q in queries.iter().filter(|q| q.tab) {
            if !outputs.iter().any(|o| o.feeds(&q.query)) {
                self.diags.warning(
                    "unused-query",
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: query `{}` feeds no output; add it to an output's `queries:`, or give it `tab: false` if it only prepares later queries",
                        q.query
                    ),
                );
            }
        }
        outputs
    }

    /// A message output's options, and the templates of any output (`text`, `file`, `title`,
    /// `when`): they compile, `results.<x>` names one of the output's queries and `outputs.<x>`
    /// another output. Only attribute access is checked; dynamic access isn't seen.
    fn check_output_templates(
        &mut self,
        o: &Output,
        all: &[Output],
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
    ) {
        let at = Some(file.to_path_buf());
        let err = |s: &mut Self, msg: String| {
            s.diags
                .error("invalid-output-option", at.clone(), None, format!("{ctx}: {msg}"))
        };
        // (what, source) of every template to check.
        let mut templates: Vec<(String, String)> = Vec::new();
        if let Some(w) = &o.when {
            templates.push(("`when`".into(), format!("{{% if {w} %}}{{% endif %}}")));
        }
        if o.is_message() {
            for k in o.options.keys() {
                if !crate::message::MESSAGE_KEYS.contains(&k.as_str()) {
                    err(
                        self,
                        format!(
                            "`{k}` isn't an option of the `message` format; its options are {}",
                            crate::message::MESSAGE_KEYS.join(", ")
                        ),
                    );
                }
            }
            if o.options.contains_key("text") && o.options.contains_key("file") {
                err(self, "use either `text:` or `file:`, not both".into());
            }
            for k in ["text", "title"] {
                match o.options.get(k) {
                    None => {}
                    Some(Json::String(t)) => templates.push((format!("`{k}`"), t.clone())),
                    Some(_) => err(self, format!("`{k}` must be a string")),
                }
            }
            match o.options.get("file") {
                None => {}
                Some(Json::String(f)) => match find_template(&self.root, f).map(std::fs::read_to_string) {
                    Some(Ok(t)) => templates.push((format!("`file` {f}"), t)),
                    Some(Err(e)) => err(self, format!("can't read message file `{f}`: {e}")),
                    None => err(
                        self,
                        format!(
                            "message file `{f}` doesn't exist (looked in the project root and templates/)"
                        ),
                    ),
                },
                Some(_) => err(self, "`file` must be a path (a string)".into()),
            }
            match o.options.get("max_rows") {
                None => {}
                Some(v) if v.as_u64().is_some_and(|n| n > 0) => {}
                Some(_) => err(self, "`max_rows` must be a positive whole number".into()),
            }
        }
        let fed: Vec<&str> = queries
            .iter()
            .filter(|q| q.tab && o.feeds(&q.query))
            .map(|q| q.query.as_str())
            .collect();
        for (what, src) in &templates {
            if let Err((line, msg)) = preflight::check_syntax("output", src) {
                let line = line.map(|l| format!(" (line {l})")).unwrap_or_default();
                err(self, format!("{what} doesn't compile{line}: {msg}"));
                continue;
            }
            for (name, _) in preflight::attributes(src, "results") {
                if !fed.contains(&name.as_str()) {
                    err(
                        self,
                        format!(
                            "{what} reads `results.{name}`, but `{name}` isn't one of this output's queries ({})",
                            if fed.is_empty() {
                                "none".to_string()
                            } else {
                                fed.join(", ")
                            }
                        ),
                    );
                }
            }
            if !o.is_message() && !preflight::attributes(src, "outputs").is_empty() {
                err(
                    self,
                    format!(
                        "{what} reads `outputs.*`, but a file output's `when` is decided before anything is delivered; use `outputs.*` in a message output"
                    ),
                );
                continue;
            }
            for (name, _) in preflight::attributes(src, "outputs") {
                let other = all.iter().find(|x| x.name.as_deref() == Some(name.as_str()));
                match other {
                    None => err(
                        self,
                        format!(
                            "{what} reads `outputs.{name}`, but no output of this report is named `{name}`"
                        ),
                    ),
                    Some(x) if std::ptr::eq(x, o) => err(
                        self,
                        format!("{what} reads `outputs.{name}`, which is this output itself"),
                    ),
                    Some(_) => {}
                }
            }
        }
    }

    /// Convert a merged output map into a typed `Output`, validating options and references.
    fn typed_output(
        &mut self,
        m: &Mapping,
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
        used: &mut Usage,
    ) -> Output {
        let file_path = file.to_path_buf();
        let file = Some(file_path.clone());
        let node = crate::yaml::to_node(&Value::Mapping(m.clone()));
        let o: OutputConfig = match de::from_node(&node) {
            Ok(o) => o,
            // Every key is `Loose`, so a map always reads.
            Err(e) => unreachable!("an output map reads as an output: {e}"),
        };
        let format = o
            .format
            .as_ref()
            .and_then(Loose::ok)
            .cloned()
            .unwrap_or_else(|| "csv".to_string());
        // `message` is built in; every other format is a plugin.
        if format != MESSAGE_FORMAT {
            used.format(&format, ctx, &file_path);
        }
        let mut opts: JsonMap<String, Json> = o.options.0.into_iter().map(|(k, v)| (k.value, v)).collect();
        // The project's defaults for this format sit under whatever the layers set.
        if let Some(Value::Mapping(d)) = self.format_options.get(format.as_str()) {
            for (k, v) in d {
                if let Some(k) = k.as_str()
                    && !OUTPUT_SHARED_KEYS.contains(&k)
                {
                    opts.entry(k.to_string()).or_insert_with(|| yaml_to_json(v));
                }
            }
        }
        let destinations = match o.destination {
            None => Vec::new(),
            Some(Loose::Ok(de::OneOf::A(d))) => {
                self.typed_destination(d, ctx, &file, used).into_iter().collect()
            }
            Some(Loose::Ok(de::OneOf::B(list))) if list.is_empty() => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!(
                        "{ctx}: `output.destination` is an empty list; name at least one destination, or remove it to keep the output in target/"
                    ),
                );
                Vec::new()
            }
            Some(Loose::Ok(de::OneOf::B(list))) => {
                let mut out = Vec::new();
                for (i, d) in list.into_iter().enumerate() {
                    match d {
                        Loose::Ok(d) => out.extend(self.typed_destination(d, ctx, &file, used)),
                        Loose::Bad(_) => self.diags.error(
                            "invalid-field",
                            file.clone(),
                            None,
                            format!("{ctx}: `output.destination` entry {} must be a map", i + 1),
                        ),
                    }
                }
                out
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: `output.destination` must be a map or a list of maps"),
                );
                Vec::new()
            }
        };
        let template = match o.template {
            None => None,
            Some(t) => {
                if format != "xlsx" {
                    self.diags.error(
                        "invalid-output-option",
                        file.clone(),
                        None,
                        format!("{ctx}: `output.template` only applies to the xlsx format"),
                    );
                }
                self.typed_template(t, ctx, &file, queries)
            }
        };
        let extension = match o.extension {
            de::Maybe::Absent => None,
            de::Maybe::Given(Loose::Ok(None | Some(de::OneOf::B(false)))) => Some(String::new()),
            de::Maybe::Given(Loose::Ok(Some(de::OneOf::A(e)))) => {
                let e = e.strip_prefix('.').unwrap_or(&e).to_string();
                if e.contains(['/', '\\']) || e.chars().any(char::is_whitespace) {
                    self.diags.error(
                        "invalid-output-option",
                        file.clone(),
                        None,
                        format!(
                            "{ctx}: `extension` must be a file extension like `aba` (no path, no spaces)"
                        ),
                    );
                }
                Some(e)
            }
            de::Maybe::Given(_) => {
                self.diags.error(
                    "invalid-output-option",
                    file.clone(),
                    None,
                    format!("{ctx}: `extension` must be a string (`aba`), or `\"\"`/`false` for none"),
                );
                None
            }
        };
        let name = match o.name {
            None => None,
            Some(Loose::Ok(n)) if is_identifier(&n) => Some(n),
            Some(_) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: output `name` must be an identifier (letters, digits, `_`)"),
                );
                None
            }
        };
        let when = match o.when {
            None => None,
            Some(Loose::Ok(de::OneOf::A(w))) => Some(w),
            Some(Loose::Ok(de::OneOf::B(b))) => Some(b.to_string()),
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: `when` must be a Jinja expression (a string)"),
                );
                None
            }
        };
        let queries = match &o.queries {
            Some(Loose::Ok(_)) => Some(queries.iter().map(|q| q.query.clone()).collect()),
            _ => None,
        };
        if extension.is_some() && format == "xlsx" {
            self.diags.error(
                "invalid-output-option",
                file.clone(),
                None,
                format!("{ctx}: `extension` doesn't apply to xlsx (Excel only opens .xlsx workbooks)"),
            );
        }
        Output {
            name,
            format,
            queries,
            when,
            options: opts,
            destinations,
            template,
            extension,
        }
    }

    /// One `output.destination` entry: `profile`, optional `path`, and plugin options.
    fn typed_destination(
        &mut self,
        d: config::report::Destination,
        ctx: &str,
        file: &Option<PathBuf>,
        used: &mut Usage,
    ) -> Option<Destination> {
        let Some(Loose::Ok(p)) = d.profile else {
            self.diags.error(
                "invalid-field",
                file.clone(),
                None,
                format!("{ctx}: `output.destination` needs a `profile:` naming a profiles.yml entry"),
            );
            return None;
        };
        let path = match d.path {
            None => None,
            Some(Loose::Ok(s)) => Some(s),
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: destination `{p}`: `path` must be a string"),
                );
                None
            }
        };
        used.destination(&p, file.clone(), None);
        let options = d.options.0.into_iter().map(|(k, v)| (k.value, v)).collect();
        let attach = match d.attach {
            None => Vec::new(),
            Some(Loose::Ok(de::OneOf::A(s))) => vec![s],
            Some(Loose::Ok(de::OneOf::B(list))) => list,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    "invalid-field",
                    file.clone(),
                    None,
                    format!("{ctx}: destination `{p}`: `attach` must be an output name or a list of them"),
                );
                Vec::new()
            }
        };
        Some(Destination {
            profile: p,
            path,
            options,
            attach,
        })
    }

    fn typed_template(
        &mut self,
        t: Loose<config::report::Template>,
        ctx: &str,
        file: &Option<PathBuf>,
        queries: &[QueryEntry],
    ) -> Option<Template> {
        let err = |s: &mut Self, msg: String| {
            s.diags
                .error("invalid-template", file.clone(), None, format!("{ctx}: {msg}"))
        };
        let Some(Loose::Ok(tf)) = t.ok().and_then(|t| t.file.clone()) else {
            err(self, "`output.template` needs a `file:`".into());
            return None;
        };
        let Loose::Ok(t) = t else {
            unreachable!("checked above")
        };
        let mut bindings = Vec::new();
        let items = match t.bindings {
            None => Vec::new(),
            Some(Loose::Ok(s)) => s,
            Some(Loose::Bad(_)) => {
                err(self, "`output.template.bindings` must be a list".into());
                Vec::new()
            }
        };
        let names: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
        for (i, b) in items.into_iter().enumerate() {
            let n = i + 1;
            let Loose::Ok(m) = b else {
                err(self, format!("template binding {n} must be a map"));
                continue;
            };
            let s = |v: &Option<Loose<String>>| v.as_ref().and_then(Loose::ok).cloned();
            for k in &m.unknown.0 {
                err(self, format!("template binding {n} has unknown key `{}`", k.name));
            }
            let Some(sheet) = s(&m.sheet) else {
                err(self, format!("template binding {n} needs a `sheet:`"));
                continue;
            };
            let tb = TemplateBinding {
                sheet,
                query: s(&m.query),
                result_index: m.result_index.as_ref().and_then(Loose::ok).map(|x| *x as usize),
                anchor: s(&m.anchor),
                header: m.header.as_ref().and_then(Loose::ok).copied(),
                columns: m.columns.as_ref().and_then(Loose::ok).cloned(),
                cell: s(&m.cell),
                value: s(&m.value),
                column: s(&m.column),
            };
            if let Some(q) = &tb.query
                && !names.contains(&q.as_str())
            {
                err(
                    self,
                    format!(
                        "template binding {n} uses query `{q}`, which isn't one of this Binding's queries"
                    ),
                );
            }
            if let Some(ri) = &m.result_index
                && ri.ok().is_none_or(|x| *x == 0)
            {
                err(
                    self,
                    format!("template binding {n}: `result_index` must be 1 or more"),
                );
            }
            match &tb.cell {
                Some(c) => {
                    if !options::is_cell(c) {
                        err(
                            self,
                            format!("template binding {n}: `cell` `{c}` isn't a valid cell reference"),
                        );
                    }
                    let by_value = tb.value.is_some();
                    let by_query = tb.query.is_some() && tb.column.is_some();
                    if by_value == by_query || (tb.query.is_some() != tb.column.is_some()) {
                        err(
                            self,
                            format!(
                                "template binding {n}: a single-cell binding needs exactly one of `value` or `query` + `column`"
                            ),
                        );
                    }
                    if tb.anchor.is_some() || tb.columns.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `anchor`/`columns` apply to table blocks, not single cells"
                            ),
                        );
                    }
                }
                None => {
                    if tb.query.is_none() {
                        err(
                            self,
                            format!(
                                "template binding {n}: a table block needs a `query` (or use `cell` for a single cell)"
                            ),
                        );
                    }
                    if let Some(a) = &tb.anchor
                        && !options::is_cell(a)
                    {
                        err(
                            self,
                            format!("template binding {n}: `anchor` `{a}` isn't a valid cell reference"),
                        );
                    }
                    if tb.value.is_some() || tb.column.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `value`/`column` apply to single-cell bindings (add `cell`)"
                            ),
                        );
                    }
                }
            }
            bindings.push(tb);
        }
        Some(Template { file: tf, bindings })
    }

    // -- unmanaged reports --------------------------------------------------------------------

    fn resolve_unmanaged(
        &mut self,
        name: &str,
        path: &Path,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Report {
        let folder = folder_segments(path.parent().unwrap_or(Path::new("")));
        let layers = folder_layers(folders, &folder);
        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        dedup(&mut tags);
        let profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()))
            .or(project.default_profile.clone());
        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(yaml_map_to_json(v));
            }
        }
        let mut output = builtin_output();
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                "invalid-field",
                Some(path.to_path_buf()),
                None,
                format!("folder config: {p}"),
            );
        }
        self.diags.warning(
            "unmanaged-report",
            Some(path.to_path_buf()),
            None,
            format!(
                "`{name}` is an unmanaged report (no YAML lists it in `queries:`); unmanaged reports are for quick tests — add a YAML to make it a managed report"
            ),
        );
        self.check_unmanaged_sql(name, path);
        let query = QueryEntry {
            query: name.to_string(),
            path: path.to_path_buf(),
            profile: None,
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
            columns: BTreeMap::new(),
        };
        let base = BindingBase {
            profile: profile.clone(),
            profile_at: inherited_profile_at(&layers, project),
            vars,
            outputs: vec![output],
            locale: layers
                .iter()
                .rev()
                .find_map(|l| l.locale.clone())
                .or_else(|| project.locale.clone()),
        };
        if let Some(p) = &profile {
            used.connection(p, None, None);
        }
        let b = self.finish_binding(
            name,
            None,
            &base,
            None,
            vec![query.clone()],
            path.to_path_buf(),
            used,
        );
        Report {
            name: name.to_string(),
            managed: false,
            file: path.to_path_buf(),
            folder,
            tags,
            queries: vec![query],
            default_set: None,
            timezone: layers
                .iter()
                .rev()
                .find_map(|l| l.timezone.clone())
                .or_else(|| project.timezone.clone()),
            has_sets: false,
            base: b.clone(),
            bindings: vec![b],
        }
    }

    /// Best-effort check on the raw text; the run path re-checks the rendered SQL.
    fn check_unmanaged_sql(&mut self, name: &str, path: &Path) {
        let Ok(text) = std::fs::read_to_string(self.root.join(path)) else {
            return;
        };
        let head = sqlsplit::strip_leading_comments(&text);
        if head.starts_with("{{") || head.starts_with("{%") || head.starts_with("{#") {
            return;
        }
        for st in sqlsplit::split(&text) {
            if !sqlsplit::classify(&st.text).is_read_only_safe() {
                let body = sqlsplit::strip_leading_comments(&st.text);
                let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
                self.diags.error(
                    "unmanaged-side-effect",
                    Some(path.to_path_buf()),
                    Some(line),
                    format!(
                        "unmanaged report `{name}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; rewrite the statement, or give the report a YAML to declare it",
                        dre_protocol::util::summarize(body, 60)
                    ),
                );
            }
        }
    }

    // -- schedules ----------------------------------------------------------------------------

    /// Schedules live only in schedules.yml (named, with vars); anywhere else is an error.
    fn moved_to_schedules(&mut self, file: &Path, line: Option<usize>, ctx: &str) {
        self.diags.error(
            "schedule-moved",
            Some(file.to_path_buf()),
            line,
            format!(
                "{ctx} is no longer supported; declare schedules in schedules.yml as named entries (`name`, `report:`/`select:`, `cron`/`every`/`rrule`, optional `vars`)"
            ),
        );
    }

    /// timings.yml: named timings. Returns the valid ones, and the names of those with errors
    /// (schedules using them aren't reported again).
    fn parse_timings(&mut self, files: &[Rc<YamlFile>]) -> (BTreeMap<String, Timing>, BTreeSet<String>) {
        let mut out = BTreeMap::new();
        let mut broken = BTreeSet::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(timings)) = de::from_node::<Loose<config::schedule::TimingsFile>>(&yf.node)
            else {
                continue;
            };
            for (k, v) in timings.0.0 {
                let file = Some(yf.display.clone());
                let line = k.line();
                let name = k.value;
                let mut ok = true;
                if !is_identifier(&name) {
                    self.diags.error(
                        "invalid-timing",
                        file.clone(),
                        line,
                        format!(
                            "timing name `{name}` must be letters, digits and `_`, not starting with a digit"
                        ),
                    );
                    ok = false;
                }
                if let Some(prev) = seen.get(&name) {
                    self.diags.error(
                        "duplicate-timing-name",
                        file.clone(),
                        line,
                        format!("timing `{name}` is already declared at {prev}; timing names must be unique"),
                    );
                    continue;
                }
                seen.insert(
                    name.clone(),
                    format!("{}:{}", yf.display.display(), line.unwrap_or(0)),
                );
                let Loose::Ok(t) = v else {
                    self.diags.error(
                        "invalid-timing",
                        file.clone(),
                        line,
                        format!("timing `{name}` must be a map, e.g. `{{cron: \"0 6 1 * *\", timezone: Australia/Sydney}}`"),
                    );
                    broken.insert(name);
                    continue;
                };
                for k in &t.unknown.0 {
                    self.diags.error(
                        "invalid-timing",
                        file.clone(),
                        line,
                        format!("timing `{name}`: unknown key `{}`", k.name),
                    );
                    ok = false;
                }
                let block = t.block();
                let shape = schedule::validate_block(
                    &json_to_yaml(block.clone()),
                    "a timing",
                    "`cron`, `every` or `rrule`",
                );
                for e in &shape {
                    self.diags.error(
                        "invalid-timing",
                        file.clone(),
                        line,
                        format!("timing `{name}`: {e}"),
                    );
                    ok = false;
                }
                let timezone = match &t.timezone {
                    None => None,
                    Some(v) => {
                        let tz = self.timezone_str(
                            v.ok().map(String::as_str),
                            &yf.display,
                            line,
                            &format!("timing `{name}`: `timezone`"),
                        );
                        ok &= tz.is_some();
                        tz
                    }
                };
                if shape.is_empty() {
                    for (code, msg) in schedule::strictness(&block) {
                        self.diags
                            .error(code, file.clone(), line, format!("timing `{name}`: {msg}"));
                    }
                    if let Some(msg) = schedule::no_time(&block) {
                        self.diags.warning(
                            "schedule-no-time",
                            file.clone(),
                            line,
                            format!("timing `{name}`: {msg}"),
                        );
                    }
                }
                if ok {
                    out.insert(
                        name,
                        Timing {
                            schedule: block,
                            timezone,
                            location: (yf.display.clone(), line),
                        },
                    );
                } else {
                    broken.insert(name);
                }
            }
        }
        (out, broken)
    }

    fn parse_schedules(
        &mut self,
        files: &[Rc<YamlFile>],
        project: &Project,
        broken_timings: &BTreeSet<String>,
    ) -> Vec<ScheduleEntry> {
        let mut out = Vec::new();
        let mut used_timings = BTreeSet::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(items)) = de::from_node::<Loose<config::schedule::SchedulesFile>>(&yf.node)
            else {
                continue;
            };
            for item in &items.0 {
                // Schedules files are recognised by every item being a map with a name or target.
                let Loose::Ok(m) = &item.value else { continue };
                let line = item.line();
                let file = Some(yf.display.clone());
                let s = |v: &Option<Loose<String>>| v.as_ref().and_then(Loose::ok).cloned();
                let (select, report, set) = (s(&m.select), s(&m.report), s(&m.set));
                let mut ok = true;
                let name = match &m.name {
                    Some(Loose::Ok(n)) if is_identifier(n) => {
                        if let Some(prev) = seen.get(n) {
                            self.diags.error(
                                "duplicate-schedule-name",
                                file.clone(),
                                line,
                                format!("schedule `{n}` is already declared at {prev}; schedule names must be unique"),
                            );
                            ok = false;
                        }
                        seen.insert(
                            n.clone(),
                            format!("{}:{}", yf.display.display(), line.unwrap_or(0)),
                        );
                        n.clone()
                    }
                    Some(Loose::Ok(n)) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("schedule name `{n}` must be letters, digits and `_`, not starting with a digit"),
                        );
                        ok = false;
                        n.clone()
                    }
                    _ => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            "every schedule needs a `name`",
                        );
                        ok = false;
                        String::new()
                    }
                };
                let vars = match &m.vars {
                    None => JsonMap::new(),
                    Some(Loose::Ok(v)) => v.clone(),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("schedule `{name}`: `vars` must be a map"),
                        );
                        ok = false;
                        JsonMap::new()
                    }
                };
                for k in &m.unknown.0 {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        format!("unknown schedule key `{}`", k.name),
                    );
                }
                let sched = m.block();
                let timing = match &m.timing {
                    None => None,
                    Some(Loose::Ok(t)) => {
                        used_timings.insert(t.clone());
                        Some(t.clone())
                    }
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!(
                                "schedule `{name}`: `timing` must be the name of a timing in timings.yml"
                            ),
                        );
                        ok = false;
                        None
                    }
                };
                let mut resolved = None;
                let shape = if let Some(t) = &timing {
                    let mut errs = Vec::new();
                    let own: Vec<String> = m.timing_keys().into_iter().map(|k| format!("`{k}`")).collect();
                    if !own.is_empty() {
                        let keys = match own.split_last() {
                            Some((last, [])) => last.clone(),
                            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                            None => String::new(),
                        };
                        errs.push(format!(
                            "schedule `{name}` uses timing `{t}`, so it can't set {keys} too; change the timing, or give the schedule its own timing instead"
                        ));
                    }
                    match project.timings.get(t) {
                        Some(def) => resolved = Some(def.schedule.clone()),
                        None if broken_timings.contains(t) => ok = false,
                        None => {
                            let names: Vec<&str> = project.timings.keys().map(String::as_str).collect();
                            let valid = if names.is_empty() {
                                "the project has no timings.yml entries".to_string()
                            } else {
                                format!("valid names: {}", names.join(", "))
                            };
                            self.diags.error(
                                "unknown-timing",
                                file.clone(),
                                line,
                                format!("schedule `{name}`: no timing `{t}`; {valid}"),
                            );
                            ok = false;
                        }
                    }
                    errs
                } else {
                    schedule::validate_block(
                        &json_to_yaml(sched.clone()),
                        "a schedule",
                        "`timing`, `cron`, `every` or `rrule`",
                    )
                };
                if shape.is_empty() && timing.is_none() {
                    let block = &sched;
                    for (code, msg) in schedule::strictness(block) {
                        self.diags
                            .error(code, file.clone(), line, format!("schedule `{name}`: {msg}"));
                    }
                    if let Some(msg) = schedule::no_time(block) {
                        self.diags.warning(
                            "schedule-no-time",
                            file.clone(),
                            line,
                            format!("schedule `{name}`: {msg}"),
                        );
                    }
                }
                for e in shape {
                    self.diags.error("invalid-schedule", file.clone(), line, e);
                    ok = false;
                }
                if select.is_some() && report.is_some() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        "use either `select:` or `report:`, not both",
                    );
                    ok = false;
                }
                if select.is_some() && set.is_some() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        "`set:` only applies with `report:`",
                    );
                    ok = false;
                }
                if let Some(sel) = &select {
                    match selector::resolve(project, sel) {
                        Ok(r) if r.is_empty() => {
                            self.diags.error(
                                "selector-matches-nothing",
                                file.clone(),
                                line,
                                format!("selector `{sel}` matches no report"),
                            );
                            ok = false;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            self.diags.error(e.code(), file.clone(), line, e.to_string());
                            ok = false;
                        }
                    }
                }
                if let Some(rn) = &report {
                    match project.report(rn) {
                        None => {
                            self.diags.error(
                                "selector-matches-nothing",
                                file.clone(),
                                line,
                                format!("report `{rn}` doesn't exist"),
                            );
                            ok = false;
                        }
                        Some(r) => {
                            if let Some(sn) = &set
                                && r.binding(sn).is_none()
                            {
                                self.diags.error(
                                    "selector-matches-nothing",
                                    file.clone(),
                                    line,
                                    format!("report `{rn}` has no Set `{sn}`"),
                                );
                                ok = false;
                            }
                        }
                    }
                }
                if select.is_none() && report.is_none() {
                    self.diags.error(
                        "invalid-schedule",
                        file.clone(),
                        line,
                        format!("schedule `{name}` needs `report:` (optionally with `set:`) or `select:`"),
                    );
                    ok = false;
                }
                let timezone = match (&m.timezone, &timing) {
                    (None, Some(t)) => project.timings.get(t).and_then(|d| d.timezone.clone()),
                    (None, None) => None,
                    (Some(v), _) => {
                        let t = self.timezone_str(
                            v.value.ok().map(String::as_str),
                            &yf.display,
                            line,
                            &format!("schedule `{name}`: `timezone`"),
                        );
                        ok &= t.is_some();
                        t
                    }
                };
                let enabled = match &m.enabled {
                    None => true,
                    Some(Loose::Ok(b)) => *b,
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            "invalid-schedule",
                            file.clone(),
                            line,
                            format!("schedule `{name}`: `enabled` must be true or false"),
                        );
                        ok = false;
                        true
                    }
                };
                if ok {
                    out.push(ScheduleEntry {
                        name,
                        select,
                        report,
                        set,
                        schedule: resolved.unwrap_or(sched),
                        timing,
                        enabled,
                        vars,
                        timezone,
                        location: (yf.display.clone(), line),
                    });
                }
            }
        }
        for (name, t) in &project.timings {
            if !used_timings.contains(name) {
                self.diags.warning(
                    "unused-timing",
                    Some(t.location.0.clone()),
                    t.location.1,
                    format!("timing `{name}` isn't used by any schedule"),
                );
            }
        }
        out
    }

    /// Warn when two schedules of one Binding would deliver to the same path: each schedule's
    /// paths are rendered with its vars and one fixed date. Paths that can't render offline (a
    /// `run_query()`, a missing `env_var()`) are skipped.
    fn check_schedule_paths(&mut self, project: &Project) {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for report in &project.reports {
            for b in report.bindings.iter().filter(|b| b.schedules.len() > 1) {
                let paths: Vec<&String> = b.destinations().filter_map(|d| d.path.as_ref()).collect();
                if paths.is_empty() {
                    continue;
                }
                let mut rendered: Vec<(&String, Vec<String>)> = Vec::new();
                for name in &b.schedules {
                    let Some(e) = project.schedules.iter().find(|e| &e.name == name) else {
                        continue;
                    };
                    let mut vars = b.vars.clone();
                    vars.extend(e.vars.clone());
                    let Ok(r) = crate::render::Renderer::new(crate::render::RendererConfig {
                        root: &project.root,
                        macros: &project.macros,
                        context: crate::render::RunContext {
                            report: report.name.clone(),
                            set: b.set.clone(),
                            target: project.target_name.clone(),
                            schedule: Some(name.clone()),
                            date,
                            now: chrono::Utc::now(),
                            scheduled_at: None,
                            calendar: crate::dates::Calendar::default(),
                            locale: Default::default(),
                        },
                        vars,
                        cli_vars: self.opts.vars.clone(),
                        runner: None,
                        connections: None,
                        mode: crate::render::Mode::Run,
                        connection: None,
                        source_type: String::new(),
                        sources: None,
                        run_query_max_rows: project.run_query_max_rows,
                        sql: project.sql.clone(),
                        lookups: project.lookups.clone(),
                        lookup_inline_max_rows: project.lookup_inline_max_rows,
                        packages: project.packages.clone(),
                        project_name: project.name.clone(),
                        dispatch: project.dispatch.clone(),
                    }) else {
                        continue;
                    };
                    let out: Result<Vec<String>, _> =
                        paths.iter().map(|p| r.render(&report.file, p)).collect();
                    if let Ok(out) = out {
                        rendered.push((name, out));
                    }
                }
                for (i, (a, pa)) in rendered.iter().enumerate() {
                    for (bn, pb) in &rendered[i + 1..] {
                        if let Some(same) = pa.iter().find(|p| pb.contains(p)) {
                            self.diags.warning(
                                "schedule-path-clash",
                                Some(report.file.clone()),
                                None,
                                format!(
                                    "schedules `{a}` and `{bn}` both run report `{}`{} and deliver to `{same}`; the second overwrites the first — put a schedule var or `run.schedule` in the path",
                                    report.name,
                                    b.set.as_ref().map(|s| format!(", Set `{s}`,")).unwrap_or_default()
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Attach every schedules.yml entry to the Bindings it targets: `report:` + `set:` one
    /// Binding, `report:` alone all of that report's Bindings, `select:` all Bindings of every
    /// report it matches. Several schedules per Binding is the point, not a conflict.
    fn apply_schedules(&mut self, project: &mut Project) {
        let entries = project.schedules.clone();
        for e in &entries {
            let reports: Vec<String> = match (&e.select, &e.report) {
                (Some(sel), _) => selector::resolve(project, sel)
                    .map(|r| r.into_iter().map(|r| r.name.clone()).collect())
                    .unwrap_or_default(),
                (_, Some(r)) => vec![r.clone()],
                _ => Vec::new(),
            };
            for report in project.reports.iter_mut().filter(|r| reports.contains(&r.name)) {
                for b in &mut report.bindings {
                    if e.set.is_none() || e.set == b.set {
                        b.schedules.push(e.name.clone());
                    }
                }
            }
        }
        self.check_schedule_paths(project);
        self.check_schedule_timezones(project);
    }

    /// Warn when a schedule fires in one timezone and a report it runs renders in another: the
    /// run date is then the report's date at the firing time, which may not be the day meant.
    fn check_schedule_timezones(&mut self, project: &Project) {
        for e in project.schedules.iter().filter(|e| e.timezone.is_none()) {
            let fires = crate::occurrences::firing_tz(project, e);
            let mut seen = BTreeSet::new();
            for (report, _) in crate::occurrences::bindings(project, &e.name) {
                let Some(r) = project.report(report) else { continue };
                let renders = r
                    .timezone
                    .as_ref()
                    .and_then(|t| crate::dates::parse_tz(t).ok())
                    .unwrap_or(chrono_tz::Tz::UTC);
                if renders != fires && seen.insert(report) {
                    self.diags.warning(
                        "schedule-timezone-mismatch",
                        Some(e.location.0.clone()),
                        e.location.1,
                        format!(
                            "schedule `{}` fires in {} but report `{report}` renders in {}, so its run date is {}'s date at the firing time; set `timezone:` on the schedule to fire and render in one timezone",
                            e.name,
                            fires.name(),
                            renders.name(),
                            renders.name()
                        ),
                    );
                }
            }
        }
    }

    // -- profiles and plugins -----------------------------------------------------------------

    fn check_profiles(&mut self, project: &Project, used: &Usage) {
        let profiles = &project.profiles;
        if used.connections.is_empty() && used.destinations.is_empty() {
            return;
        }
        if !profiles.exists() {
            self.diags.error(
                "profiles-missing",
                None,
                None,
                if profiles.found_by == "~/.dre" {
                    format!(
                        "the project references profiles, but no profiles.yml was found in the project directory or at {}",
                        profiles.path.display()
                    )
                } else {
                    format!(
                        "the project references profiles, but no profiles.yml was found at {} (from {})",
                        profiles.path.display(),
                        profiles.found_by
                    )
                },
            );
            return;
        }
        for (role, refs) in [
            (Role::Connection, &used.connections),
            (Role::Destination, &used.destinations),
        ] {
            for (name, (file, line)) in refs {
                if role == Role::Destination && name == LOCAL_TYPE {
                    continue;
                }
                if !profiles.declares(role, name) {
                    self.diags.error(
                        "unknown-profile",
                        file.clone(),
                        *line,
                        format!(
                            "{} profile `{name}` isn't defined under `{}:` in {}",
                            role.as_str(),
                            profiles.section_key(role),
                            profiles.path.display()
                        ),
                    );
                }
            }
        }
    }

    /// A `plugins:` entry in its map form: `{name: foo, github: acme/dre-foo, version: "^1"}`.
    fn plugin_entry(
        &mut self,
        m: &config::dependencies::PluginEntry,
        file: &Path,
        line: Option<usize>,
    ) -> Option<(String, Option<Value>, PluginSource)> {
        let file = Some(file.to_path_buf());
        let name = m.name.ok().cloned().unwrap_or_default();
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                "invalid-plugin-declaration",
                file.clone(),
                line,
                format!("plugin package `{name}`: {msg}"),
            );
        };
        if name.is_empty() {
            err(self, "`name` must be a non-empty string".into());
            return None;
        }
        if let Some(k) = m.unknown.0.first() {
            err(
                self,
                format!(
                    "unknown key `{}`; use `name`, `version` and one of `github`, `local`, `registry`",
                    k.name
                ),
            );
            return None;
        }
        let given: Vec<(&str, &str)> = [
            ("github", &m.github),
            ("local", &m.local),
            ("registry", &m.registry),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k, v.as_str().unwrap_or(""))))
        .collect();
        let source = match given.as_slice() {
            [] => PluginSource::Default,
            [(k, "")] => {
                err(self, format!("`{k}` must be a non-empty string"));
                return None;
            }
            [("github", r)] => {
                let ok = r.split('/').count() == 2 && r.split('/').all(|p| !p.is_empty());
                if !ok {
                    err(self, format!("`github: {r}` must be `owner/repo`"));
                    return None;
                }
                PluginSource::Github(r.to_string())
            }
            [("local", p)] => {
                if m.version.is_some() {
                    err(
                        self,
                        "a `local` package has no `version`: it's used as it is".into(),
                    );
                    return None;
                }
                PluginSource::Local(p.to_string())
            }
            [(_, u)] => PluginSource::Registry(u.to_string()),
            _ => {
                err(self, "give only one of `github`, `local` and `registry`".into());
                return None;
            }
        };
        Some((name, m.version.as_ref().map(json_to_yaml_value), source))
    }

    /// The `plugins:` declarations, merged across files; and every plugin the project uses, for
    /// [`crate::plugins::check_uses`] to check against what the declared packages provide.
    fn check_plugins(&mut self, decls: &[(Rc<YamlFile>, Mapping)], project: &mut Project, used: &Usage) {
        struct Decl {
            req: semver::VersionReq,
            raw: String,
            file: PathBuf,
            source: PluginSource,
        }
        let mut by_package: BTreeMap<String, Vec<Decl>> = BTreeMap::new();
        for (yf, _) in decls {
            {
                let Ok(config::dependencies::PluginsKey { plugins: Some(v) }) = de::from_node(&yf.node)
                else {
                    continue;
                };
                let line = v.line();
                let file = Some(yf.display.clone());
                let entries: Vec<(String, Option<Value>, PluginSource, Option<usize>)> = match v.value {
                    Loose::Ok(de::OneOf::A(items)) => items
                        .into_iter()
                        .filter_map(|i| {
                            let iline = i.line();
                            match i.value {
                                Loose::Ok(de::OneOf::A(name)) => Some((name.0, None, PluginSource::Default, iline)),
                                Loose::Ok(de::OneOf::B(de::OneOf::B(entry))) => self
                                    .plugin_entry(&entry, &yf.display, iline)
                                    .map(|(n, c, s)| (n, c, s, iline)),
                                Loose::Ok(de::OneOf::B(de::OneOf::A(pin))) => Some((
                                    pin.name.value,
                                    Some(json_to_yaml_value(&pin.version)),
                                    PluginSource::Default,
                                    iline,
                                )),
                                Loose::Bad(_) => {
                                    self.diags.error(
                                        "invalid-plugin-declaration",
                                        file.clone(),
                                        iline,
                                        "each `plugins` entry is a package name, `name: \"<version>\"`, or a map with `name:` and one of `github:`, `local:`, `registry:`",
                                    );
                                    None
                                }
                            }
                        })
                        .collect(),
                    Loose::Ok(de::OneOf::B(m)) => m
                        .0
                        .into_iter()
                        .map(|(k, v)| {
                            let kline = k.line();
                            (k.value, Some(json_to_yaml_value(&v)), PluginSource::Default, kline)
                        })
                        .collect(),
                    Loose::Bad(f) if f.kind == "nothing" => Vec::new(),
                    Loose::Bad(_) => {
                        self.diags.error(
                            "invalid-plugin-declaration",
                            file.clone(),
                            line,
                            "`plugins` must be a list like `- duckdb: \">=1.0\"`",
                        );
                        continue;
                    }
                };
                for (name, c, source, eline) in entries {
                    if !dre_protocol::valid_name(&name) {
                        self.diags.error(
                            "invalid-plugin-declaration",
                            file.clone(),
                            eline,
                            format!(
                                "plugin package `{name}`: a package name is lowercase letters, digits and `_`"
                            ),
                        );
                        continue;
                    }
                    let raw = match &c {
                        None | Some(Value::Null) => "*".to_string(),
                        Some(v) => crate::yaml::scalar_str(v).unwrap_or_default(),
                    };
                    match semver::VersionReq::parse(&raw) {
                        Ok(req) => by_package.entry(name).or_default().push(Decl {
                            req,
                            raw,
                            file: yf.display.clone(),
                            source,
                        }),
                        Err(e) => self.diags.error(
                            "invalid-version-constraint",
                            file.clone(),
                            eline,
                            format!("plugin package `{name}`: invalid version constraint `{raw}`: {e}"),
                        ),
                    }
                }
            }
        }
        let mut out = Vec::new();
        for (name, ds) in &by_package {
            // Every declaration of one package must agree on where it comes from.
            let source = ds
                .iter()
                .map(|d| &d.source)
                .find(|s| !s.is_default())
                .cloned()
                .unwrap_or_default();
            if let Some(other) = ds.iter().find(|d| !d.source.is_default() && d.source != source) {
                self.diags.error(
                    "conflicting-plugin-sources",
                    Some(other.file.clone()),
                    None,
                    format!(
                        "plugin package `{name}` is declared with two sources: {} and {}",
                        source.lock_key().unwrap_or_default(),
                        other.source.lock_key().unwrap_or_default()
                    ),
                );
                project.plugins_incomplete = true;
                continue;
            }
            let reqs: Vec<&semver::VersionReq> = ds.iter().map(|d| &d.req).collect();
            if !constraints::compatible(&reqs) {
                for (i, a) in ds.iter().enumerate() {
                    for b in &ds[i + 1..] {
                        if !constraints::compatible(&[&a.req, &b.req]) {
                            self.diags.error(
                                "conflicting-plugin-constraints",
                                Some(b.file.clone()),
                                None,
                                format!(
                                    "plugin package `{name}` is declared `{}` in {} and `{}` in {}; no version satisfies both",
                                    a.raw,
                                    a.file.display(),
                                    b.raw,
                                    b.file.display()
                                ),
                            );
                        }
                    }
                }
                project.plugins_incomplete = true;
                continue;
            }
            let combined = constraints::combine(&reqs);
            let mut files: Vec<PathBuf> = ds.iter().map(|d| d.file.clone()).collect();
            files.dedup();
            out.push(PluginRequirement {
                name: name.clone(),
                version: combined.to_string(),
                declared_in: files,
                source,
            });
        }
        project.plugins = out;

        let mut uses = Vec::new();
        let profiles = &project.profiles;
        for (role, kind, refs) in [
            (Role::Connection, PluginKind::Source, &used.connections),
            (Role::Destination, PluginKind::Destination, &used.destinations),
        ] {
            for name in refs.keys() {
                let Some(p) = profiles.get(role, name) else {
                    continue;
                };
                let role_name = role.as_str();
                for out_ in p.targets.values() {
                    if kind == PluginKind::Destination && out_.kind == LOCAL_TYPE {
                        continue;
                    }
                    let id = PluginId::new(kind, out_.kind.clone());
                    if uses.iter().any(|u: &PluginUse| u.plugin == id) {
                        continue;
                    }
                    uses.push(PluginUse {
                        plugin: id,
                        file: profiles.file.clone(),
                        line: profiles.line_of(role, name),
                        what: format!("`type: {}` used by {role_name} profile `{name}`", out_.kind),
                    });
                }
            }
        }
        for (fmt, (ctx, file)) in &used.formats {
            let note = if fmt == "csv" {
                " (csv is the built-in default output)"
            } else {
                ""
            };
            uses.push(PluginUse {
                plugin: PluginId::new(PluginKind::Format, fmt.clone()),
                file: Some(file.clone()),
                line: None,
                what: format!("format `{fmt}` is used by {ctx}{note}"),
            });
        }
        project.plugin_uses = uses;
    }

    /// `sources:`, `formats:` or `destinations:` where plugins were once declared.
    fn old_plugin_key(&mut self, yf: &YamlFile, key: &str) {
        self.diags.error(
            "moved-plugin-declaration",
            Some(yf.display.clone()),
            yf.line_of(key, None),
            format!(
                "`{key}:` no longer declares plugins: list plugin packages under `plugins:` instead (e.g. `plugins: [duckdb, object_store]`)"
            ),
        );
    }

    // -- Jinja pre-flight ---------------------------------------------------------------------

    fn preflight(&mut self, project: &Project, _used: &Usage) {
        let cli_owned: Vec<String> = self.opts.vars.keys().cloned().collect();
        let cli: BTreeSet<&str> = cli_owned.iter().map(String::as_str).collect();
        let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut read = |root: &Path, p: &Path| -> Option<String> {
            if let Some(s) = sources.get(p) {
                return Some(s.clone());
            }
            let s = std::fs::read_to_string(root.join(p)).ok()?;
            sources.insert(p.to_path_buf(), s.clone());
            Some(s)
        };
        let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
        let root = self.root.clone();
        // Macros: syntax, env_var, run.*; their definitions, for var() checks per Binding.
        let mut defs: BTreeMap<String, (PathBuf, preflight::MacroDef)> = BTreeMap::new();
        for m in &project.macros {
            if let Some(src) = read(&root, m) {
                self.check_template_text(m, &src, 0, None, &cli);
                checked.insert(m.clone());
                for d in preflight::macro_defs(&src) {
                    defs.insert(d.name.clone(), (m.clone(), d));
                }
            }
        }
        for r in &project.reports {
            for b in &r.bindings {
                let ctx = match &b.set {
                    Some(s) => format!("report `{}`, Set `{s}`", r.name),
                    None => format!("report `{}`", r.name),
                };
                let mut called: Vec<String> = Vec::new();
                let mut refs: Vec<String> = Vec::new();
                for q in &b.queries {
                    let Some(src) = read(&root, &q.path) else { continue };
                    let first = checked.insert(q.path.clone());
                    self.check_vars(&q.path, &src, 0, &b.vars, &cli, &ctx);
                    if first {
                        self.check_template_text(&q.path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
                }
                // SQL this Binding pulls in through ref(), checked in the Binding's context.
                let mut seen_refs = BTreeSet::new();
                while let Some(name) = refs.pop() {
                    let Some(path) = project.sql.get(&name) else {
                        continue;
                    };
                    if !seen_refs.insert(name) {
                        continue;
                    }
                    let Some(src) = read(&root, path) else { continue };
                    self.check_vars(path, &src, 0, &b.vars, &cli, &ctx);
                    if checked.insert(path.clone()) {
                        self.check_template_text(path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
                }
                // Macros this Binding calls, directly or through other macros.
                let mut seen = BTreeSet::new();
                while let Some(name) = called.pop() {
                    let Some((file, def)) = defs.get(&name) else {
                        continue;
                    };
                    if !seen.insert(name) {
                        continue;
                    }
                    self.check_vars(file, &def.body, def.line_offset, &b.vars, &cli, &ctx);
                    called.extend(preflight::called_names(&def.body));
                }
                // Templated output values render with the same context.
                let mut values: Vec<String> = Vec::new();
                for d in b.destinations() {
                    values.extend(d.path.clone());
                    values.extend(d.options.values().flat_map(json_strings));
                }
                for t in b.outputs.iter().filter_map(|o| o.template.as_ref()) {
                    values.extend(t.bindings.iter().filter_map(|tb| tb.value.clone()));
                }
                for v in values.iter().filter(|v| preflight::is_templated(v)) {
                    let yf = std::fs::read_to_string(root.join(&r.file)).unwrap_or_default();
                    let line = yf.lines().position(|l| l.contains(v.as_str())).map(|i| i + 1);
                    self.check_template_text(&r.file, v, line.map_or(0, |l| l - 1), line, &cli);
                    self.check_vars(&r.file, v, line.map_or(0, |l| l - 1), &b.vars, &cli, &ctx);
                }
            }
        }
    }

    /// Syntax, `env_var()` and `run.*` checks for one template source.
    fn check_template_text(
        &mut self,
        file: &Path,
        src: &str,
        line_offset: usize,
        fixed_line: Option<usize>,
        _cli: &BTreeSet<&str>,
    ) {
        let f = Some(file.to_path_buf());
        if let Err((line, msg)) = preflight::check_syntax(&file.to_string_lossy(), src) {
            let line = fixed_line.or(line.map(|l| l + line_offset));
            self.diags.error(
                "jinja-syntax",
                f.clone(),
                line,
                format!("Jinja syntax error: {msg}"),
            );
            return;
        }
        for c in preflight::calls(src)
            .into_iter()
            .filter(|c| c.func == "env_var" && !c.has_default)
        {
            if std::env::var_os(&c.name).is_none() {
                self.diags.error(
                    "unset-env-var",
                    f.clone(),
                    Some(fixed_line.unwrap_or(c.line + line_offset)),
                    format!(
                        "`env_var('{}')`: environment variable `{}` is not set and no default is given",
                        c.name, c.name
                    ),
                );
            }
        }
        for (name, instead, line) in preflight::removed_names(src) {
            self.diags.error(
                "removed-template-name",
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!("`{name}` was removed in DRE 0.2: use {instead}"),
            );
        }
        for (r, line) in preflight::unknown_run_refs(src) {
            self.diags.error(
                "unknown-run-attribute",
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!(
                    "`{r}` isn't part of the run context; known: run.report, run.set, run.target, run.schedule, run.date (a date: .prev_month, .month_start, .yyyymmdd, ...), run.now, run.scheduled_at, run.timezone, run.date_format(...)"
                ),
            );
        }
    }

    fn check_vars(
        &mut self,
        file: &Path,
        src: &str,
        line_offset: usize,
        vars: &JsonMap<String, Json>,
        cli: &BTreeSet<&str>,
        ctx: &str,
    ) {
        for c in preflight::calls(src)
            .into_iter()
            .filter(|c| c.func == "var" && !c.has_default)
        {
            if !vars.contains_key(&c.name) && !cli.contains(c.name.as_str()) {
                self.diags.error(
                    "unresolved-var",
                    Some(file.to_path_buf()),
                    Some(c.line + line_offset),
                    format!(
                        "`var('{}')` has no value for {ctx} (checked --var, Set, report, folder and project vars) and no default",
                        c.name
                    ),
                );
            }
        }
    }

    fn check_template_files(&mut self, project: &Project) {
        let mut seen = BTreeSet::new();
        for r in &project.reports {
            for t in r
                .bindings
                .iter()
                .flat_map(|b| b.outputs.iter().filter_map(|o| o.template.as_ref()))
            {
                if !seen.insert((r.name.clone(), t.file.clone(), format!("{:?}", t.bindings))) {
                    continue;
                }
                let Some(path) = find_template(&self.root, &t.file) else {
                    self.diags.error(
                        "missing-template",
                        Some(r.file.clone()),
                        None,
                        format!("report `{}`: template file `{}` doesn't exist", r.name, t.file),
                    );
                    continue;
                };
                match template_sheets(&path) {
                    Ok(sheets) => {
                        for tb in &t.bindings {
                            if !sheets.contains(&tb.sheet) {
                                self.diags.error(
                                    "invalid-template",
                                    Some(r.file.clone()),
                                    None,
                                    format!(
                                        "report `{}`: template `{}` has no sheet `{}` (sheets: {})",
                                        r.name,
                                        t.file,
                                        tb.sheet,
                                        sheets.join(", ")
                                    ),
                                );
                            }
                        }
                    }
                    Err(e) => self.diags.error(
                        "invalid-template",
                        Some(r.file.clone()),
                        None,
                        format!("report `{}`: can't read template `{}`: {e}", r.name, t.file),
                    ),
                }
            }
        }
    }
}

/// Sheet names in an xlsx template, read without modifying it.
/// Where a template file named in YAML (`template:`, a message's `file:`) is read from: the
/// project root, else `templates/`.
pub fn find_template(root: &Path, file: &str) -> Option<PathBuf> {
    [root.join(file), root.join("templates").join(file)]
        .into_iter()
        .find(|p| p.is_file())
}

/// "output `name`", or "output 2" for an unnamed one, as messages name an output.
pub fn output_label(name: Option<&str>, index: usize) -> String {
    match name {
        Some(n) => format!("output `{n}`"),
        None => format!("output {}", index + 1),
    }
}

pub fn template_sheets(path: &Path) -> Result<Vec<String>, String> {
    use calamine::Reader;
    let wb: calamine::Xlsx<_> =
        calamine::open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
    Ok(wb.sheet_names().to_vec())
}

struct RawReport {
    name: String,
    file: Rc<YamlFile>,
    folder: Vec<String>,
    /// Each key's value and the fragment it's in, for the keys not yet read as typed config.
    keys: BTreeMap<String, Located<Value>>,
    /// The line of each key, in its fragment.
    lines: BTreeMap<String, Option<usize>>,
    /// The typed keys, each from the fragment that declares it.
    typed: ReportFile,
}

struct BindingBase {
    profile: Option<String>,
    profile_at: Option<ProfileAt>,
    vars: JsonMap<String, Json>,
    /// The merged outputs, in order: one unless a layer gave a list.
    outputs: Vec<Mapping>,
    locale: Option<String>,
}

/// Everything the project references, for profile and plugin checks.
#[derive(Default)]
struct Usage {
    connections: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    destinations: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    /// format → first user, for messages.
    formats: BTreeMap<String, (String, PathBuf)>,
}

impl Usage {
    /// A connection profile used by name. A value holding Jinja is recorded once the parse pass
    /// has rendered it.
    fn connection(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        if preflight::is_templated(p) {
            return;
        }
        let e = self.connections.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    fn destination(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        if preflight::is_templated(p) {
            return;
        }
        let e = self.destinations.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    fn format(&mut self, f: &str, ctx: &str, file: &Path) {
        self.formats
            .entry(f.to_string())
            .or_insert_with(|| (ctx.to_string(), file.to_path_buf()));
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn builtin_output() -> Mapping {
    let mut m = Mapping::new();
    m.insert(Value::String("format".into()), Value::String("csv".into()));
    m
}

fn project_default_output(p: &Project) -> Option<Mapping> {
    let yf = YamlFile::parse(
        std::fs::read_to_string(p.root.join(PROJECT_FILE)).ok()?,
        PathBuf::from(PROJECT_FILE),
        &mut Diagnostics::default(),
    )?;
    yf.value
        .get("default_output")
        .and_then(Value::as_mapping)
        .cloned()
}

/// Apply one report or Set `output:` layer over the inherited outputs; returns the problems.
///
/// - A list replaces whatever was inherited; each entry starts from the built-in defaults.
/// - A map merges ([`merge_output`]) into the single inherited output, or into the one its
///   `name:` names. With several inherited outputs and no `name:`, it's unclear which one to
///   change: the layer is ignored and the problem says why.
pub fn layer_outputs(base: &mut Vec<Mapping>, over: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    match over {
        Value::Sequence(list) if list.is_empty() => problems.push(
            "`output` is an empty list; give at least one output, or remove it to use the inherited one"
                .into(),
        ),
        Value::Sequence(list) => {
            let mut outs = Vec::new();
            for (i, entry) in list.iter().enumerate() {
                match entry.as_mapping() {
                    Some(m) => {
                        let mut o = builtin_output();
                        problems.extend(merge_output(&mut o, m));
                        outs.push(o);
                    }
                    None => problems.push(format!("`output` entry {} must be a map", i + 1)),
                }
            }
            if problems.is_empty() {
                *base = outs;
            }
        }
        Value::Mapping(m) => {
            let name_of = |o: &Mapping| o.get("name").and_then(Value::as_str).map(str::to_string);
            let target = match m.get("name").and_then(Value::as_str) {
                Some(n) => match base.iter().position(|o| name_of(o).as_deref() == Some(n)) {
                    Some(i) => Some(i),
                    None if base.len() == 1 && name_of(&base[0]).is_none() => Some(0),
                    None => {
                        let names: Vec<String> = base
                            .iter()
                            .filter_map(name_of)
                            .map(|n| format!("`{n}`"))
                            .collect();
                        problems.push(format!(
                            "no inherited output is named `{n}`{}; override the full `output:` list to add one",
                            if names.is_empty() {
                                String::new()
                            } else {
                                format!(" (they're {})", names.join(", "))
                            }
                        ));
                        None
                    }
                },
                None if base.len() == 1 => Some(0),
                None => {
                    problems.push(format!(
                        "this overrides `output` with no `name:`, but it inherits {} outputs, so it's unclear which one to change; give the `name:` of the output to change, or override the full list",
                        base.len()
                    ));
                    None
                }
            };
            if let Some(i) = target {
                problems.extend(merge_output(&mut base[i], m));
            }
        }
        _ => problems.push("`output` must be a map or a list of maps".into()),
    }
    problems
}

/// Merge an output layer over `base`. Changing `format` drops the lower layers' format options,
/// since they belong to a different format.
///
/// `destination` (a map or a list of maps):
/// - a list replaces whatever was inherited;
/// - a map naming a different `profile` replaces it too, so one plugin's options never leak into
///   another's;
/// - a map without `profile` (or with the same one) merges key by key into the inherited single
///   destination, so a Binding can override just `path`. With several inherited destinations
///   that's ambiguous: the layer's destination is ignored and the returned message says why.
pub fn merge_output(base: &mut Mapping, over: &Mapping) -> Option<String> {
    let fmt_key = Value::String("format".into());
    if let Some(f) = over.get(&fmt_key)
        && base.get(&fmt_key) != Some(f)
    {
        base.retain(|k, _| is_one_of(k, FORMAT_INDEPENDENT_KEYS));
    }
    let mut problem = None;
    for (k, v) in over {
        if k.as_str() == Some("destination")
            && let Value::Mapping(o) = v
        {
            let profile = |m: &Mapping| m.get("profile").cloned();
            match base.get_mut(k) {
                Some(Value::Sequence(list)) if o.get("profile").is_none() && list.len() > 1 => {
                    problem = Some(format!(
                        "this overrides `destination` with no `profile:`, but it inherits {} destinations, so it's unclear which one to change; override the full list instead",
                        list.len()
                    ));
                    continue;
                }
                Some(Value::Sequence(list))
                    if list.len() == 1
                        && list[0]
                            .as_mapping()
                            .is_some_and(|b| o.get("profile").is_none() || profile(b) == profile(o)) =>
                {
                    let mut b = list[0].as_mapping().cloned().unwrap_or_default();
                    b.extend(o.clone());
                    base.insert(k.clone(), Value::Mapping(b));
                    continue;
                }
                Some(Value::Mapping(b)) if o.get("profile").is_none() || profile(b) == profile(o) => {
                    b.extend(o.clone());
                    continue;
                }
                _ => {}
            }
        }
        base.insert(k.clone(), v.clone());
    }
    problem
}

/// Where an inherited `profile` comes from when the report sets none: the deepest folder's
/// `+profile`, else `default_profile`.
fn inherited_profile_at(layers: &[&FolderCfg], project: &Project) -> Option<ProfileAt> {
    if let Some((_, line)) = layers.iter().rev().find_map(|l| l.profile.as_ref()) {
        return Some(ProfileAt {
            file: PathBuf::from(PROJECT_FILE),
            line: *line,
            key: "`+profile`".into(),
        });
    }
    project.default_profile.as_ref().map(|_| ProfileAt {
        file: PathBuf::from(PROJECT_FILE),
        line: project.default_profile_line,
        key: "`default_profile`".into(),
    })
}

fn folder_layers<'a>(folders: &'a BTreeMap<Vec<String>, FolderCfg>, folder: &[String]) -> Vec<&'a FolderCfg> {
    (0..=folder.len())
        .filter_map(|n| folders.get(&folder[..n].to_vec()))
        .collect()
}

fn folder_segments(rel: &Path) -> Vec<String> {
    rel.components()
        .skip(1)
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect()
}

pub fn dotted(path: &[String]) -> String {
    path.join(".")
}

fn pick(v: &Value, keys: &[&str]) -> Mapping {
    v.as_mapping()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| is_one_of(k, keys))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Every string inside a JSON value (the templated values of a destination option).
fn json_strings(v: &Json) -> Vec<String> {
    match v {
        Json::String(s) => vec![s.clone()],
        Json::Array(a) => a.iter().flat_map(json_strings).collect(),
        Json::Object(o) => o.values().flat_map(json_strings).collect(),
        _ => Vec::new(),
    }
}

fn is_one_of(k: &Value, keys: &[&str]) -> bool {
    k.as_str().is_some_and(|k| keys.contains(&k))
}

/// A map of names to timings: every value holds `cron`, `every` or `rrule`.
fn is_timing_registry(m: &Mapping) -> bool {
    !m.is_empty()
        && m.values().all(|v| {
            v.as_mapping()
                .is_some_and(|t| ["cron", "every", "rrule"].iter().any(|k| t.contains_key(*k)))
        })
}

/// Every entry is a Set: a map of `profile`/`vars`, which may be empty (`plain: {}`, the
/// report's defaults) or left blank.
fn is_set_registry(m: &Mapping) -> bool {
    m.values().any(Value::is_mapping)
        && m.values().all(|v| {
            v.is_null()
                || v.as_mapping()
                    .is_some_and(|e| e.keys().all(|k| is_one_of(k, &["profile", "vars", "locale"])))
        })
}

fn string_list(v: &Value) -> Option<Vec<String>> {
    v.as_sequence()?
        .iter()
        .map(|i| i.as_str().map(str::to_string))
        .collect()
}

/// The query a `queries:` item names, if it names one.
fn query_item_name(item: &QueryItem) -> Option<&str> {
    match &item.value {
        Loose::Ok(de::OneOf::A(s)) => Some(s),
        Loose::Ok(de::OneOf::B(e)) => e.query.as_ref().and_then(Loose::ok).map(String::as_str),
        Loose::Bad(_) => None,
    }
}

fn entry_name(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Mapping(m) => m.get("query").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    v.retain(|t| seen.insert(t.clone()));
}

/// A JSON value as YAML, for the output merging that still works on YAML.
fn json_to_yaml_value(v: &Json) -> Value {
    serde_yaml_ng::to_value(v).unwrap_or(Value::Null)
}

/// A JSON map as a YAML mapping, for the output and vars merging that still works on YAML.
fn json_to_yaml(m: JsonMap<String, Json>) -> Mapping {
    match serde_yaml_ng::to_value(Json::Object(m)) {
        Ok(Value::Mapping(m)) => m,
        _ => Mapping::new(),
    }
}

pub fn yaml_to_json(v: &Value) -> Json {
    serde_json::to_value(v).unwrap_or(Json::Null)
}

pub fn yaml_map_to_json(m: &Mapping) -> JsonMap<String, Json> {
    match yaml_to_json(&Value::Mapping(m.clone())) {
        Json::Object(o) => o,
        _ => JsonMap::new(),
    }
}

/// Letters, digits and `_`, not starting with a digit.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
