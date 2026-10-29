//! Loading a DRE project directory into a resolved, validated project.
//!
//! Every YAML file in the project is parsed and classified by its shape, not its name. Report
//! config merges by report name from anywhere, then resolves with one precedence rule:
//! Binding > report YAML > folder config (deepest wins) > project defaults > built-in defaults.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Serialize;
use serde_json::Value;
use serde_json::{Map as JsonMap, Value as Json};

/// A map as read from YAML (or merged from several files).
type Mapping = JsonMap<String, Json>;

use crate::codes::Code;
use crate::dates::{WeekNumbering, WeekStart};
use crate::diag::Diagnostics;
use crate::formats::{Files, Format};
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

pub use outputs::{find_template, layer_outputs, merge_output, output_label, resolve_template, template_sheets};
use plugins::Usage;
use reports::BindingBase;
use schedules::is_timing_registry;
use sets::is_set_registry;
use outputs::{builtin_output, project_default_output};

mod discover;
mod outputs;
mod plugins;
mod project_file;
mod reports;
mod schedules;
mod sets;
mod sources;
mod templates;

pub const PROJECT_FILE: &str = "dre_project.yml";
pub const REPORTS_DIR: &str = "reports";
pub const MACROS_DIR: &str = "macros";
pub const TARGET_DIR: &str = "target";
pub const LOGS_DIR: &str = "logs";
pub use crate::plugins::DEPS_DIR;
pub const DEFAULT_RUN_QUERY_MAX_ROWS: u64 = 10_000;

pub const REPORT_KEYS: &[&str] = &[
    "name",
    "description",
    "meta",
    "tags",
    "queries",
    "output",
    "profile",
    "sets",
    "default_set",
    "schedule",
    "vars",
    "var_schema",
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
    "meta",
    "default_profile",
    "default_output",
    "format_options",
    "default_set",
    "vars",
    "run_query_max_rows",
    "lookup_inline_max_rows",
    "flags",
    "dispatch",
    "mask_secrets",
    "mask_row_values",
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
    "+meta",
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
    "query",
    "profile",
    "tab",
    "tab_name",
    "anchor",
    "header",
    "columns",
    "autofit",
    "style",
    "on_empty",
    "secret_columns",
];
/// The shared output keys that don't belong to one format: they survive a layer changing `format`.
const FORMAT_INDEPENDENT_KEYS: &[&str] = &[
    "name",
    "queries",
    "when",
    "destination",
    "template",
    "on_empty",
    "for_each",
    "file_name",
];
/// The keys of a destination entry core reads; every other key is the plugin's.
pub const DESTINATION_KEYS: &[&str] = &["profile", "path", "attach", "hold", "rate", "burst", "zip"];
pub const OUTPUT_SHARED_KEYS: &[&str] = &[
    "name",
    "format",
    "queries",
    "when",
    "destination",
    "template",
    "extension",
    "on_empty",
    "for_each",
    "file_name",
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
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub meta: JsonMap<String, Json>,
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
    /// How many runs of each Binding to keep in `target/run/` (the current one always stays).
    #[serde(skip)]
    pub keep_runs: usize,
    /// `flags: run_timeout`: how long a run may take (off when unset).
    #[serde(skip)]
    pub run_timeout: Option<std::time::Duration>,
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
    /// Mask per-row destination values from `each.*` in logs and records (default true).
    pub mask_row_values: bool,
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
    /// Every setting and where its value came from.
    #[serde(skip)]
    pub settings: crate::settings::Settings,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub meta: JsonMap<String, Json>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meta_layers: Vec<MetaLayer>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub var_schema: crate::variables::Definitions,
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

/// An unmerged metadata layer. Consumers own its semantics and can explain its origin.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct MetaLayer {
    pub file: String,
    pub key: String,
    pub meta: JsonMap<String, Json>,
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
    /// xlsx: size this tab's columns from their content, over the output's `autofit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofit: Option<bool>,
    /// xlsx: how this tab looks, over the output's `style`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<dre_protocol::style::SheetStyle>,
    /// `on_empty:`: what to do when this query returns no rows; `None`: the output decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_empty: Option<crate::no_data::OnEmpty>,
    /// `secret_columns:`: columns whose values are masked wherever DRE reports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub secret_columns: Vec<String>,
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

    /// Every output's destinations, in order.
    pub fn destinations(&self) -> impl Iterator<Item = &Destination> {
        self.outputs.iter().flat_map(|o| o.destinations.iter())
    }

    /// Every output's destinations, each with its key: how the parse pass's rendered profiles
    /// ([`crate::parse::ParsedBinding::destination`]) are found again.
    pub fn keyed_destinations(&self) -> impl Iterator<Item = (DestinationKey, &Destination)> {
        self.outputs.iter().enumerate().flat_map(|(output, o)| {
            o.destinations
                .iter()
                .enumerate()
                .map(move |(entry, d)| (DestinationKey { output, entry }, d))
        })
    }

    /// The output named `name`.
    pub fn output(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name.as_deref() == Some(name))
    }
}

/// Whether a template reads `each.*`.
pub fn mentions_each(src: &str) -> bool {
    static EACH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\beach\s*(\.|\[)").expect("the pattern is a valid regex")
    });
    EACH.is_match(src)
}

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
    /// `on_empty:`: what to do when every query returns no rows; `None`: `continue`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_empty: Option<crate::no_data::OnEmpty>,
    /// `for_each:`: one copy of the output per row of a driver query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ForEach>,
    /// `file_name:` (Jinja, may use `each.*`): a per-row output's file name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

/// `for_each:` on an output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForEach {
    /// The driver query.
    pub query: String,
    /// The key column(s).
    pub key: Vec<String>,
    /// Rows at once; `None`: the default (4) or `--row-concurrency`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    pub on_row_error: RowErrors,
    pub on_orphan_rows: OrphanRows,
    pub max_population_rows: u64,
    pub keep_results: bool,
    /// `None`: the project's `mask_row_values`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_row_values: Option<bool>,
}

/// The default `for_each.max_population_rows`.
pub const DEFAULT_MAX_POPULATION_ROWS: u64 = 5_000_000;
/// The default `for_each.concurrency`.
pub const DEFAULT_ROW_CONCURRENCY: usize = 4;

/// `for_each.on_row_error`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RowErrors {
    /// Record a failed row and carry on; the run ends failed.
    #[default]
    Continue,
    /// Render every row first; deliver only if all rendered.
    Fail,
}

/// `for_each.on_orphan_rows`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OrphanRows {
    #[default]
    Warn,
    Fail,
}

impl Output {
    /// Whether this output makes a section (a tab, a file, `results.<query>`) of `query`: it
    /// formats it, and it isn't the output's `for_each` driver.
    pub fn sections(&self, query: &str) -> bool {
        self.feeds(query) && self.for_each.as_ref().is_none_or(|f| f.query != query)
    }

    /// Whether this output formats `query`'s result.
    pub fn feeds(&self, query: &str) -> bool {
        self.queries.as_ref().is_none_or(|q| q.iter().any(|n| n == query))
    }

    pub fn is_message(&self) -> bool {
        Format::new(&self.format).is_message()
    }

    /// How messages name it: "output `x`", or "output 2" when unnamed.
    pub fn label(&self, index: usize) -> String {
        output_label(self.name.as_deref(), index)
    }
}

/// A destination entry's place in its Binding: its output's index, and its own among that
/// output's destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestinationKey {
    pub output: usize,
    pub entry: usize,
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
    /// `hold: true`: delivered later, by `dre release`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hold: bool,
    /// `rate:` (and `burst:`): core paces deliveries to this destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<crate::rate::Rate>,
    /// `zip:`: deliver one zip of the output's (and attached outputs') files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip: Option<crate::zip_pack::ZipSpec>,
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
    /// The settings the command line resolved (the run's timezone, date and scheduled
    /// instant); the load adds the project's.
    pub settings: crate::settings::Settings,
}

/// [`load_with`] in an empty context: no environment variables (no `DRE_*` setting, no
/// `env_var()` value) and plugins run as child processes.
pub fn load(root: &Path, opts: &LoadOptions) -> (Option<Project>, Diagnostics) {
    load_with(root, opts, &crate::Context::empty())
}

/// Parse and validate the project at `root` in `ctx` (its environment and secrets). Returns the
/// resolved project when it could be built at all, plus every diagnostic found along the way.
pub fn load_with(root: &Path, opts: &LoadOptions, ctx: &crate::Context) -> (Option<Project>, Diagnostics) {
    let mut l = Loader {
        ctx: ctx.clone(),
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
    /// The command's environment and secrets.
    ctx: crate::Context,
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
    meta: Option<MetaLayer>,
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
                Code::ProjectFileMissing,
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
                    Code::InvalidProject,
                    Some(pyaml.display.clone()),
                    None,
                    format!("{PROJECT_FILE} must be a map"),
                );
                return None;
            }
            Err(e) => {
                self.diags.error(
                    Code::InvalidProject,
                    Some(pyaml.display.clone()),
                    e.line,
                    e.message,
                );
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
                    Code::InvalidField,
                    Some(PathBuf::from(PROJECT_FILE)),
                    target_line,
                    "`target_path` must be a path",
                );
                return None;
            }
        };
        match crate::target::resolve(
            &self.root,
            self.opts.target_path.as_deref(),
            project_target,
            self.ctx.env(),
        ) {
            Ok(t) => {
                self.target_inside = crate::target::inside(&self.root, &t.dir);
                // Databricks Workspace files don't reliably keep a rename: run folders' `current`
                // pointer and atomic writes need a Volume (or local disk).
                if crate::slash(&t.dir).to_string_lossy().starts_with("/Workspace/") {
                    let from_file = self.opts.target_path.is_none()
                        && self.ctx.env().setting(crate::target::ENV).is_none();
                    self.diags.warning(
                        Code::TargetPathOnWorkspace,
                        from_file.then(|| PathBuf::from(PROJECT_FILE)),
                        from_file.then_some(target_line).flatten(),
                        format!(
                            "the target path {} is in Databricks Workspace files, which don't reliably keep DRE's atomic writes and run pointer; use a Volume (`/Volumes/<catalog>/<schema>/<volume>/...`) or local disk",
                            t.dir.display()
                        ),
                    );
                }
                self.target = t;
            }
            Err(e) => {
                let from_file =
                    self.opts.target_path.is_none() && self.ctx.env().setting(crate::target::ENV).is_none();
                self.diags.error(
                    Code::InvalidTargetPath,
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

        let (profiles_dir, found_by) = crate::profiles::locate(
            self.opts.profiles_dir.as_deref(),
            Some(&self.root),
            self.ctx.env(),
        );
        project.profiles = Profiles::load(&profiles_dir, found_by, self.ctx.secrets(), &mut self.diags);
        project.profiles.run = project.run_target();
        self.record_settings(&mut project);

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
                &yf,
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
            .filter_map(|(name, paths)| match paths.as_slice() {
                [path] => Some((name.clone(), path.clone())),
                _ => None,
            })
            .collect();
        // Queries resolve by bare basename; whatever no report YAML references is unmanaged.
        let mut referenced: BTreeSet<String> = fragments
            .iter()
            .filter_map(|f| f.map.get("queries").and_then(Value::as_array))
            .flatten()
            .filter_map(entry_name)
            .collect();
        project.lookups = lookups::discover(&self.root, &found.lookups, &mut self.diags);
        for (name, l) in &project.lookups {
            if let Some(sql) = sql_index.get(name) {
                self.diags.error(
                    Code::DuplicateRefName,
                    Some(l.file.clone()),
                    None,
                    format!(
                        "lookup `{name}` has the same name as {}; `ref()` names must be unique",
                        sql[0].display()
                    ),
                );
            }
            if let Err(e) = lookups::read(&self.root, l) {
                self.diags
                    .error(Code::InvalidLookup, Some(l.file.clone()), None, e);
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
            let [path] = path.as_slice() else {
                continue;
            };
            if referenced.contains(name) {
                continue;
            }
            if let Some(other) = managed_names.get(name) {
                self.diags.error(
                    Code::DuplicateReportName,
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
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

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

fn metadata_layers(project: &Project, folders: &[&FolderCfg]) -> Vec<MetaLayer> {
    let mut layers = Vec::new();
    if !project.meta.is_empty() {
        layers.push(MetaLayer {
            file: PROJECT_FILE.into(),
            key: "meta".into(),
            meta: project.meta.clone(),
        });
    }
    layers.extend(folders.iter().filter_map(|f| f.meta.clone()));
    layers
}

fn folder_layers<'a>(folders: &'a BTreeMap<Vec<String>, FolderCfg>, folder: &[String]) -> Vec<&'a FolderCfg> {
    (0..=folder.len())
        .filter_map(|n| folders.get(folder.get(..n)?))
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
    v.as_object()
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

fn is_one_of(k: &str, keys: &[&str]) -> bool {
    keys.contains(&k)
}

fn string_list(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
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
        Value::Object(m) => m.get("query").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    v.retain(|t| seen.insert(t.clone()));
}

/// Letters, digits and `_`, not starting with a digit.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
