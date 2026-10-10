//! Every diagnostic and error code DRE reports, in one registry.
//!
//! A code is a readable slug (`unknown-key`) that begins every diagnostic and names a section of
//! the error codes reference. `dre explain <code>` prints its explanation, and `--log-format
//! json` events, `dre validate --json` and `run_results.json` carry it. Each code has a [`Kind`],
//! which decides the exit code and whether a failure is worth retrying. A plugin's codes are
//! namespaced by the plugin (`sftp/host-key-mismatch`) and declared in its descriptor.
//!
//! From 1.0 codes are stable: a code is never reused, only retired, and renaming one is a
//! breaking change.

use std::fmt;

use serde::Serialize;

/// What kind of problem a code is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The command was called wrongly: an unknown selector, a missing argument.
    Usage,
    /// The project's files are wrong: a bad key, a missing reference.
    Config,
    /// A plugin is missing, can't be installed, or doesn't behave.
    Plugin,
    /// DRE refused something unsafe: a statement an unmanaged report may not run.
    Refused,
    /// A connection couldn't be made; trying again may work.
    Connection,
    /// A connection was refused its credentials.
    Auth,
    /// A query failed on the database.
    Query,
    /// An output couldn't be delivered.
    Delivery,
    /// Something DRE didn't expect: a file it can't read, a bug.
    Internal,
    /// The run was cancelled.
    Cancelled,
    /// The run took longer than its timeout.
    TimedOut,
}

impl Kind {
    /// The exit code a failure of this kind ends a command with when it stops the command from
    /// starting: 2 for a usage, config, plugin or refusal problem (fix it; retrying won't help),
    /// 124 for a timeout, 130 for a cancel, else 1. A failure while a run is under way exits 1
    /// whatever its kind (see `dre_core::run::RunSummary::exit_code`).
    pub fn exit_code(self) -> u8 {
        match self {
            Kind::Usage | Kind::Config | Kind::Plugin | Kind::Refused => 2,
            Kind::TimedOut => 124,
            Kind::Cancelled => 130,
            Kind::Connection | Kind::Auth | Kind::Query | Kind::Delivery | Kind::Internal => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Config => "config",
            Kind::Plugin => "plugin",
            Kind::Refused => "refused",
            Kind::Connection => "connection",
            Kind::Auth => "auth",
            Kind::Query => "query",
            Kind::Delivery => "delivery",
            Kind::Internal => "internal",
            Kind::Cancelled => "cancelled",
            Kind::TimedOut => "timed_out",
        }
    }
}

impl Kind {
    /// The kind with this name (`timed_out`), as plugins send it.
    pub fn parse(name: &str) -> Option<Kind> {
        [
            Kind::Usage,
            Kind::Config,
            Kind::Plugin,
            Kind::Refused,
            Kind::Connection,
            Kind::Auth,
            Kind::Query,
            Kind::Delivery,
            Kind::Internal,
            Kind::Cancelled,
            Kind::TimedOut,
        ]
        .into_iter()
        .find(|k| k.as_str() == name)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! codes {
    ($( $(#[$doc:meta])* $variant:ident = $slug:literal, $kind:ident, $summary:literal, $explanation:literal; )*) => {
        /// A registered code. See the module docs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code {
            $( $(#[$doc])* $variant, )*
        }

        impl Code {
            /// Every code, in the reference's order.
            pub const ALL: &'static [Code] = &[$( Code::$variant, )*];

            /// The slug, e.g. `unknown-key`.
            pub fn as_str(self) -> &'static str {
                match self { $( Code::$variant => $slug, )* }
            }

            pub fn kind(self) -> Kind {
                match self { $( Code::$variant => Kind::$kind, )* }
            }

            /// One line: what the code means.
            pub fn summary(self) -> &'static str {
                match self { $( Code::$variant => $summary, )* }
            }

            /// What causes it and how to fix it.
            pub fn explanation(self) -> &'static str {
                match self { $( Code::$variant => $explanation, )* }
            }
        }
    };
}

impl Code {
    /// The code with this slug.
    pub fn parse(slug: &str) -> Option<Code> {
        Code::ALL.iter().copied().find(|c| c.as_str() == slug)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// A code as a failure reports it: a registered [`Code`], or a plugin's own, namespaced by the
/// plugin (`sftp/host-key-mismatch`) with the kind the plugin gave.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    Core(Code),
    Plugin { code: String, kind: Kind },
}

impl ErrorCode {
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::Core(c) => c.as_str(),
            ErrorCode::Plugin { code, .. } => code,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            ErrorCode::Core(c) => c.kind(),
            ErrorCode::Plugin { kind, .. } => *kind,
        }
    }
}

impl From<Code> for ErrorCode {
    fn from(c: Code) -> ErrorCode {
        ErrorCode::Core(c)
    }
}

impl PartialEq<Code> for ErrorCode {
    fn eq(&self, other: &Code) -> bool {
        *self == ErrorCode::Core(*other)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl PartialEq<&str> for Code {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for Code {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

codes! {
    // -- YAML and the project's files -------------------------------------------------------
    YamlSyntax = "yaml-syntax", Config,
        "A YAML file doesn't parse.",
        "The file isn't valid YAML: an unclosed bracket or quote, a tab used for indentation, a key written twice. The message names the line. Fix the file; `dre validate` reports every file's problems in one pass.";
    IoError = "io-error", Internal,
        "A file couldn't be read.",
        "DRE couldn't read a file it found (permissions, a broken link, a file that vanished). The message has the operating system's reason.";
    ProjectFileMissing = "project-file-missing", Usage,
        "There's no dre_project.yml.",
        "DRE looks for `dre_project.yml` in `--project-dir` (default: the current folder). Run the command from the project root, pass `--project-dir`, or create a project with `dre new`.";
    InvalidProject = "invalid-project", Config,
        "dre_project.yml has the wrong shape.",
        "`dre_project.yml` must be a map of keys (`name:`, `default_profile:`, ...). See the dre_project.yml reference.";
    InvalidReport = "invalid-report", Config,
        "A report file has the wrong shape.",
        "A report YAML file must be a map of report keys (`queries:`, `output:`, ...). See the report reference.";
    UnknownKey = "unknown-key", Config,
        "A key DRE doesn't know.",
        "The file has a key that isn't part of its format, usually a typo or a key in the wrong place (a report key in dre_project.yml). The message lists the keys that are allowed there; the YAML references list them all.";
    RemovedKey = "removed-key", Config,
        "A key that was removed.",
        "The key belonged to an older DRE. The message says what replaces it.";
    InvalidField = "invalid-field", Config,
        "A key has a value of the wrong type or shape.",
        "The value doesn't fit the key: a string where a list is expected, a number out of range, an unknown choice. The message says what the key takes.";
    MissingField = "missing-field", Config,
        "A required key is missing.",
        "The file leaves out a key it needs, e.g. `name:` in dre_project.yml, or the name of a report outside a report folder.";
    InvalidFolderConfig = "invalid-folder-config", Config,
        "Folder config under `reports:` has the wrong shape.",
        "Each folder under `reports:` in dre_project.yml is a map: `+` keys are settings, other keys are subfolders.";
    UnknownFolder = "unknown-folder", Config,
        "Folder config names a folder that doesn't exist.",
        "Folder config in dre_project.yml (`reports:`) sets something for a folder that isn't under `reports/`. Check the spelling and nesting, or remove it.";
    InvalidTimezone = "invalid-timezone", Config,
        "A timezone isn't a known IANA name.",
        "`timezone:` takes an IANA name such as `Australia/Sydney` or `UTC`, not an offset or abbreviation.";
    InvalidLocale = "invalid-locale", Config,
        "A locale isn't one the number filters know.",
        "`locale:` takes a tag such as `en`, `de-DE` or `fr`; the message lists what's known.";
    InvalidTargetPath = "invalid-target-path", Config,
        "The target path isn't usable.",
        "The target path (`--target-path`, `DRE_TARGET_PATH` or `target_path:`) must be a local or mounted folder outside the project's sources, not a URL. To copy outputs to object storage, deliver them with a destination.";
    TargetPathOnWorkspace = "target-path-on-workspace", Config,
        "The target path is in Databricks Workspace files.",
        "Workspace files (`/Workspace/...`) have shown flushing and rename problems, so DRE's atomic writes and the `current` pointer of its run folders can't be relied on there. On Databricks, point the target path at a Unity Catalog Volume (`/Volumes/<catalog>/<schema>/<volume>/dre/target`), or at local disk when nothing needs to outlive the job. A warning, so a run there still works as before.";
    TargetPathUnwritable = "target-path-unwritable", Config,
        "The target path can't be written.",
        "DRE couldn't create or write the target folder (permissions, a read-only mount, a full disk). The message has the reason.";
    UnrecognizedYaml = "unrecognized-yaml", Config,
        "A YAML file isn't a report, Set, plugin or schedule file.",
        "DRE recognises project YAML files by their keys. A file outside `reports/` that's none of a report (`queries:`), Sets, timings, schedules, `plugins:` or `sources:` is ignored. Move it, or fix its keys.";
    // -- reports and queries ----------------------------------------------------------------
    DuplicateReportName = "duplicate-report-name", Config,
        "Two reports have the same name.",
        "Report names are unique across the project: a report's name is its folder's (or its `name:`), and an unmanaged report is named after its `.sql` file. Rename one of them.";
    ConflictingDeclaration = "conflicting-declaration", Config,
        "Two fragments of a report set the same key.",
        "A report may be split over several YAML files, but each key is set in one of them only. Remove the key from one file.";
    MissingQueries = "missing-queries", Config,
        "A report has no queries.",
        "A managed report needs a non-empty `queries:` list naming `.sql` files under `reports/`. A YAML file that only configures a report must name one that declares `queries:`.";
    InvalidQueryName = "invalid-query-name", Config,
        "A query is named with a path or extension.",
        "`queries:` entries are bare `.sql` file names without folders or the extension: `orders`, not `reports/sales/orders.sql`.";
    UnknownQuery = "unknown-query", Config,
        "A query name doesn't match a .sql file or one of the report's queries.",
        "The name in `queries:` (or a Set's `exclude:`/`queries:`, an output's `queries:`, `tab_names:`) doesn't match a `.sql` file under `reports/`, or isn't one of the report's queries. Check the spelling.";
    DuplicateSqlName = "duplicate-sql-name", Config,
        "Two .sql files have the same name.",
        "Queries are found by file name across `reports/`, so two `.sql` files with one name are ambiguous. Rename one.";
    UnusedQuery = "unused-query", Config,
        "A query feeds no output.",
        "Every output names its queries, and this one is in none of them. Add it to an output's `queries:`, or give it `tab: false` if it only prepares later queries (temp tables, `SET`s).";
    DuplicateOutput = "duplicate-output", Config,
        "Two outputs would have the same name or file.",
        "Outputs of one report need unique `name:`s, and two unnamed outputs of one format would write the same file. Give each output a `name:`.";
    InvalidOutputOption = "invalid-output-option", Config,
        "An output option is wrong.",
        "A shared output key (`template`, `extension`, ...) or a format's option has a bad value, or applies to another format. The message names the option.";
    InvalidDestinationOption = "invalid-destination-option", Config,
        "A destination option is wrong.",
        "An `output.destination` entry has a bad value for one of its options (`attach:` naming no output, a plugin's option of the wrong type). The message names the option.";
    InvalidTemplate = "invalid-template", Config,
        "An xlsx template binding is wrong.",
        "`output.template` needs a `file:`, and each binding a `sheet:` with either a table block (`query`, optional `anchor`, `columns`) or a single cell (`cell` with `value`, or `query` + `column`).";
    MissingTemplate = "missing-template", Config,
        "A template file doesn't exist.",
        "`output.template.file` names a file that isn't in the project. Paths are relative to the project root.";
    InvalidCell = "invalid-cell", Config,
        "A cell reference isn't valid.",
        "Cells are written like `A1` or `AB12`.";
    JinjaSyntax = "jinja-syntax", Config,
        "A template doesn't compile.",
        "A Jinja template in SQL or YAML has a syntax error: an unclosed `{{` or `{%`, an unknown tag. The message has the line.";
    ParseFailed = "parse-failed", Config,
        "A query doesn't render.",
        "Rendering a query without a database failed: a macro error, a bad `source()` or `ref()`, a function used where it can't run. The message has the cause.";
    CompileFailed = "compile-failed", Config,
        "A report doesn't compile.",
        "`dre compile` or `validate` couldn't render a Binding's SQL. The message has the cause.";
    CompileNeedsRun = "compile-needs-run", Config,
        "A report can only be checked by running it.",
        "The report queries the database while rendering (`run_query()`, `columns()`), so `validate` can't compile it offline. `dre run` (or `validate --live`) checks it.";
    UnknownRef = "unknown-ref", Config,
        "`ref()` names nothing.",
        "`ref('name')` must name a `.sql` file under `reports/` or a lookup under `lookups/`.";
    RefCycle = "ref-cycle", Config,
        "`ref()` calls go round in a circle.",
        "Shared SQL files `ref()` each other in a loop. The message shows the cycle; break it.";
    DuplicateRefName = "duplicate-ref-name", Config,
        "A lookup and a .sql file have the same name.",
        "`ref()` names are shared by `.sql` files and lookups, so they must be unique. Rename one.";
    UnresolvedVar = "unresolved-var", Config,
        "`var()` has no value.",
        "No level sets the variable: `--var`, the schedule, the Set, the report, the folders, or the project. Set it at one of them, or give `var()` a default.";
    UnsetEnvVar = "unset-env-var", Config,
        "`env_var()` names an unset variable.",
        "The environment variable isn't set and `env_var()` has no default. Set it, or give a default: `env_var('NAME', 'fallback')`.";
    UnknownRunAttribute = "unknown-run-attribute", Config,
        "`run.<x>` isn't part of the run context.",
        "Templates can use `run.report`, `run.set`, `run.target`, `run.schedule`, `run.date` (with its navigation, e.g. `.prev_month`) and `run.now`.";
    RemovedTemplateName = "removed-template-name", Config,
        "A template uses a name that was removed.",
        "The function or variable was renamed in an earlier release; the message says what to use.";
    UnmanagedReport = "unmanaged-report", Config,
        "A .sql file runs as an unmanaged report.",
        "A `.sql` file no YAML lists in `queries:` (and no `ref()` uses) is an unmanaged report: it runs with the folder's settings into a csv. That's for quick tests; add a YAML to make it a managed report.";
    UnmanagedSideEffect = "unmanaged-side-effect", Refused,
        "An unmanaged report runs a statement it may not.",
        "Unmanaged reports may only run `SELECT`/`WITH` and create temp tables or views, so a stray `.sql` file can't change data. Rewrite the statement, or give the report a YAML.";
    // -- Sets and connections ---------------------------------------------------------------
    DuplicateSet = "duplicate-set", Config,
        "A Set is declared or listed twice.",
        "Set names are unique in `sets.yml` files, and a report lists each Set once.";
    UnknownSet = "unknown-set", Config,
        "A report names a Set that doesn't exist.",
        "A name in a report's `sets:` must be declared in `sets.yml` (or be a map that declares it inline).";
    UnknownDefaultSet = "unknown-default-set", Config,
        "`default_set` isn't one of the report's Sets.",
        "`default_set` (in the report or dre_project.yml) must name one of the report's `sets:`.";
    ExcludeAndQueries = "exclude-and-queries", Config,
        "A Set uses both `exclude:` and `queries:`.",
        "A Set either leaves queries out (`exclude:`) or replaces them (`queries:`), not both.";
    ProfileAndSets = "profile-and-sets", Config,
        "A report sets both `profile:` and `sets:`.",
        "With Sets, each Set chooses its connection; give the report one or the other.";
    InvalidProfiles = "invalid-profiles", Config,
        "profiles.yml has the wrong shape.",
        "profiles.yml is a map with `connections:` and/or `destinations:`, each a map of profiles. See the profiles.yml reference.";
    InvalidProfile = "invalid-profile", Config,
        "A profile in profiles.yml is wrong.",
        "Each profile has `targets:`, a map of entries with a `type:` (or `{deliver: false}` for a destination), and optionally `target:`, the entry it uses by default.";
    InvalidProfileValue = "invalid-profile-value", Config,
        "A value that chooses a connection uses something only a connection has.",
        "`profile:` and `default_profile` are rendered before any connection is open, so they can't use `connection.*`, `run_query()` or `columns()`.";
    ProfilesSourcesRenamed = "profiles-sources-renamed", Config,
        "profiles.yml uses the old `sources:` section.",
        "DRE 0.2 renamed profiles.yml's `sources:` to `connections:`. Rename it.";
    ProfilesMissing = "profiles-missing", Config,
        "The project uses profiles but there's no profiles.yml.",
        "DRE looks for profiles.yml in `--profiles-dir`, `DRE_PROFILES_DIR`, the project directory, then `~/.dre`. Create one with `dre init`, or point DRE at yours.";
    UnknownProfile = "unknown-profile", Config,
        "A profile isn't defined in profiles.yml.",
        "The connection or destination profile isn't under `connections:`/`destinations:` in the profiles.yml DRE found. Check the name, or `dre validate -v` for where DRE looked.";
    TargetMismatch = "target-mismatch", Config,
        "The profiles are on another target than the run.",
        "Every profile the run uses chooses a different entry than the run's target (`--target`, `DRE_TARGET`, else `dev`). Pass `--target` or set `DRE_TARGET` so `target.name` matches.";
    NoConnection = "no-connection", Config,
        "A query has no connection.",
        "Nothing gives the query a connection: give it `profile:`, use a source with a `profile:`, or give the report one (`profile:` on the report or Set, a folder's `+profile`, or `default_profile` in dre_project.yml).";
    ConnectionConflict = "connection-conflict", Config,
        "A query would need two connections.",
        "A query uses sources (or a `profile:`) on different connections, but one query runs on one connection. Split it, or move the tables to one system.";
    MissingTargetEntry = "missing-target-entry", Config,
        "A profile has no entry for the run's target.",
        "Each profile the run uses picks an entry by `--target`, else `DRE_TARGET`, else its own `target:`, else `dev`. Add that entry to the profile's `targets:`, or choose another target. A destination can deliver nowhere on a target with `{deliver: false}`.";
    SetupOnOtherConnection = "setup-on-other-connection", Config,
        "A setup statement runs on another connection than the queries after it.",
        "A `tab: false` query prepares state (temp tables, `SET`s) that only exists on its own connection, but a later query reads it on another. Run them on one connection.";
    // -- sources ----------------------------------------------------------------------------
    InvalidSource = "invalid-source", Config,
        "A source declaration is wrong.",
        "`sources:` is dbt's format: a list of sources, each with a `name:` and `tables:`, optionally `database`, `schema`, `profile`, `quoting`, `tags`, `meta`. See the sources reference.";
    DuplicateSource = "duplicate-source", Config,
        "Two sources have the same name.",
        "Source names are unique across the project's YAML files.";
    DuplicateSourceTable = "duplicate-source-table", Config,
        "A source declares a table twice.",
        "Table names are unique within a source.";
    DuplicateSourceColumn = "duplicate-source-column", Config,
        "A table declares a column twice.",
        "Column names are unique within a table, compared case-insensitively.";
    SourceKeyNotSupported = "source-key-not-supported", Config,
        "A dbt source key DRE doesn't use yet.",
        "The key is valid dbt but DRE ignores it for now (freshness, loader, tests, ...). The declaration still works.";
    UnknownSource = "unknown-source", Usage,
        "A selector names a source that doesn't exist.",
        "`source:<name>` selects reports using that source; the name must be declared under `sources:`.";
    // -- selectors and schedules ------------------------------------------------------------
    AmbiguousSelector = "ambiguous-selector", Usage,
        "A selector matches a report and a folder.",
        "A bare name can mean a report or a folder. Use `folder:<path>` or the report's full dotted path.";
    InvalidSelector = "invalid-selector", Usage,
        "A selector can't be resolved.",
        "The selector (`-s`, `--set`, `--schedule`) doesn't resolve to Bindings to run. The message says why; `dre ls` lists what exists.";
    SelectorMatchesNothing = "selector-matches-nothing", Usage,
        "A selector or schedule matches no report.",
        "The selector (`-s`, a schedule's `select:` or `report:`) names no report, folder, tag or Set. `dre ls` lists what exists.";
    InvalidSchedule = "invalid-schedule", Config,
        "A schedule is wrong.",
        "Each schedules.yml entry has a `name`, `report:` (optionally `set:`) or `select:`, and a timing: `timing:` naming one in timings.yml, or its own `cron`, `every` or `rrule`.";
    DuplicateScheduleName = "duplicate-schedule-name", Config,
        "Two schedules have the same name.",
        "Schedule names are unique across the project.";
    ScheduleMoved = "schedule-moved", Config,
        "A schedule is set where schedules are no longer written.",
        "Schedules moved from report and folder YAML (`schedule:`, `+schedule`) to schedules.yml, as named entries.";
    ScheduleNoTime = "schedule-no-time", Config,
        "A timing has no time of day.",
        "An `every` or `rrule` timing without `at:` (or `BYHOUR`) fires at midnight. Add `at:` to fire at another time.";
    ScheduleNeedsAnchor = "schedule-needs-anchor", Config,
        "A timing needs a start date.",
        "`every`, and an rrule with `INTERVAL` above 1, a `COUNT` or a day taken from its start, need `starting:` to know which days to fire on.";
    ScheduleSeconds = "schedule-seconds", Config,
        "A cron expression has seconds.",
        "DRE's cron expressions have five fields (minute to weekday); a sixth field of seconds isn't supported.";
    ScheduleTooFrequent = "schedule-too-frequent", Config,
        "A timing fires more often than DRE allows.",
        "Schedules fire at most every minute; the message has the interval.";
    SchedulePathClash = "schedule-path-clash", Config,
        "Two schedules of one Binding deliver to the same path.",
        "With the same vars and date, two schedules would write the same file and one would overwrite the other. Give them different vars, or a path that tells them apart.";
    ScheduleTimezoneMismatch = "schedule-timezone-mismatch", Config,
        "A schedule fires in another timezone than its report renders in.",
        "The run date is the report's timezone's date at the firing time, which may not be the day you expect. Set `timezone:` on the schedule to fire and render in one.";
    InvalidTiming = "invalid-timing", Config,
        "A timing in timings.yml is wrong.",
        "Each timing is a map with exactly one of `cron`, `every` or `rrule`, and optionally `starting`, `at`, `except`, `also` and `timezone`.";
    DuplicateTimingName = "duplicate-timing-name", Config,
        "Two timings have the same name.",
        "Timing names are unique across the project.";
    UnknownTiming = "unknown-timing", Config,
        "A schedule names a timing that doesn't exist.",
        "`timing:` must name an entry of timings.yml; the message lists them.";
    UnusedTiming = "unused-timing", Config,
        "A timing isn't used by any schedule.",
        "Remove it, or use it with `timing:` in a schedule.";
    // -- lookups ----------------------------------------------------------------------------
    InvalidLookup = "invalid-lookup", Config,
        "A lookup or its config is wrong.",
        "A lookup is a csv, xlsx, xls, json or jsonl file under `lookups/` (or a `.yml` of rows), with an optional `<name>.yml` config of `columns`, `sheet` and `load`. The message names the problem.";
    DuplicateLookup = "duplicate-lookup", Config,
        "Two lookups have the same name.",
        "Lookups are named after their file, without the extension; two files with one name are ambiguous.";
    // -- running ----------------------------------------------------------------------------
    RunFailed = "run-failed", Internal,
        "A Binding failed for a reason without its own code.",
        "Something went wrong while running the Binding that isn't one of the other run errors. The message has the cause; please report it if it looks like a bug.";
    RunCancelled = "run-cancelled", Cancelled,
        "The run was stopped by Ctrl-C or a termination signal.",
        "DRE received Ctrl-C (SIGINT) or a termination signal (SIGTERM; on Windows Ctrl-Break, closing the console, logging off or shutting down), from you or from the orchestrator cancelling the job. It asked every running plugin to stop, waited up to 8 seconds, then stopped them; a source that can cancel its query on the server (Postgres, Databricks, DuckDB) did. The Binding that was running is recorded as `cancelled` in its `run_results.json`, Bindings that hadn't started don't run, and nothing is delivered after the cancel. `dre` exits 130 after Ctrl-C and 143 after a termination. A second Ctrl-C stops at once.";
    RunInProgress = "run-in-progress", Refused,
        "The same Binding is already running.",
        "Another `dre run` of this report and Set holds its lock (the `lock` file in `target/run/<report>/<set or default>/`), on this machine or, with a shared target path, on another. A Binding never runs twice at once, so the second run stops at once and changes nothing: it deletes no files, delivers nothing and leaves the drift snapshot alone. The message names the run in progress (its id, start time, host and process). Wait for it to finish. If that run is gone (a crashed machine, a killed container on another host), `dre unlock <report> [--binding <set>]` shows the lock and removes it. A lock left by a process on this machine that has gone is taken over automatically, with a warning.";
    RunTimedOut = "run-timed-out", TimedOut,
        "The run took longer than its timeout.",
        "The run's timeout (`dre run --timeout`, `DRE_RUN_TIMEOUT` or `flags: run_timeout` in dre_project.yml; off unless set) ran out. DRE stopped the run as it does for a termination signal: it asked every running plugin to stop, waited up to 8 seconds, then stopped them. The Binding that was running is recorded as `timed_out` in its `run_results.json`, the ones that hadn't started don't run, nothing is delivered after, and `dre` exits 124. Raise the timeout if the run is just slow, or look at which statement was running (the log names it).";
    ConnectionFailed = "connection-failed", Connection,
        "A connection couldn't be opened.",
        "The source plugin couldn't connect: the host is unreachable, the credentials are refused, the warehouse is unavailable. The message has the plugin's reason. Trying again can work when the cause is temporary.";
    QueryFailed = "query-failed", Query,
        "A query failed on the database.",
        "The database rejected or failed a statement: a syntax error, a missing table, a permission. The message names the file and line.";
    NoResultSet = "no-result-set", Config,
        "A query that makes a tab returned no result set.",
        "The query's last statement returned no rows to write (it created a view, set a variable). If it only prepares data for later queries, give it `tab: false` in the YAML.";
    RenderFailed = "render-failed", Config,
        "A template failed to render during the run.",
        "A destination, message or `when:` template failed to render with the run's results. The message has the cause.";
    FormatFailed = "format-failed", Plugin,
        "An output couldn't be written in its format.",
        "The format plugin failed to write the output (bad options, a template it can't fill, a result it can't represent). The message has the plugin's reason.";
    DeliveryFailed = "delivery-failed", Delivery,
        "An output couldn't be delivered.",
        "The destination plugin failed to deliver the file or message. The message has the plugin's reason; the output stays in the target path.";
    // -- plugins and packages ---------------------------------------------------------------
    InvalidPluginDeclaration = "invalid-plugin-declaration", Config,
        "A `plugins:` entry is wrong.",
        "Each entry is a package name, `name: \"<version>\"`, or a map with `name:` and one of `github:`, `local:`, `registry:`.";
    InvalidVersionConstraint = "invalid-version-constraint", Config,
        "A plugin's version constraint doesn't parse.",
        "Constraints are semver requirements such as `1.2.0`, `>=1.0` or `^1`.";
    ConflictingPluginConstraints = "conflicting-plugin-constraints", Config,
        "Two declarations of a plugin allow no common version.",
        "A package declared in several files must have constraints some version satisfies. Align them.";
    ConflictingPluginSources = "conflicting-plugin-sources", Config,
        "A plugin is declared with two sources.",
        "Every declaration of a package must agree on where it comes from (`github:`, `local:`, `registry:`).";
    MovedPluginDeclaration = "moved-plugin-declaration", Config,
        "Plugins are declared under an old key.",
        "Plugin packages are declared under `plugins:` (usually in dependencies.yml), not `destinations:`, `formats:` or a list of names under `sources:`.";
    UndeclaredPlugin = "undeclared-plugin", Plugin,
        "The project uses a plugin no declared package provides.",
        "A connection type, format or destination type needs a plugin package listed under `plugins:`. Add the package the message names, then run `dre deps`.";
    PluginNotInstalled = "plugin-not-installed", Plugin,
        "A declared plugin isn't installed.",
        "Run `dre deps` (or any command without `--no-auto-install`) to install the project's plugins.";
    PluginNotFound = "plugin-not-found", Plugin,
        "There's no plugin to check options against.",
        "The plugin isn't installed, so its options couldn't be checked. Run `dre deps`.";
    PluginInstallFailed = "plugin-install-failed", Plugin,
        "A plugin couldn't be installed.",
        "Downloading or verifying a plugin package failed. The message has the reason (network, checksum, no build for this platform).";
    OptionsUnchecked = "options-unchecked", Plugin,
        "A plugin's options weren't checked.",
        "The plugin is too old to describe its options, or couldn't be asked. Run `dre plugin update` to get a version that checks them.";
    InvalidPackages = "invalid-packages", Config,
        "`packages:` is wrong.",
        "`packages:` (in dependencies.yml or packages.yml) is a list of macro packages, each with `git:` and `revision:`, or `local:`.";
    InvalidPackage = "invalid-package", Config,
        "A macro package is broken.",
        "The package's folder or manifest couldn't be read. The message has the reason.";
    DuplicatePackage = "duplicate-package", Config,
        "Two macro packages have the same name.",
        "Macros are called through the package's name, so names are unique.";
    ConflictingPackages = "conflicting-packages", Config,
        "A macro package is declared twice with different revisions.",
        "Declare each package once, at one revision.";
    PackageMissing = "package-missing", Config,
        "A macro package isn't there.",
        "A local package's folder doesn't exist, or a git package isn't installed; run `dre deps`.";
    MisplacedPackages = "misplaced-packages", Config,
        "`packages:` is in the wrong file.",
        "Macro packages are declared in dependencies.yml or packages.yml at the project root.";
    PackageNameClash = "package-name-clash", Config,
        "A macro package's name is already taken.",
        "Package names are Jinja variables, so one can't share a name with a DRE function, a project macro or the project itself.";
}

/// Where the error codes reference is published; a code's section is `#<slug>`.
pub const REFERENCE_URL: &str = "https://getdre.com/docs/reference-error-codes/";

/// The error codes reference, `docs/reference-error-codes.md`: every code by kind, with its
/// summary and explanation. Generated; a test fails when the committed page is stale.
pub fn reference_page() -> String {
    let mut out = String::from(
        "---\ntitle: \"Error codes reference\"\ndescription: \"Every code DRE reports, what it means and how to fix it.\"\nsection: reference\nposition: 12\n---\n\n\
         # Error codes reference\n\n\
         <!-- Generated from crates/dre-core/src/codes.rs. Edit the registry, not this page. -->\n\n\
         Every problem DRE reports has a code: `error[unknown-key]: ...` in the console, `code` in \
         `--log-format json` events, `dre validate --json` and `run_results.json`. `dre explain <code>` prints \
         the explanation below. A plugin's own codes are namespaced by the plugin \
         (`sftp/host-key-mismatch`) and documented with it. Codes are stable: one is never reused \
         for something else.\n\n\
         Each code has a kind, which decides `dre`'s exit code and whether trying again can help.\n",
    );
    let mut kinds: Vec<Kind> = Code::ALL.iter().map(|c| c.kind()).collect();
    kinds.sort();
    kinds.dedup();
    for k in kinds {
        out.push_str(&format!("\n## {}\n", heading(k)));
        let mut codes: Vec<Code> = Code::ALL.iter().copied().filter(|c| c.kind() == k).collect();
        codes.sort_by_key(|c| c.as_str());
        for c in codes {
            out.push_str(&format!(
                "\n### {}\n\n{}\n\n{}\n",
                c.as_str(),
                c.summary(),
                c.explanation()
            ));
        }
    }
    out
}

fn heading(k: Kind) -> &'static str {
    match k {
        Kind::Usage => "Usage: how the command was called",
        Kind::Config => "Config: the project's files",
        Kind::Plugin => "Plugins",
        Kind::Refused => "Refused: unsafe statements",
        Kind::Connection => "Connections",
        Kind::Auth => "Authentication",
        Kind::Query => "Queries",
        Kind::Delivery => "Delivery",
        Kind::Internal => "Internal",
        Kind::Cancelled => "Cancelled runs",
        Kind::TimedOut => "Timeouts",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_unique_kebab_case_and_parse_back() {
        let mut seen = std::collections::BTreeSet::new();
        for c in Code::ALL {
            let s = c.as_str();
            assert!(seen.insert(s), "{s} twice");
            assert!(
                s.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "{s}"
            );
            assert_eq!(Code::parse(s), Some(*c));
            assert!(
                c.summary().ends_with('.') && c.explanation().ends_with(['.', ')']),
                "{s}"
            );
        }
    }

    #[test]
    fn codes_serialize_and_compare_as_their_slug() {
        assert_eq!(serde_json::to_value(Code::UnknownKey).unwrap(), "unknown-key");
        assert!(Code::UnknownKey == "unknown-key");
        assert_eq!(Code::UnknownKey.kind(), Kind::Config);
    }
}
