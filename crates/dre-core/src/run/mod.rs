//! The run path: turn a resolved project into delivered files.
//!
//! A run is [planned](plan::plan) first: which Bindings, each parsed once with the run's inputs,
//! every profile's entry, and how they're scheduled. Then each Binding runs through its stages
//! (see [`mode`]): **render** its SQL ([`compile`]), **query** each statement on its
//! connection's session, one per connection, in strict YAML order ([`query`]), **format** its
//! outputs into `target/run/` ([`format`]), **deliver** them ([`deliver`]) and **record** the
//! schema snapshot and `run_results.json` ([`record`]). A per-row output is made and delivered
//! row by row ([`per_row`]). The [`scheduler`] runs Bindings, and rows, at once. Everything that
//! happens is a [`RunEvent`] for the host's [`EventSink`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use arrow::datatypes::SchemaRef;
use chrono::NaiveDate;
use dre_protocol::msg::ColumnOptions;
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json};

use crate::codes::{Code, ErrorCode};
use crate::context::Context;
use crate::dates::Calendar;
use crate::delivery::Deliverer;
use crate::engine::{CancelToken, EventSink, Events, RunEvent, RunStore};
use crate::error::Error;
use crate::parse::ParsedBinding;
use crate::project::{Binding, Project, Report};
use crate::render::Renderer;
use crate::run_results::{self, Delivery};
use crate::sqlsplit::StatementKind;

mod binding;
mod compile;
mod deliver;
mod format;
mod mode;
pub(crate) mod per_row;
mod plan;
mod query;
mod record;
mod scheduler;
mod session;

pub use crate::engine::Level;
pub use crate::run_results::Status;
pub use mode::{Delivery as DeliveryMode, Mode, Results};
pub use plan::{Plan, Planned, choose_bindings, plan, unknown_schedule};

use mode::Stages;
use scheduler::Job;
use session::Pool;

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub selector: Option<String>,
    /// `--set <name>` or `--set all`.
    pub set: Option<String>,
    /// `--target` as given; the run's target is [`Project::target_name`].
    pub target: Option<String>,
    /// `--profile`: the inherited connection, for every Binding.
    pub profile: Option<String>,
    pub vars: BTreeMap<String, String>,
    /// `--schedule <name>`: run exactly the Bindings that schedule targets, with its vars.
    pub schedule: Option<String>,
    /// `--timezone` or `DRE_TIMEZONE`: above every configured `timezone:`.
    pub timezone: Option<String>,
    /// `run.date` (`DRE_RUN_DATE`).
    pub date: Option<NaiveDate>,
    /// The instant this run was scheduled for (`DRE_RUN_AT`): `run.now` and `run.scheduled_at`,
    /// and `run.date` in the run's timezone unless `date` is set.
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Where results come from and what happens to the outputs.
    pub mode: Mode,
    pub output_name: Option<String>,
    pub output_path: Option<String>,
    /// Optional isolated run store, plus typed rows under report/binding/query.json.
    pub output_json: Option<PathBuf>,
    /// How per-row outputs run.
    pub rows: RowOptions,
    /// `--rate`: every destination's rate for this run, over its `rate:`.
    pub rate: Option<crate::rate::Rate>,
    pub accept_schema_change: bool,
    /// Asks a person which Set of a report to run; `None` when nobody is at a terminal.
    pub choose_set: Option<SetChooser>,
    /// SHA-256 of the manifest this command wrote, for `run_results.json`.
    pub manifest_checksum: Option<String>,
    /// Stops the run: no further Binding, statement or delivery starts once it's cancelled.
    pub cancel: CancelToken,
    /// `--keep-runs` or `DRE_KEEP_RUNS`: above `flags: keep_runs`.
    pub keep_runs: Option<usize>,
    /// `--threads` or `DRE_THREADS`: how many Bindings may run at once in the whole run, over
    /// each connection's `threads:`.
    pub threads: Option<usize>,
}

/// How a run's per-row outputs run.
#[derive(Debug, Clone, Default)]
pub struct RowOptions {
    /// `--row-concurrency` or `DRE_ROW_CONCURRENCY`: rows of a per-row output at once.
    pub concurrency: Option<usize>,
    /// `--only-keys`: make only these rows of each per-row output.
    pub only_keys: Option<Vec<String>>,
    /// `--limit-rows`: make at most this many rows of each per-row output.
    pub limit: Option<usize>,
    /// `--preview --unmask`: show per-row values in the preview as they are.
    pub unmask: bool,
}

/// Asks which Set of a report to run, given its Sets; `Ok(None)` means all of them.
pub type ChooseSet = dyn Fn(&str, &[String]) -> Result<Option<String>, String> + Send + Sync;

/// Who answers when a report has several Sets and nothing picks one.
#[derive(Clone)]
pub struct SetChooser(Arc<ChooseSet>);

impl SetChooser {
    pub fn new(
        ask: impl Fn(&str, &[String]) -> Result<Option<String>, String> + Send + Sync + 'static,
    ) -> SetChooser {
        SetChooser(Arc::new(ask))
    }

    /// Which of `report`'s `sets` to run; `None`: all.
    pub fn ask(&self, report: &str, sets: &[String]) -> Result<Option<String>, String> {
        (self.0)(report, sets)
    }
}

impl std::fmt::Debug for SetChooser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetChooser")
    }
}

impl RunOptions {
    /// What the run was asked to do, for `run_results.json` and the log.
    pub fn params(&self, date: NaiveDate) -> run_results::Params {
        run_results::Params {
            selector: self.selector.clone(),
            set: self.set.clone(),
            schedule: self.schedule.clone(),
            target: self.target.clone(),
            profile: self.profile.clone(),
            vars: self.vars.clone(),
            run_date: date.to_string(),
            scheduled_at: self.scheduled_at.map(rfc3339),
            timezone: self.timezone.clone(),
            output_name: self.output_name.clone(),
            output_path: self.output_path.clone(),
            dry_run: self.mode.dry_run(),
            preview: self.mode.preview,
            accept_schema_change: self.accept_schema_change,
        }
    }
}

/// How far a per-row output (or a `dre release`) has got. A row is done once all its deliveries
/// are (delivered, held, skipped or failed).
#[derive(Debug, Clone, Serialize)]
pub struct RowProgress {
    /// What's progressing, e.g. ``output `email` ``.
    pub output: String,
    pub done: u64,
    pub failed: u64,
    pub total: u64,
    /// The rate the deliveries are paced at, if any: a ceiling, not the speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<crate::rate::Rate>,
    /// The last event for this output: every row is done, or the run stopped.
    pub finished: bool,
}

/// A message `--preview` shows.
#[derive(Debug, Clone, Serialize)]
pub struct ShownMessage {
    /// The output's `name:`.
    pub output: Option<String>,
    pub title: String,
    /// Portable Markdown.
    pub text: String,
    /// Length, the row sample, `when:` and each destination's limit.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindingOutcome {
    pub report: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    pub binding: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `error`'s code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    pub files: Vec<PathBuf>,
    /// One line describing what the Binding produced (result sets, rows, outputs, delivery).
    pub summary: String,
    /// The schedule this ran under (`--schedule`), and the vars it layered in.
    pub schedule: Option<String>,
    pub schedule_vars: Option<JsonMap<String, Json>>,
    /// Every var the Binding rendered with: its own, the schedule's, then `--var`.
    pub vars: JsonMap<String, Json>,
    /// The run's timezone (IANA name); empty when the Binding never started.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timezone: String,
    #[serde(skip)]
    pub elapsed: Duration,
}

/// What a compiled Binding would use and produce if it ran.
#[derive(Debug, Clone, Serialize)]
pub struct BindingPlan {
    pub report: String,
    pub set: Option<String>,
    /// Rendered SQL files, relative to the project.
    pub compiled: Vec<PathBuf>,
    /// The inherited connection, if any.
    pub profile: Option<String>,
    /// Each query's connection, its type and the sources it reads.
    pub queries: Vec<PlannedQuery>,
    pub target: String,
    pub format: String,
    /// The file written under `target/run/` (a single-table format writes one per result set
    /// when there are several).
    pub output: PathBuf,
    pub destinations: Vec<PlannedDestination>,
    /// Every output (the fields above repeat the first one).
    pub outputs: Vec<PlannedOutput>,
    pub schedules: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedOutput {
    pub name: Option<String>,
    pub format: String,
    /// The file written under `target/run/`.
    pub output: PathBuf,
    pub destinations: Vec<PlannedDestination>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedQuery {
    pub query: String,
    pub connection: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// The connection's entry: the run's target, else its own `target:`, else `dev`.
    pub target: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedDestination {
    pub profile: String,
    /// The destination type of its entry for this run; `None` for `deliver: false`.
    pub kind: Option<String>,
    /// Its entry: the run's target, else the profile's own `target:`, else `dev`.
    pub target: Option<String>,
    pub path: Option<String>,
    /// False for a `deliver: false` entry: the output stays in the target path.
    pub delivers: bool,
}

#[derive(Debug, Default)]
pub struct RunSummary {
    pub outcomes: Vec<BindingOutcome>,
    /// Why nothing ran: the selection failed (`invalid-selector`), or a profile has no entry for
    /// the run (`missing-target-entry`).
    pub error: Option<Error>,
    /// Why the run was cancelled, if it was.
    pub cancelled: Option<crate::engine::CancelReason>,
    /// Bindings that never started because the run was cancelled.
    pub not_run: usize,
}

impl RunSummary {
    pub fn failed(&self) -> bool {
        self.error.is_some()
            || self.cancelled.is_some()
            || self
                .outcomes
                .iter()
                .any(|o| matches!(o.status, Status::Error | Status::Cancelled | Status::TimedOut))
    }

    /// The code of what failed first: why nothing ran, else the first failed Binding's.
    pub fn error_code(&self) -> Option<ErrorCode> {
        if let Some(e) = &self.error {
            return Some(e.code.clone());
        }
        self.outcomes
            .iter()
            .find(|o| matches!(o.status, Status::Error | Status::Cancelled | Status::TimedOut))
            .map(|o| o.error_code.clone().unwrap_or(ErrorCode::Core(Code::RunFailed)))
            .or_else(|| {
                self.cancelled.map(|r| {
                    ErrorCode::Core(match r {
                        crate::engine::CancelReason::Timeout => Code::RunTimedOut,
                        _ => Code::RunCancelled,
                    })
                })
            })
    }

    /// The exit code (see docs/exit-codes.md): 130, 143 or 124 for a cancelled or timed-out run;
    /// 2 when nothing ran because the selection or a profile is wrong; 1 when a Binding failed;
    /// else 0.
    pub fn exit_code(&self) -> u8 {
        if let Some(r) = self.cancelled {
            return r.exit_code();
        }
        if self.error.is_some() {
            // Nothing ran: the selection or a profile is wrong.
            return self.error_code().map_or(2, |c| c.kind().exit_code());
        }
        let failed: Vec<&BindingOutcome> = self
            .outcomes
            .iter()
            .filter(|o| matches!(o.status, Status::Error | Status::Cancelled | Status::TimedOut))
            .collect();
        // Every Binding that failed was refused before it started (already running): nothing ran.
        if !failed.is_empty()
            && failed.iter().all(|o| {
                o.error_code
                    .as_ref()
                    .is_some_and(|c| c.kind() == crate::codes::Kind::Refused)
            })
            && self.outcomes.len() == failed.len()
        {
            return 2;
        }
        // The run happened; a failed Binding exits 1 whatever its kind.
        if failed.is_empty() { 0 } else { 1 }
    }
}

/// A profile a run uses, and its entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsedProfile {
    /// `connection` or `destination`.
    pub role: &'static str,
    pub profile: String,
    /// Its entry: the run's target, else the profile's own `target:`, else `dev`.
    pub target: String,
    /// False for a destination entry written `deliver: false`.
    pub deliver: bool,
}

/// The run's target and the entry each profile it uses picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunTargets {
    /// `target.name`.
    pub name: String,
    /// `--target`, `DRE_TARGET` or `default`.
    pub from: String,
    pub profiles: Vec<UsedProfile>,
}

impl RunTargets {
    /// `dev (default)`, then each profile whose entry differs: `; connection `warehouse`: prod`.
    pub fn line(&self) -> String {
        let mut out = format!("{} ({})", self.name, self.from);
        for p in self.profiles.iter().filter(|p| p.target != self.name) {
            out.push_str(&format!("; {} `{}`: {}", p.role, p.profile, p.target));
        }
        out
    }

    /// When every profile the run uses is on one target other than the run's, `target.name`
    /// probably isn't what the templates expect.
    pub fn mismatch(&self) -> Option<String> {
        let first = &self.profiles.first()?.target;
        if first == &self.name || self.profiles.iter().any(|p| &p.target != first) {
            return None;
        }
        Some(format!(
            "every profile is on `{first}` but the run's target is `{}`: pass `--target {first}` or set `{}`",
            self.name,
            crate::profiles::TARGET_ENV
        ))
    }
}

/// Run what `opts` selects: [`plan`] it, then run the plan on a worker thread while this thread
/// hands its events to `sink`.
///
/// # Panics
///
/// Panics if the worker thread panics (a bug), so the panic isn't lost.
pub fn run(ctx: &Context, project: &Project, opts: &RunOptions, sink: &mut dyn EventSink) -> RunSummary {
    let mut summary = RunSummary::default();
    let plan = match plan(ctx, project, opts, sink, &mut summary.outcomes) {
        Ok(p) => p,
        Err(e) => {
            summary.error = Some(e);
            return summary;
        }
    };
    sink.event(RunEvent::Targets(plan.targets.clone()));
    sink.event(RunEvent::Plan(plan.bindings.len()));
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = opts.cancel.clone();
    let outcomes = std::thread::scope(|s| {
        let worker = s.spawn(|| execute(ctx, project, &plan, opts, Events::new(tx)));
        for e in rx {
            sink.event(e);
        }
        worker.join().expect("the run's worker doesn't panic")
    });
    summary.not_run = plan.bindings.len().saturating_sub(outcomes.len());
    summary.cancelled = cancel.reason();
    summary.outcomes.extend(outcomes);
    summary
}

/// Run a plan's Bindings on its scheduler, reporting through `events`; a cancelled run starts
/// no further Binding. Bindings running at once hold their events and send them as one block,
/// so their lines don't interleave; outcomes come back in declaration order.
pub fn execute(
    ctx: &Context,
    project: &Project,
    plan: &Plan<'_>,
    opts: &RunOptions,
    events: Events,
) -> Vec<BindingOutcome> {
    let target = opts
        .output_json
        .as_ref()
        .map(|p| project.root().join(p))
        .unwrap_or_else(|| project.target_dir().to_path_buf());
    let at_once = plan.scheduler.workers() > 1;
    let store = if at_once {
        RunStore::concurrent(&target, ctx.secrets())
    } else {
        RunStore::new(&target, ctx.secrets())
    };
    let jobs = plan.bindings.iter().map(|p| {
        Ok::<_, std::convert::Infallible>(Job {
            uses: p.parsed.connections().into_iter().map(str::to_string).collect(),
            item: p,
        })
    });
    let run_one = |_: &mut (), _: usize, p: &Planned<'_>| {
        let events = if at_once { events.held() } else { events.clone() };
        if at_once {
            events.emit_now(RunEvent::Running {
                label: p.label(),
                running: true,
            });
        }
        events.emit(RunEvent::BindingStart {
            report: p.report.name.clone(),
            set: p.binding.set.clone(),
        });
        let outcome = BindingRun::new(ctx, project, p, opts, events.clone(), &store).run();
        events.emit(RunEvent::BindingEnd(outcome.clone()));
        if at_once {
            events.emit_now(RunEvent::Running {
                label: p.label(),
                running: false,
            });
            events.flush();
        }
        Some(outcome)
    };
    let ran = plan.scheduler.run(jobs, &opts.cancel, || (), run_one);
    ran.done.into_iter().map(|(_, o)| o).collect()
}

/// The tab one query produced: its last statement's result set, spooled to disk.
struct Produced {
    query: String,
    connection: String,
    schema: SchemaRef,
    rows: u64,
    spool: PathBuf,
    name: String,
    anchor: Option<String>,
    header: Option<bool>,
    columns: BTreeMap<String, ColumnOptions>,
    autofit: Option<bool>,
    style: Option<dre_protocol::style::SheetStyle>,
}

impl Produced {
    /// How a format plugin is told about this result set.
    fn meta(&self) -> dre_protocol::msg::ResultSetMeta {
        dre_protocol::msg::ResultSetMeta {
            name: self.name.clone(),
            query: self.query.clone(),
            result_index: 1,
            anchor: self.anchor.clone(),
            header: self.header,
            columns: self.columns.clone(),
            autofit: self.autofit,
            style: self.style.clone(),
        }
    }
}

struct Statement {
    query: String,
    /// The connection it runs on.
    connection: String,
    file: PathBuf,
    line: usize,
    text: String,
    kind: StatementKind,
    /// The complete rendered query contained a registered secret before it was split. Every
    /// statement inherits this because splitting can itself separate a secret into fragments.
    sensitive: bool,
    /// Whether this statement's result is its query's tab: the file's last statement, when the
    /// query has `tab: true`.
    tab: bool,
}

struct BindingRun<'a> {
    /// The run's context: its plugin host, secrets and caches.
    ctx: Context,
    project: &'a Project,
    report: &'a Report,
    b: &'a Binding,
    /// What each stage does, from the run's mode.
    stages: Stages,
    /// The Binding's vars with the schedule's layered on (`--var` stays separate, on top).
    vars: JsonMap<String, Json>,
    /// Everything `var()` sees, `--var` included: what's recorded.
    rendered_vars: JsonMap<String, Json>,
    schedule_vars: Option<JsonMap<String, Json>>,
    opts: &'a RunOptions,
    /// `run.date`: `DRE_RUN_DATE`, else today in the run's timezone.
    date: NaiveDate,
    calendar: Calendar,
    ui: Events,
    store: &'a RunStore,
    compiled_dir: PathBuf,
    /// The Binding's folder in `target/run/`, with its runs and `current` pointer.
    runs: crate::runs::BindingRuns,
    /// This run's id and folder (`runs/<run id>/`).
    run_id: String,
    run_dir: PathBuf,
    /// Whether this run becomes the current one when it finishes: false when a run for a later
    /// instant already is (it then also leaves the drift snapshot alone).
    becomes_current: bool,
    schema_dir: PathBuf,
    /// The run's target (environment).
    target: String,
    /// The parse pass for this run's inputs, from the plan.
    parsed: &'a ParsedBinding,
    /// Set when a query a template sent failed in the database.
    query_failed: Arc<AtomicBool>,
    /// The Binding's connections and their sessions.
    pool: Option<Arc<Pool>>,
    /// One renderer per connection (`None`: the inherited one, for paths and template values).
    renderers: BTreeMap<Option<String>, Arc<Renderer>>,
    started: Instant,
    started_at: chrono::DateTime<chrono::Utc>,
    produced: Vec<Produced>,
    /// Each of the Binding's outputs, in order: built from `b.outputs`, so an output's index
    /// (`oi`) reads both.
    outs: Vec<OutputRun>,
    /// Delivers the Binding's outputs, reusing a destination process per type.
    deliverer: Option<Deliverer<'a>>,
    drift: Vec<String>,
    /// The current schema with stable identities and any protection carried from the baseline.
    protected_snapshot: Option<Json>,
    /// `--retry-failed`: the run it redoes, and the rows each per-row output already finished.
    retry: Option<per_row::Retry>,
    /// Deliveries held for `dre release` (`hold: true`, `--hold`).
    held: Vec<crate::release::HeldEntry>,
    /// The zips built for `zip:` entries, by their files, name and password.
    zips: std::collections::HashMap<String, PathBuf>,
}

/// One output's progress through the run.
#[derive(Default)]
struct OutputRun {
    /// Its destinations with paths and options rendered.
    dests: Vec<RenderedDest>,
    /// Each file it wrote, and where it was first delivered.
    files: Vec<(PathBuf, Option<String>)>,
    /// One record per destination: `run_results.json`'s `deliveries`.
    deliveries: Vec<Delivery>,
    delivery_note: Option<String>,
    /// `None` until it's done.
    status: Option<OutputStatus>,
    error: Option<String>,
    /// The result of its `when:`, once evaluated.
    when: Option<bool>,
    /// A message output's rendered title and text.
    message: Option<(String, String)>,
    /// Why it was skipped or failed by design (`no_data`).
    reason: Option<String>,
    /// Its queries that returned no rows.
    no_data: Vec<String>,
    /// Empty queries left out (`on_empty: skip`).
    dropped: Vec<String>,
    /// A per-row output's summary.
    for_each: Option<run_results::ForEachResult>,
    /// A per-row output's files by row key, for a message that attaches it.
    row_files: std::collections::HashMap<String, Vec<PathBuf>>,
}

/// How one output ended, as `run_results.json` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputStatus {
    Delivered,
    /// It stays in the target path: no destination, `deliver: false`, or a preview.
    Kept,
    /// Its `when:` was false, or its message rendered empty.
    Skipped,
    Failed,
    /// Its deliveries are held for `dre release`.
    Held,
}

impl OutputStatus {
    fn as_str(self) -> &'static str {
        match self {
            OutputStatus::Delivered => "delivered",
            OutputStatus::Kept => "kept",
            OutputStatus::Skipped => "skipped",
            OutputStatus::Failed => "failed",
            OutputStatus::Held => "held",
        }
    }
}

/// A destination entry after rendering.
struct RenderedDest {
    profile: String,
    path: Option<String>,
    options: JsonMap<String, Json>,
    /// Other outputs whose files go with a message.
    attach: Vec<String>,
    /// `hold: true`: recorded for `dre release`, not delivered now.
    hold: bool,
    /// `rate:` (or `--rate`), recorded with a held delivery.
    rate: Option<crate::rate::Rate>,
    /// `zip:`, its password rendered.
    zip: Option<crate::zip_pack::ZipSpec>,
    /// The zip built for this entry, delivered instead of the output's files.
    zip_file: Option<PathBuf>,
}

/// Why a Binding (or one of its steps) failed.
type Fail = Error;

fn rel(root: &Path, p: &Path) -> PathBuf {
    crate::slash(p.strip_prefix(root).unwrap_or(p))
}

/// An instant as DRE records it: RFC 3339 in UTC, to the second.
pub fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// How a generated file is recorded in `run_results.json`: relative to the project root when
/// it's inside it, else relative to the target path, so moving the target folder doesn't
/// change what's recorded.
fn record_path(project: &Project, p: &Path) -> PathBuf {
    match p.strip_prefix(project.root()) {
        Ok(r) => crate::slash(r),
        Err(_) => rel(project.target_dir(), p),
    }
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

/// `dre clean`: remove the target folder `t`.
pub fn clean(t: &Path) -> std::io::Result<bool> {
    if t.exists() {
        std::fs::remove_dir_all(t)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
