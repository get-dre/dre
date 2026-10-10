//! The project manifest, `<target>/manifest.json`: everything a project declares, as the engine
//! resolves it, for orchestrators, CI and other tools. It's a deliberate projection of the loaded
//! [`Project`], not a dump: built offline (no connection, no profiles, no plugins), the same bytes
//! for the same project and the same inputs on every OS, and free of secrets and
//! machine-specific values.
//!
//! Like dbt's, it's resolved for the run's inputs: Jinja in `profile:` values and source fields
//! is rendered with the run's target, vars, environment variables and `run.*`, and each query's
//! sources and connection come from the parse pass. Two targets can give two manifests.
//!
//! The format is a public, versioned contract (`docs/manifest.md`; `docs/manifest.schema.json` is
//! generated from [`Manifest`]): adding optional fields keeps [`SCHEMA_VERSION`]; removing,
//! renaming or re-typing a field, or changing what one means, bumps it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json};
use sha2::{Digest, Sha256};

use crate::diag::{Diagnostics, Severity};
use crate::lookups::LOOKUPS_DIR;
use crate::project::{Binding, MACROS_DIR, PluginSource, Project, QueryEntry, REPORTS_DIR, Report};

/// The manifest format's version.
pub const SCHEMA_VERSION: &str = "dre/manifest/v3";
/// The manifest's file name in the target folder.
pub const FILE: &str = "manifest.json";

/// Per-report problems, by report name: they mark the report invalid in the manifest.
pub type ReportErrors = BTreeMap<String, Vec<String>>;

/// Where the manifest goes for `project`.
pub fn path(project: &Project) -> PathBuf {
    project.target_dir.join(FILE)
}

/// The whole project's manifest.
pub fn build(project: &Project, errors: &ReportErrors) -> Json {
    let schedules = project
        .schedules
        .iter()
        .map(|e| (e.name.clone(), schedule(project, &e.name)))
        .collect();
    document(
        project,
        project
            .reports
            .iter()
            .map(|r| (r, r.bindings.iter().collect()))
            .collect(),
        schedules,
        errors,
    )
}

/// A manifest-shaped document holding only `reports` (each with the given Bindings) and
/// `schedules`: what `dre ls --output json` prints.
pub fn subset(
    project: &Project,
    reports: Vec<(&Report, Vec<&Binding>)>,
    schedule_names: &[String],
    errors: &ReportErrors,
) -> Json {
    let schedules = schedule_names
        .iter()
        .map(|n| (n.clone(), schedule(project, n)))
        .collect();
    document(project, reports, schedules, errors)
}

// -- the format ---------------------------------------------------------------------------------

type Map = JsonMap<String, Json>;

/// target/manifest.json, written by `dre compile`, `dre validate` and `dre run`, and the document `dre ls --output json` prints (holding only the matching reports and schedules). Resolved for the run's inputs, as dbt's is: the target, vars, environment variables and run.* decide Jinja in `profile:` values and source fields, so the same project and the same inputs give the same bytes. New optional fields may appear within a version; consumers should ignore fields they don't know. See docs/manifest.md.
#[derive(Serialize, JsonSchema)]
#[schemars(title = "DRE project manifest")]
pub struct Manifest {
    /// The format's version: `dre/manifest/v3` (DRE 0.4). Version 2 (DRE 0.2) had `"schema": 2` instead.
    #[schemars(schema_with = "manifest_version")]
    pub schema_version: &'static str,
    /// The DRE version that wrote it.
    pub version: String,
    pub project: ManifestProject,
    /// By report name.
    pub reports: BTreeMap<String, ManifestReport>,
    /// By schedule name (schedules.yml).
    pub schedules: BTreeMap<String, Option<ManifestSchedule>>,
    /// Declared sources (`sources:`), by name.
    pub sources: BTreeMap<String, ManifestSource>,
    /// The plugin packages the project declares.
    pub plugins: Vec<ManifestPlugin>,
}

#[derive(Serialize, JsonSchema)]
pub struct ManifestProject {
    pub name: String,
    /// The run's target (environment), `target.name` in templates: --target, else DRE_TARGET, else `dev`. Each profile's own entry may differ (its `target:` in profiles.yml).
    pub target: String,
    /// The default connection's name, as written (it may hold Jinja).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// The project's `timezone:` (IANA name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Over the shared inputs: dre_project.yml, folder config, schedules.yml, dependencies.yml and other YAML that isn't a report's own, macros/, lookups/, and every .sql under reports/ that isn't a declared query. When it changes, every report may have changed.
    #[schemars(schema_with = "sha256")]
    pub checksum: String,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "report")]
pub struct ManifestReport {
    pub name: String,
    /// False for a bare .sql under reports/ (an unmanaged report).
    pub managed: bool,
    /// The defining YAML, or the .sql of an unmanaged report.
    #[schemars(schema_with = "path_schema")]
    pub file: String,
    /// Folder segments under reports/.
    pub folder: Vec<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_set: Option<String>,
    pub queries: Vec<ManifestQuery>,
    /// Every source any of the report's Bindings reads, `source.table`.
    pub depends_on: DependsOn,
    /// Over the defining file, every query file and any template.
    #[schemars(schema_with = "sha256")]
    pub checksum: String,
    pub valid: bool,
    /// Present when invalid: what's wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
    pub bindings: Vec<ManifestBinding>,
}

#[derive(Serialize, JsonSchema)]
pub struct DependsOn {
    /// `source.table` of every `source()` used, in order of first use.
    pub sources: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "query")]
pub struct ManifestQuery {
    pub query: String,
    #[schemars(schema_with = "path_schema")]
    pub file: String,
    /// The query's own `profile:`, as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// In a Binding: the connection the query runs on, from the parse pass (its own `profile:`, a source's, else the inherited one). Null when none resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<Option<String>>,
    /// In a Binding: what the query reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<DependsOn>,
    /// Whether its result becomes a tab (false: run for its effects only).
    pub tab: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    /// Per result column, how to show it (xlsx).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Json>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "binding")]
pub struct ManifestBinding {
    /// Null for a report without Sets.
    pub set: Option<String>,
    /// The inherited connection (Set, report, folder `+profile`, `default_profile`), rendered. A query's own connection is on the query.
    pub profile: Option<String>,
    /// Fully merged: project < folders < report < Set.
    pub vars: Map,
    pub queries: Vec<ManifestQuery>,
    /// The first output; every output is under `outputs`.
    pub output: ManifestOutput,
    /// Every output's destinations, in delivery order.
    pub destinations: Vec<ManifestDestination>,
    /// Every output, in declared order.
    pub outputs: Vec<ManifestOutput>,
    /// The schedules that run this Binding.
    pub schedules: Vec<String>,
}

#[derive(Serialize, JsonSchema, Clone, Default)]
pub struct ManifestOutput {
    /// The output's `name:`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub format: String,
    /// The queries it formats; absent means all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    /// The `when:` condition, unrendered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub options: Map,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// The template file, as declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "path_schema")]
    pub template: Option<String>,
    /// In delivery order. (Absent on a Binding's `output`.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destinations: Option<Vec<ManifestDestination>>,
}

#[derive(Serialize, JsonSchema, Clone)]
#[schemars(rename = "destination")]
pub struct ManifestDestination {
    /// The destination profile, rendered.
    pub profile: String,
    /// The path template, unrendered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A message output's entry: the outputs whose files go with the message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach: Option<Vec<String>>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "schedule")]
pub struct ManifestSchedule {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    /// The resolved timing: `cron`, `every` or `rrule`, with `starting`, `at`, `except` and `also` when given. With `timing`, the shared timing's fields.
    pub schedule: Map,
    /// The timings.yml entry the timing comes from, when it's a shared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    /// False when the schedule is paused (`enabled: false`).
    pub enabled: bool,
    pub vars: Map,
    /// The schedule's `timezone:`, or its shared timing's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// What `dre run --schedule <name>` runs.
    pub bindings: Vec<ScheduledBinding>,
}

#[derive(Serialize, JsonSchema)]
pub struct ScheduledBinding {
    pub report: String,
    pub set: Option<String>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "source")]
pub struct ManifestSource {
    pub name: String,
    /// The YAML file declaring it.
    #[schemars(schema_with = "path_schema")]
    pub file: String,
    /// The connection it lives on, rendered. Absent: it runs wherever its query runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Rendered. Absent: `source()` renders two parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Rendered; the source's name unless set.
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub meta: Map,
    /// By table name.
    pub tables: BTreeMap<String, ManifestTable>,
    /// Fields that didn't render (left as written).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
}

#[derive(Serialize, JsonSchema)]
pub struct ManifestTable {
    pub name: String,
    /// The real table name, rendered; the table's name unless set.
    pub identifier: String,
    /// Which parts `source()` quotes, the table's over the source's.
    #[schemars(schema_with = "quoting")]
    pub quoting: crate::render::Quoting,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub meta: Map,
    #[schemars(schema_with = "columns")]
    pub columns: Vec<crate::project::SourceColumn>,
    /// The reports that read it. Empty: unused.
    pub used_by: BTreeSet<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct ManifestPlugin {
    pub package: String,
    /// The version requirement (`*` for any).
    pub version: String,
    pub source: PluginSourceRecord,
}

#[derive(Serialize, JsonSchema)]
pub struct PluginSourceRecord {
    #[serde(rename = "type")]
    #[schemars(schema_with = "plugin_source_type")]
    pub kind: &'static str,
    /// Another registry's URL or path, a GitHub `owner/repo`, or a local executable's path. Absent for DRE's default registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

fn manifest_version(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"const": SCHEMA_VERSION})
}

fn sha256(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "pattern": "^[0-9a-f]{64}$"})
}

fn path_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "description": "Relative to the project root, with forward slashes."})
}

fn plugin_source_type(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"enum": ["registry", "github", "local"]})
}

fn quoting(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {"database": {"type": "boolean"}, "schema": {"type": "boolean"}, "identifier": {"type": "boolean"}},
        "required": ["database", "schema", "identifier"]
    })
}

fn columns(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {"name": {"type": "string"}, "description": {"type": "string"}, "data_type": {"type": "string"}},
            "required": ["name"]
        }
    })
}

// -- building it --------------------------------------------------------------------------------

fn document(
    project: &Project,
    reports: Vec<(&Report, Vec<&Binding>)>,
    schedules: BTreeMap<String, Option<ManifestSchedule>>,
    errors: &ReportErrors,
) -> Json {
    let reports = reports
        .into_iter()
        .map(|(r, bs)| (r.name.clone(), report(project, r, &bs, errors.get(&r.name))))
        .collect();
    let plugins = project
        .plugins
        .iter()
        .map(|p| {
            let (kind, location) = match &p.source {
                PluginSource::Default => ("registry", None),
                PluginSource::Registry(u) => ("registry", Some(u.clone())),
                PluginSource::Github(r) => ("github", Some(r.clone())),
                PluginSource::Local(l) => ("local", Some(l.clone())),
            };
            ManifestPlugin {
                package: p.name.clone(),
                version: p.version.clone(),
                source: PluginSourceRecord { kind, location },
            }
        })
        .collect();
    let doc = Manifest {
        schema_version: SCHEMA_VERSION,
        version: crate::version().to_string(),
        project: ManifestProject {
            name: project.name.clone(),
            target: project.target_name.clone(),
            default_profile: project.default_profile.clone(),
            timezone: project.timezone.clone(),
            checksum: project_checksum(project),
        },
        reports,
        schedules,
        sources: sources(project),
        plugins,
    };
    sorted(serde_json::to_value(doc).expect("a manifest serializes"))
}

fn report(
    project: &Project,
    r: &Report,
    bindings: &[&Binding],
    errors: Option<&Vec<String>>,
) -> ManifestReport {
    // Every source any Binding reads, as `depends_on` says it in dbt.
    let mut used: Vec<String> = Vec::new();
    for p in r.bindings.iter().filter_map(|b| b.parsed.as_ref()) {
        for k in p.source_keys() {
            if !used.iter().any(|u| u == k) {
                used.push(k.to_string());
            }
        }
    }
    let errors = errors.cloned().unwrap_or_default();
    ManifestReport {
        name: r.name.clone(),
        managed: r.managed,
        file: slash(&r.file),
        folder: r.folder.clone(),
        tags: r.tags.clone(),
        timezone: r.timezone.clone(),
        default_set: r.default_set.clone(),
        queries: r.queries.iter().map(|q| query(q, None)).collect(),
        depends_on: DependsOn { sources: used },
        checksum: report_checksum(project, r),
        valid: errors.is_empty(),
        errors: (!errors.is_empty()).then_some(errors),
        bindings: bindings.iter().map(|b| binding(b)).collect(),
    }
}

/// A query entry; with the parse pass's result (`parsed`), also its connection and sources.
fn query(q: &QueryEntry, parsed: Option<&crate::parse::ParsedQuery>) -> ManifestQuery {
    ManifestQuery {
        query: q.query.clone(),
        file: slash(&q.path),
        profile: q.profile.clone(),
        connection: parsed.map(|p| p.connection.clone()),
        depends_on: parsed.map(|p| DependsOn {
            sources: p.sources.clone(),
        }),
        tab: q.tab,
        tab_name: q.tab_name.clone(),
        anchor: q.anchor.clone(),
        header: q.header,
        columns: (!q.columns.is_empty()).then(|| serde_json::to_value(&q.columns).unwrap_or(Json::Null)),
    }
}

fn binding(b: &Binding) -> ManifestBinding {
    let parsed = b.parsed.as_deref();
    // Destinations are numbered across every output, as the parse pass renders them.
    let mut next = 0;
    let outputs: Vec<ManifestOutput> = b
        .outputs
        .iter()
        .map(|o| {
            let destinations = o
                .destinations
                .iter()
                .map(|d| {
                    let rendered = parsed.and_then(|p| p.destinations.get(next).cloned().flatten());
                    next += 1;
                    ManifestDestination {
                        profile: rendered.unwrap_or_else(|| d.profile.clone()),
                        path: d.path.clone(),
                        attach: (!d.attach.is_empty()).then(|| d.attach.clone()),
                    }
                })
                .collect();
            ManifestOutput {
                name: o.name.clone(),
                format: o.format.clone(),
                queries: o.queries.clone(),
                when: o.when.clone(),
                options: o.options.clone(),
                extension: o.extension.clone(),
                template: o.template.as_ref().map(|t| t.file.clone()),
                destinations: Some(destinations),
            }
        })
        .collect();
    let output = outputs
        .first()
        .map(|o| ManifestOutput {
            destinations: None,
            ..o.clone()
        })
        .unwrap_or_default();
    let destinations = outputs
        .iter()
        .flat_map(|o| o.destinations.clone().unwrap_or_default())
        .collect();
    ManifestBinding {
        set: b.set.clone(),
        profile: parsed.map_or(b.profile.clone(), |p| p.inherited.clone()),
        vars: b.vars.clone(),
        queries: b
            .queries
            .iter()
            .map(|q| query(q, parsed.and_then(|p| p.query(&q.query))))
            .collect(),
        output,
        destinations,
        outputs,
        schedules: b.schedules.clone(),
    }
}

/// Every declared source, with its fields rendered for the run's inputs (project vars, `--var`,
/// the target, and `run.*` with no report: the run date, but a fixed `run.now` unless
/// `DRE_RUN_AT` sets it, so the bytes don't change by the second). A field that doesn't render
/// is left as written, with the problem under `errors`.
fn sources(project: &Project) -> BTreeMap<String, ManifestSource> {
    if project.sources.is_empty() {
        return BTreeMap::new();
    }
    let inputs = &project.inputs;
    let calendar = crate::dates::Calendar {
        tz: inputs
            .timezone
            .as_deref()
            .or(project.timezone.as_deref())
            .and_then(|t| crate::dates::parse_tz(t).ok())
            .unwrap_or(chrono_tz::Tz::UTC),
        week_start: project.week_start,
        numbering: project.week_numbering,
    };
    let context = crate::render::RunContext {
        report: String::new(),
        set: None,
        target: project.target_name.clone(),
        schedule: None,
        date: crate::parse::run_date(&calendar, inputs.date, inputs.scheduled_at),
        now: inputs
            .scheduled_at
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        scheduled_at: inputs.scheduled_at,
        calendar,
        locale: Default::default(),
    };
    let limited = crate::render::Limited::new(context, project.vars.clone(), inputs.cli_vars.clone());
    let mut used: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &project.reports {
        for p in r.bindings.iter().filter_map(|b| b.parsed.as_ref()) {
            for k in p.source_keys() {
                used.entry(k.to_string()).or_default().insert(r.name.clone());
            }
        }
    }
    let mut out = BTreeMap::new();
    for (name, s) in &project.sources {
        let mut errors: Vec<String> = Vec::new();
        let mut render = |key: &str, v: &str| match limited.render(&format!("`{key}`"), v) {
            Ok(r) => r,
            Err(e) => {
                errors.push(e);
                v.to_string()
            }
        };
        let profile = s.profile.as_ref().map(|p| render("profile", p));
        let database = s.database.as_ref().map(|d| render("database", d));
        let schema = render("schema", s.schema.as_deref().unwrap_or(name));
        let tables = s
            .tables
            .iter()
            .map(|t| {
                let key = format!("{name}.{}", t.name);
                let table = ManifestTable {
                    name: t.name.clone(),
                    identifier: render("identifier", t.identifier.as_deref().unwrap_or(&t.name)),
                    quoting: t.quoting.over(&s.quoting),
                    description: t.description.clone(),
                    tags: t.tags.clone(),
                    meta: t.meta.clone(),
                    columns: t.columns.clone(),
                    used_by: used.get(&key).cloned().unwrap_or_default(),
                };
                (t.name.clone(), table)
            })
            .collect();
        out.insert(
            name.clone(),
            ManifestSource {
                name: name.clone(),
                file: slash(&s.file),
                profile,
                database,
                schema,
                description: s.description.clone(),
                tags: s.tags.clone(),
                meta: s.meta.clone(),
                tables,
                errors: (!errors.is_empty()).then_some(errors),
            },
        );
    }
    out
}

fn schedule(project: &Project, name: &str) -> Option<ManifestSchedule> {
    let e = project.schedules.iter().find(|e| e.name == name)?;
    let bindings = project
        .reports
        .iter()
        .flat_map(|r| {
            r.bindings
                .iter()
                .filter(|b| b.schedules.iter().any(|s| s == name))
                .map(|b| ScheduledBinding {
                    report: r.name.clone(),
                    set: b.set.clone(),
                })
        })
        .collect();
    Some(ManifestSchedule {
        name: e.name.clone(),
        report: e.report.clone(),
        select: e.select.clone(),
        set: e.set.clone(),
        schedule: e.schedule.clone(),
        timing: e.timing.clone(),
        enabled: e.enabled,
        vars: e.vars.clone(),
        timezone: e.timezone.clone(),
        bindings,
    })
}

fn slash(p: &Path) -> String {
    crate::slash(p).to_string_lossy().into_owned()
}

/// `v` as compact JSON with every object's keys sorted: the same bytes for the same value.
pub fn canonical(v: &Json) -> String {
    serde_json::to_string(&sorted(v.clone())).unwrap()
}

/// `v` with every object's keys sorted, so the bytes don't depend on insertion order.
fn sorted(v: Json) -> Json {
    match v {
        Json::Object(m) => {
            let mut entries: Vec<(String, Json)> = m.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Json::Array(a) => Json::Array(a.into_iter().map(sorted).collect()),
        v => v,
    }
}

// -- checksums --------------------------------------------------------------------------------

/// SHA-256 (hex) over `files` (relative to the root), in sorted order, each as its path and
/// contents with a fixed separator; a missing file hashes as its path alone.
fn checksum(root: &Path, files: &BTreeSet<String>) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.as_bytes());
        h.update([0u8]);
        match std::fs::read(root.join(f)) {
            Ok(bytes) => {
                h.update([1u8]);
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update([0u8]),
        }
    }
    hex(&h.finalize())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A report's own files: its defining file, every query file of any Binding, and templates.
fn report_files(project: &Project, r: &Report) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    files.insert(slash(&r.file));
    for q in r
        .queries
        .iter()
        .chain(r.bindings.iter().flat_map(|b| b.queries.iter()))
    {
        files.insert(slash(&q.path));
    }
    for o in r.bindings.iter().flat_map(|b| b.outputs.iter()) {
        if let Some(t) = &o.template {
            files.insert(template_file(&project.root, &t.file));
        }
        // A message's text template counts like an xlsx template.
        if let Some(f) = o
            .options
            .get("file")
            .and_then(Json::as_str)
            .filter(|_| o.is_message())
        {
            files.insert(template_file(&project.root, f));
        }
    }
    files
}

/// Where a template file is read from (the run's rule): the root, else `templates/`.
fn template_file(root: &Path, file: &str) -> String {
    let direct = Path::new(file);
    let under = Path::new("templates").join(file);
    if !root.join(direct).is_file() && root.join(&under).is_file() {
        slash(&under)
    } else {
        slash(direct)
    }
}

fn report_checksum(project: &Project, r: &Report) -> String {
    checksum(&project.root, &report_files(project, r))
}

/// The shared inputs: `dre_project.yml`, folder config, `schedules.yml`, `dependencies.yml` and
/// other YAML that isn't a report's own, `macros/`, `lookups/`, and every `.sql` under
/// `reports/` that isn't a declared query (the usual `ref()` targets).
fn project_checksum(project: &Project) -> String {
    // An unmanaged report's `.sql` stays in: it's a `ref()` target like any other.
    let owned: BTreeSet<String> = project
        .reports
        .iter()
        .flat_map(|r| {
            let mut files = report_files(project, r);
            if !r.managed {
                files.remove(&slash(&r.file));
            }
            files
        })
        .collect();
    let files: BTreeSet<String> = project
        .files
        .iter()
        .map(|p| slash(p))
        .filter(|p| !owned.contains(p))
        .filter(|p| {
            let p = Path::new(p);
            let yaml = matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml"));
            yaml || p.starts_with(MACROS_DIR)
                || p.starts_with(LOOKUPS_DIR)
                || (p.starts_with(REPORTS_DIR) && p.extension().is_some_and(|e| e == "sql"))
        })
        .collect();
    checksum(&project.root, &files)
}

// -- errors -----------------------------------------------------------------------------------

/// The error diagnostics that belong to a report: those in one of its own files.
pub fn report_errors(project: &Project, diags: &Diagnostics) -> ReportErrors {
    let mut out = ReportErrors::new();
    for r in &project.reports {
        let files = report_files(project, r);
        let msgs: Vec<String> = diags
            .sorted()
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .filter(|d| d.file.as_ref().is_some_and(|f| files.contains(&slash(f))))
            .map(|d| d.to_string())
            .chain(
                project
                    .parse_errors
                    .get(&r.name)
                    .into_iter()
                    .flatten()
                    .map(|d| d.to_string()),
            )
            .collect();
        if !msgs.is_empty() {
            out.insert(r.name.clone(), msgs);
        }
    }
    out
}

// -- writing ----------------------------------------------------------------------------------

/// The manifest's text: pretty JSON, secrets masked, newline-terminated.
pub fn render(doc: &Json) -> String {
    crate::secrets::to_json_pretty(doc).unwrap() + "\n"
}

/// Write `project`'s manifest atomically ([`crate::fs::write_atomic`]). Returns the SHA-256 of
/// the bytes written.
pub fn write(project: &Project, errors: &ReportErrors) -> Result<String, String> {
    let text = render(&build(project, errors));
    let dir = &project.target_dir;
    crate::target::ensure(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let dst = dir.join(FILE);
    crate::fs::write_atomic(&dst, text.as_bytes())
        .map_err(|e| format!("can't write {}: {e}", dst.display()))?;
    Ok(hex(&Sha256::digest(text.as_bytes())))
}

/// Remove a stale manifest from `target_dir` (the project didn't load).
pub fn remove(target_dir: &Path) {
    let _ = std::fs::remove_file(target_dir.join(FILE));
}
