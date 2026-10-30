//! The run path: turn a resolved project into delivered files.
//!
//! Per Binding: parse pass → render → split → (unmanaged check) → execute, each query on its
//! connection's session (one per connection, opened on first use, in strict YAML order) →
//! format into `target/run/` → schema-drift check → deliver → snapshot → `run_results.json`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use chrono::NaiveDate;
use dre_protocol::host::{Execution, HostError, LogSink};
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use dre_protocol::{CAP_LOAD, CAP_MESSAGE, CAP_READ_ONLY, CAP_SESSIONS};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json, json};

use crate::codes::{Code, ErrorCode};
use crate::context::Context;
use crate::dates::Calendar;
use crate::delivery::{Deliverer, Request, Sent};
use crate::engine::{CancelToken, Events, RunEvent, RunStore};
use crate::formats::{self, Files, Format};
use crate::lookups::Table;
use crate::message::QueryResult;
use crate::parse::ParsedBinding;
use crate::plugin_host::{Described, Open, Plugin, PluginHost, WriteRequest};
use crate::profiles::{Entry, LOCAL_TYPE, ProfileTarget, Profiles, Role};
use crate::project::{Binding, PluginKind, Project, QueryEntry, Report};
use crate::render::{
    Column, Connection, Connections, Mode, QueryRows, QueryRunner, RenderError, Renderer, RendererConfig,
};
use crate::run_results::{self, Delivery, RunResults};
use crate::secrets::Secrets;
use crate::sqlsplit::{self, StatementKind};

pub(crate) mod per_row;

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
    pub output_name: Option<String>,
    pub output_path: Option<String>,
    pub dry_run: bool,
    pub no_deliver: bool,
    /// Optional isolated run store, plus typed rows under report/binding/query.json.
    pub output_json: Option<PathBuf>,
    /// `--from-results <DIR>`: replay results saved with `--output-json` instead of running any
    /// query (implies `no_deliver`).
    pub from_results: Option<PathBuf>,
    /// `--row-concurrency` or `DRE_ROW_CONCURRENCY`: rows of a per-row output at once.
    pub row_concurrency: Option<usize>,
    /// `--hold`: hold every delivery for `dre release`.
    pub hold: bool,
    /// `--retry-failed <run id>`: redo that run's failed and never-started rows of its per-row
    /// outputs, from its saved results.
    pub retry_failed: Option<String>,
    /// `--only-keys`: make only these rows of each per-row output.
    pub only_keys: Option<Vec<String>>,
    /// `--limit-rows`: make at most this many rows of each per-row output.
    pub limit_rows: Option<usize>,
    /// `--preview --unmask`: show per-row values in the preview as they are.
    pub unmask: bool,
    /// `--rate`: every destination's rate for this run, over its `rate:`.
    pub rate: Option<crate::rate::Rate>,
    /// `--preview [N]`: row limit per query.
    pub preview: Option<u64>,
    pub accept_schema_change: bool,
    /// Whether a person is at a terminal to answer prompts.
    pub interactive: bool,
    /// `run.date` (`DRE_RUN_DATE`).
    pub date: Option<NaiveDate>,
    /// The instant this run was scheduled for (`DRE_RUN_AT`): `run.now` and `run.scheduled_at`,
    /// and `run.date` in the run's timezone unless `date` is set.
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `validate --live`: check statements instead of executing them.
    pub live_check: bool,
    /// `--schedule <name>`: run exactly the Bindings that schedule targets, with its vars.
    pub schedule: Option<String>,
    /// `--timezone` or `DRE_TIMEZONE`: above every configured `timezone:`.
    pub timezone: Option<String>,
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
            dry_run: self.dry_run,
            preview: self.preview,
            accept_schema_change: self.accept_schema_change,
        }
    }
}

/// How much a message matters: `Info` is shown by default, `Debug` with `-v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
}

/// How the run reports progress. The engine emits structured events; the CLI decides how
/// they look (colour, progress bar, JSON, log file).
pub trait Ui {
    /// The run's target and each used profile's entry, once they're all known to exist.
    fn targets(&mut self, _targets: &RunTargets) {}
    /// The number of Bindings about to run, once Sets are resolved.
    fn plan(&mut self, _bindings: usize) {}
    fn binding_start(&mut self, _report: &str, _set: Option<&str>) {}
    /// The running Binding's log file, in its run folder (a real run only).
    fn binding_log(&mut self, _path: &Path) {}
    /// Right after `binding_start`: the schedule it runs under (if any) and every var it uses.
    fn binding_vars(
        &mut self,
        _schedule: Option<&str>,
        _schedule_vars: Option<&JsonMap<String, Json>>,
        _vars: &JsonMap<String, Json>,
    ) {
    }
    /// One step inside the current Binding: a short verb, a detail, and how long it took.
    fn step(&mut self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>);
    fn warn(&mut self, msg: &str);
    fn binding_end(&mut self, _outcome: &BindingOutcome) {}
    /// A Binding was compiled (`--dry-run`, `dre compile`, `dre validate`): what it would do.
    fn compiled(&mut self, _plan: &BindingPlan) {}
    /// `--preview`: a rendered message, shown instead of sent.
    fn message(&mut self, _message: &ShownMessage) {}
    /// Ask which Set to run; `None` means "all".
    fn choose_set(&mut self, report: &str, sets: &[String]) -> Result<Option<String>, String>;
    /// Bindings running at once: `label` started (`running`) or finished. Its own events come
    /// later, together.
    fn running(&mut self, _label: &str, _running: bool) {}
    /// A per-row output's or a release's progress: rows done so far, sent as each one finishes.
    fn rows(&mut self, _progress: &RowProgress) {}
    /// Where plugin stderr goes.
    fn plugin_log(&self) -> LogSink;
    /// Where the full text of every statement sent to a source goes: `(label, sql)`.
    fn sql_log(&self) -> LogSink {
        Arc::new(|_, _| {})
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

pub use crate::run_results::Status;

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
    /// Selection failed, or a profile has no entry for the run: nothing ran.
    pub error: Option<String>,
    /// `error` is about a profile's missing entry.
    pub missing_entry: bool,
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
        if self.error.is_some() {
            return Some(ErrorCode::Core(if self.missing_entry {
                Code::MissingTargetEntry
            } else {
                Code::InvalidSelector
            }));
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

/// What a run will do: the Bindings to run, and the run's target with every profile's entry.
pub struct Plan<'a> {
    pub bindings: Vec<(&'a Report, Binding)>,
    pub targets: RunTargets,
}

/// Run what `opts` selects: [`plan`] it, then run the plan on a worker thread while this thread
/// shows its events through `ui`.
///
/// # Panics
///
/// Panics if the worker thread panics (a bug), so the panic isn't lost.
pub fn run(ctx: &Context, project: &Project, opts: &RunOptions, ui: &mut dyn Ui) -> RunSummary {
    let mut summary = RunSummary::default();
    let Some(plan) = plan(ctx, project, opts, ui, &mut summary) else {
        return summary;
    };
    ui.targets(&plan.targets);
    ui.plan(plan.bindings.len());
    let (plugin_log, sql_log) = (ui.plugin_log(), ui.sql_log());
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = opts.cancel.clone();
    let outcomes = std::thread::scope(|s| {
        let worker = s.spawn(|| execute(ctx, project, &plan, opts, &cancel, Events::new(tx)));
        for e in rx {
            crate::engine::dispatch(ui, e, &plugin_log, &sql_log);
        }
        worker.join().expect("the run's worker doesn't panic")
    });
    summary.not_run = plan.bindings.len().saturating_sub(outcomes.len());
    summary.cancelled = cancel.reason();
    summary.outcomes.extend(outcomes);
    summary
}

/// Which Bindings `opts` selects, and every profile's entry for the run. Asking which Set to run
/// happens here, through `ui`. A selection that fails for one report is recorded as that
/// report's failed outcome in `summary`; a failure that stops the whole run is `None`, with
/// `summary.error` set.
pub fn plan<'a>(
    ctx: &Context,
    project: &'a Project,
    opts: &RunOptions,
    ui: &mut dyn Ui,
    summary: &mut RunSummary,
) -> Option<Plan<'a>> {
    if let Err(e) = crate::target::ensure(project.target_dir()) {
        summary.error = Some(format!(
            "can't write to the target path {} (from {}): {e}",
            project.target_dir().display(),
            project.target_source()
        ));
        return None;
    }
    let planned: Vec<(&Report, Binding)> = match &opts.schedule {
        Some(name) => {
            if !project.schedule(name).is_some() {
                summary.error = Some(unknown_schedule(project, name));
                return None;
            }
            match schedule_bindings(project, name, opts) {
                Ok(p) => p,
                Err(e) => {
                    summary.error = Some(e);
                    return None;
                }
            }
        }
        None => {
            let reports: Vec<&Report> = match &opts.selector {
                None => project.reports().iter().collect(),
                Some(s) => match project.select(s) {
                    Ok(r) if r.is_empty() => {
                        summary.error = Some(format!("selector `{s}` matches no report"));
                        return None;
                    }
                    Ok(r) => r,
                    Err(e) => {
                        summary.error = Some(e.to_string());
                        return None;
                    }
                },
            };
            // Resolve every report's Bindings first (prompts happen here), so progress has a
            // total.
            let mut planned = Vec::new();
            for report in reports {
                match choose_bindings(project, report, opts, ui) {
                    Ok(bs) => planned.extend(bs.into_iter().map(|b| (report, b))),
                    Err(e) => {
                        let outcome = BindingOutcome {
                            report: report.name.clone(),
                            set: None,
                            binding: "-".into(),
                            status: Status::Error,
                            error: Some(e),
                            error_code: Some(Code::InvalidSelector.into()),
                            files: Vec::new(),
                            summary: String::new(),
                            schedule: opts.schedule.clone(),
                            schedule_vars: None,
                            vars: JsonMap::new(),
                            timezone: String::new(),
                            elapsed: Duration::ZERO,
                        };
                        ui.binding_end(&outcome);
                        summary.outcomes.push(outcome);
                    }
                }
            }
            planned
        }
    };
    // Every profile the run uses needs an entry for it, before anything runs.
    match run_targets(ctx, project, &planned, opts) {
        Ok(targets) => Some(Plan {
            bindings: planned,
            targets,
        }),
        Err(e) => {
            summary.error = Some(e);
            summary.missing_entry = true;
            None
        }
    }
}

/// Run a plan's Bindings, reporting through `events`; a cancelled run starts no further
/// Binding. One at a time, in order, unless a connection's entry sets `threads:` above 1 or the
/// run sets `--threads` (see [`Limits`]).
///
/// # Panics
///
/// Panics if a Binding running at once with others panics (a bug): the shared state's lock is
/// then poisoned.
pub fn execute(
    ctx: &Context,
    project: &Project,
    plan: &Plan<'_>,
    opts: &RunOptions,
    cancel: &CancelToken,
    events: Events,
) -> Vec<BindingOutcome> {
    let limits = Limits::new(ctx, project, plan, opts);
    let target = opts
        .output_json
        .as_ref()
        .map(|p| project.root().join(p))
        .unwrap_or_else(|| project.target_dir().to_path_buf());
    if limits.sequential() {
        let store = RunStore::new(&target, ctx.secrets());
        let mut outcomes = Vec::new();
        for (report, b) in &plan.bindings {
            if cancel.is_cancelled() {
                break;
            }
            events.emit(RunEvent::BindingStart {
                report: report.name.clone(),
                set: b.set.clone(),
            });
            let mut r = BindingRun::new(ctx, project, report, b, opts, events.clone(), &store);
            let outcome = r.run();
            events.emit(RunEvent::BindingEnd(outcome.clone()));
            outcomes.push(outcome);
        }
        return outcomes;
    }
    let store = RunStore::concurrent(&target, ctx.secrets());
    let n = plan.bindings.len();
    let state = std::sync::Mutex::new(Running::new(n));
    let wake = std::sync::Condvar::new();
    let outcomes: std::sync::Mutex<Vec<Option<BindingOutcome>>> = std::sync::Mutex::new(vec![None; n]);
    std::thread::scope(|s| {
        for _ in 0..limits.cap.min(n) {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut st = state.lock().unwrap();
                        loop {
                            if cancel.is_cancelled() || st.started.iter().all(|x| *x) {
                                break None;
                            }
                            match limits.next(&st) {
                                Some(i) => {
                                    st.start(i, &limits.uses[i]);
                                    break Some(i);
                                }
                                None => st = wake.wait(st).unwrap(),
                            }
                        }
                    };
                    let Some(i) = i else {
                        wake.notify_all();
                        return;
                    };
                    let (report, b) = &plan.bindings[i];
                    let label = match &b.set {
                        Some(set) => format!("{} [{set}]", report.name),
                        None => report.name.clone(),
                    };
                    let held = events.held();
                    held.emit_now(RunEvent::Running {
                        label: label.clone(),
                        running: true,
                    });
                    held.emit(RunEvent::BindingStart {
                        report: report.name.clone(),
                        set: b.set.clone(),
                    });
                    let mut r = BindingRun::new(ctx, project, report, b, opts, held.clone(), &store);
                    let outcome = r.run();
                    held.emit(RunEvent::BindingEnd(outcome.clone()));
                    held.emit_now(RunEvent::Running {
                        label,
                        running: false,
                    });
                    held.flush();
                    outcomes.lock().unwrap()[i] = Some(outcome);
                    state.lock().unwrap().finish(&limits.uses[i]);
                    wake.notify_all();
                }
            });
        }
    });
    // Declaration order, whatever order they finished in.
    outcomes.into_inner().unwrap().into_iter().flatten().collect()
}

/// How many Bindings may run at once: each connection entry's `threads:` (default 1; a DuckDB
/// file is always 1, as only one process can write it), and `--threads` (`DRE_THREADS`) over
/// the whole run.
struct Limits {
    /// The connections each Binding uses, by the Binding's index in the plan. `Running::started`
    /// and the run's outcomes have the same length, so one index reads all three.
    uses: Vec<Vec<String>>,
    per_connection: BTreeMap<String, usize>,
    cap: usize,
}

/// Which Bindings have started, and what's running on each connection.
struct Running {
    started: Vec<bool>,
    total: usize,
    on: BTreeMap<String, usize>,
    /// Connections a Binding has used to the end: until then, a connection runs one Binding,
    /// so its sign-in (a browser, a token refresh) happens once.
    warm: std::collections::BTreeSet<String>,
}

impl Running {
    fn new(n: usize) -> Running {
        Running {
            started: vec![false; n],
            total: 0,
            on: BTreeMap::new(),
            warm: Default::default(),
        }
    }

    fn start(&mut self, i: usize, uses: &[String]) {
        self.started[i] = true;
        self.total += 1;
        for c in uses {
            *self.on.entry(c.clone()).or_default() += 1;
        }
    }

    fn finish(&mut self, uses: &[String]) {
        self.total -= 1;
        for c in uses {
            *self.on.entry(c.clone()).or_default() -= 1;
            self.warm.insert(c.clone());
        }
    }
}

impl Limits {
    fn new(ctx: &Context, project: &Project, plan: &Plan<'_>, opts: &RunOptions) -> Limits {
        let schedule_vars = opts
            .schedule
            .as_ref()
            .and_then(|n| project.schedule(n))
            .map(|e| e.vars.clone());
        let inputs = crate::parse::Inputs {
            target: project.target_name().to_string(),
            cli_vars: opts.vars.clone(),
            date: opts.date,
            scheduled_at: opts.scheduled_at,
            timezone: opts.timezone.clone(),
            schedule: opts.schedule.clone(),
            started_at: None,
            process: ctx.clone(),
        };
        let uses: Vec<Vec<String>> = plan
            .bindings
            .iter()
            .map(|(report, b)| {
                let mut vars = b.vars.clone();
                vars.extend(schedule_vars.clone().unwrap_or_default());
                crate::parse::binding(project, report, b, &vars, &inputs)
                    .connections()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .collect();
        let mut per_connection = BTreeMap::new();
        for c in uses.iter().flatten() {
            per_connection
                .entry(c.clone())
                .or_insert_with(|| connection_threads(project, c));
        }
        // Only a real `dre run` (`--preview` included) runs Bindings at once.
        let cap = if opts.dry_run || opts.live_check {
            1
        } else {
            opts.threads.unwrap_or(usize::MAX).max(1)
        };
        Limits {
            uses,
            per_connection,
            cap,
        }
    }

    /// One at a time: `--threads 1`, or no `--threads` and no connection above 1.
    fn sequential(&self) -> bool {
        self.cap == 1 || (self.cap == usize::MAX && self.per_connection.values().all(|n| *n <= 1))
    }

    /// The first Binding, in declaration order, that may start now.
    fn next(&self, st: &Running) -> Option<usize> {
        if st.total >= self.cap {
            return None;
        }
        (0..st.started.len()).find(|&i| {
            !st.started[i]
                && self.uses[i].iter().all(|c| {
                    let on = st.on.get(c).copied().unwrap_or(0);
                    let limit = if st.warm.contains(c) {
                        self.per_connection[c]
                    } else {
                        1
                    };
                    on < limit
                })
        })
    }
}

/// A connection entry's `threads:` (default 1); a DuckDB file is 1 whatever it says.
fn connection_threads(project: &Project, connection: &str) -> usize {
    let Some(t) = project.profiles().target(Role::Connection, connection) else {
        return 1;
    };
    let n = match t.fields.get("threads") {
        Some(Json::Number(n)) => n.as_u64().unwrap_or(1),
        Some(Json::String(s)) => s.trim().parse().unwrap_or(1),
        _ => 1,
    } as usize;
    let duckdb_file = t.kind == "duckdb"
        && t.fields
            .get("path")
            .and_then(Json::as_str)
            .is_some_and(|p| p != ":memory:");
    if duckdb_file { 1 } else { n.max(1) }
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

/// The profiles the planned Bindings use (by the parse pass, with this run's inputs) and each
/// one's entry. A profile without an entry for the run is an error naming them all; profiles
/// nothing planned uses aren't checked.
pub fn run_targets(
    ctx: &Context,
    project: &Project,
    planned: &[(&Report, Binding)],
    opts: &RunOptions,
) -> Result<RunTargets, String> {
    let profiles = project.profiles();
    let schedule_vars = opts
        .schedule
        .as_ref()
        .and_then(|n| project.schedule(n))
        .map(|e| e.vars.clone());
    let inputs = crate::parse::Inputs {
        target: project.target_name().to_string(),
        cli_vars: opts.vars.clone(),
        date: opts.date,
        scheduled_at: opts.scheduled_at,
        timezone: opts.timezone.clone(),
        schedule: opts.schedule.clone(),
        started_at: None,
        process: ctx.clone(),
    };
    let mut used: Vec<(Role, String)> = Vec::new();
    for (report, b) in planned {
        let mut vars = b.vars.clone();
        vars.extend(schedule_vars.clone().unwrap_or_default());
        // A Binding whose parse fails reports that when it runs.
        let parsed = crate::parse::binding(project, report, b, &vars, &inputs);
        let names = parsed
            .connections()
            .into_iter()
            .map(|c| (Role::Connection, c.to_string()))
            .chain(
                parsed
                    .destinations
                    .values()
                    .flatten()
                    .filter(|_| !opts.no_deliver)
                    .filter(|d| !profiles.is_builtin_local(d))
                    .map(|d| (Role::Destination, d.clone())),
            );
        for u in names {
            if !used.contains(&u) {
                used.push(u);
            }
        }
    }
    let mut missing = Vec::new();
    let mut out = Vec::new();
    for (role, name) in &used {
        let deliver = match profiles.entry(*role, name) {
            Entry::Use(_) => true,
            Entry::Nowhere => false,
            Entry::Missing => {
                missing.push(profiles.missing_entry(*role, name));
                continue;
            }
            // Reported by the load.
            Entry::Unknown => continue,
        };
        out.push(UsedProfile {
            role: role.as_str(),
            profile: name.clone(),
            target: profiles.target_of(*role, name),
            deliver,
        });
    }
    match missing.as_slice() {
        [] => Ok(RunTargets {
            name: project.target_name().to_string(),
            from: project.run_target().from.to_string(),
            profiles: out,
        }),
        [one] => Err(format!("{one}; nothing was run")),
        all => Err(format!(
            "{} profiles this run uses have no entry for it; nothing was run:\n    {}",
            all.len(),
            all.join("\n    ")
        )),
    }
}

/// The Bindings `--schedule <name>` runs: all of them, or those a selector and/or `--set` pick.
/// Picking something the schedule doesn't run is an error naming what it does run.
fn schedule_bindings<'a>(
    project: &'a Project,
    name: &str,
    opts: &RunOptions,
) -> Result<Vec<(&'a Report, Binding)>, String> {
    let reports: Option<Vec<&str>> = match &opts.selector {
        None => None,
        Some(s) => Some(
            project
                .select(s)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|r| r.name.as_str())
                .collect(),
        ),
    };
    let set = opts.set.as_deref().filter(|s| *s != "all");
    let all: Vec<(&Report, &Binding)> = project.scheduled_bindings(name);
    let picked: Vec<(&Report, Binding)> = all
        .iter()
        .filter(|(r, _)| reports.as_ref().is_none_or(|rs| rs.contains(&r.name.as_str())))
        .filter(|(_, b)| set.is_none() || b.set.as_deref() == set)
        .map(|(r, b)| {
            let mut b = (*b).clone();
            if let Some(p) = &opts.profile {
                b.profile = Some(p.clone());
                b.profile_at = Some(profile_flag_at());
            }
            (*r, b)
        })
        .collect();
    if picked.is_empty() && (reports.is_some() || set.is_some()) {
        let mut asked = Vec::new();
        if let Some(s) = &opts.selector {
            asked.push(format!("`{s}`"));
        }
        if let Some(s) = set {
            asked.push(format!("Set `{s}`"));
        }
        let runs: Vec<String> = all
            .iter()
            .map(|(r, b)| match &b.set {
                Some(s) => format!("{}/{s}", r.name),
                None => r.name.clone(),
            })
            .collect();
        return Err(format!(
            "schedule `{name}` doesn't run {}; it runs: {}",
            asked.join(" with "),
            if runs.is_empty() {
                "nothing".to_string()
            } else {
                runs.join(", ")
            }
        ));
    }
    Ok(picked)
}

/// The usage error for `--schedule` with a name that isn't declared.
pub fn unknown_schedule(project: &Project, name: &str) -> String {
    let names: Vec<&str> = project.schedules().iter().map(|e| e.name.as_str()).collect();
    if names.is_empty() {
        format!("no schedule `{name}`: this project's schedules.yml declares none")
    } else {
        format!("no schedule `{name}`; valid names: {}", names.join(", "))
    }
}

/// Which Bindings of a report run, following ADR 0009's Set rules.
pub fn choose_bindings(
    project: &Project,
    report: &Report,
    opts: &RunOptions,
    ui: &mut dyn Ui,
) -> Result<Vec<Binding>, String> {
    let mut out: Vec<Binding> = match opts.set.as_deref() {
        Some("all") => report.bindings.clone(),
        Some(name) => match report.binding(name) {
            Some(b) => vec![b.clone()],
            None => vec![ad_hoc(project, report, name)],
        },
        None if !report.has_sets => report.bindings.clone(),
        None => {
            let names: Vec<String> = report.bindings.iter().filter_map(|b| b.set.clone()).collect();
            let pick = report
                .default_set
                .clone()
                .or_else(|| {
                    project
                        .default_set()
                        .map(str::to_string)
                        .filter(|d| names.contains(d))
                })
                .or_else(|| match names.as_slice() {
                    [only] => Some(only.clone()),
                    _ => None,
                });
            match pick {
                Some(p) => report.binding(&p).cloned().into_iter().collect(),
                None if opts.interactive => match ui.choose_set(&report.name, &names)? {
                    Some(p) => report.binding(&p).cloned().into_iter().collect(),
                    None => report.bindings.clone(),
                },
                None => {
                    return Err(format!(
                        "report `{}` has several Sets ({}) and no `default_set`; declare `default_set:` or pass `--set <name>` or `--set all`",
                        report.name,
                        names.join(", ")
                    ));
                }
            }
        }
    };
    if let Some(p) = &opts.profile {
        for b in &mut out {
            b.profile = Some(p.clone());
            b.profile_at = Some(profile_flag_at());
        }
    }
    Ok(out)
}

/// A Set that isn't declared on the report: start from the report itself, then apply the
/// `sets.yml` entry of that name if there is one. `--profile`/`--var` complete it.
fn ad_hoc(project: &Project, report: &Report, name: &str) -> Binding {
    let mut b = report.base.clone();
    b.set = Some(name.to_string());
    if let Some(reg) = project.set(name) {
        if let Some(p) = &reg.profile {
            b.profile = Some(p.clone());
            b.profile_at = Some(crate::project::ProfileAt {
                file: reg.file.clone(),
                line: reg.line,
                key: format!("`profile` of Set `{name}`"),
            });
        }
        b.vars.extend(reg.vars.clone());
    }
    b
}

/// `--profile` as the source of a Binding's inherited connection, for messages.
fn profile_flag_at() -> crate::project::ProfileAt {
    crate::project::ProfileAt {
        file: PathBuf::from("--profile"),
        line: None,
        key: "`--profile`".into(),
    }
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
    /// The parse pass for this run's inputs.
    parsed: ParsedBinding,
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
type Fail = crate::error::Error;

fn masked_source_error(secrets: &Secrets, error: &HostError, sensitive: bool) -> String {
    let error = error.to_string();
    secrets.mask_source_error(&error, sensitive).into_owned()
}

fn masked_sql_summary(secrets: &Secrets, sql: &str, max: usize, sensitive: bool) -> String {
    if sensitive {
        crate::secrets::MASK.to_string()
    } else {
        dre_protocol::util::summarize(&secrets.mask(sql), max)
    }
}

fn protected_sql<'a>(secrets: &Secrets, sql: &'a str, sensitive: bool) -> std::borrow::Cow<'a, str> {
    if sensitive {
        std::borrow::Cow::Borrowed(crate::secrets::MASK)
    } else {
        secrets.mask(sql)
    }
}

impl<'a> BindingRun<'a> {
    fn new(
        ctx: &Context,
        project: &'a Project,
        report: &'a Report,
        b: &'a Binding,
        opts: &'a RunOptions,
        ui: Events,
        store: &'a RunStore,
    ) -> Self {
        let schedule = opts.schedule.as_ref().and_then(|n| project.schedule(n));
        let schedule_vars = schedule.map(|e| e.vars.clone());
        // Names were checked when the project and the command line were read.
        let calendar: Calendar = crate::parse::calendar(
            project,
            report,
            opts.timezone.as_deref(),
            opts.schedule.as_deref(),
        );
        let date = crate::parse::run_date(&calendar, opts.date, opts.scheduled_at);
        // Binding vars, then the schedule's, then `--var` on top.
        let mut vars = b.vars.clone();
        vars.extend(schedule_vars.clone().unwrap_or_default());
        let rendered_vars = {
            let mut v = vars.clone();
            v.extend(
                opts.vars
                    .iter()
                    .map(|(k, x)| (k.clone(), Json::String(x.clone()))),
            );
            v
        };
        ui.binding_vars(opts.schedule.as_deref(), schedule_vars.as_ref(), &rendered_vars);
        let started_at = chrono::Utc::now();
        let runs = crate::runs::BindingRuns::new(&store.run_dir(&report.name, b.dir_name()));
        // A dry run (compile, validate) makes no run folder: its plan shows where files would go.
        let run_id = if opts.dry_run || opts.live_check {
            "<run id>".to_string()
        } else {
            crate::runs::new_run_id(started_at)
        };
        BindingRun {
            ctx: ctx.clone(),
            project,
            report,
            b,
            vars,
            rendered_vars,
            schedule_vars,
            opts,
            date,
            calendar,
            ui,
            store,
            compiled_dir: store.compiled_dir(&report.name, b.dir_name()),
            run_dir: runs.run_dir(&run_id),
            runs,
            run_id,
            becomes_current: true,
            schema_dir: store.schema_dir(&report.name, b.dir_name()),
            target: project.target_name().to_string(),
            parsed: ParsedBinding::default(),
            pool: None,
            renderers: BTreeMap::new(),
            started: Instant::now(),
            started_at,
            produced: Vec::new(),
            outs: b.outputs.iter().map(|_| OutputRun::default()).collect(),
            deliverer: None,
            drift: Vec::new(),
            protected_snapshot: None,
            retry: None,
            held: Vec::new(),
            zips: Default::default(),
        }
    }

    /// What `connection`, `destination` and `profile()` read for this Binding.
    fn connections(&self) -> Arc<ProfileConnections> {
        Arc::new(ProfileConnections {
            ctx: self.ctx.clone(),
            profiles: self.project.profiles().clone(),
            root: self.project.root().to_path_buf(),
            plugins: self.project.plugins().to_vec(),
            log: self.ui.plugin_log(),
        })
    }

    /// The renderer for SQL on `connection` (`None`: the inherited connection, for output paths
    /// and template values), made on first use.
    fn renderer(&mut self, connection: Option<&str>) -> Result<Arc<Renderer>, Fail> {
        let key = connection.map(str::to_string);
        if let Some(r) = self.renderers.get(&key) {
            return Ok(r.clone());
        }
        let pool = self.pool.clone().expect("the pool is made before rendering");
        // Outside query SQL, `connection.*` and `run_query()` mean the inherited connection.
        let name = key.clone().or_else(|| self.parsed.inherited.clone());
        let source_type = match (&key, &name) {
            (Some(n), _) => pool.kind(n)?,
            // The inherited connection's type matters only if something uses it.
            (None, Some(n)) => pool.kind(n).unwrap_or_default(),
            (None, None) => String::new(),
        };
        let context =
            crate::parse::run_context(self.project, self.report, self.b, &self.inputs(), self.started_at);
        let limited = Arc::new(crate::render::Limited::new(
            context.clone(),
            self.vars.clone(),
            self.opts.vars.clone(),
            &self.ctx,
        ));
        let r = Renderer::new(RendererConfig {
            root: self.project.root(),
            project_name: self.project.name(),
            templates: self.project.templates(),
            context,
            vars: self.vars.clone(),
            cli_vars: self.opts.vars.clone(),
            runner: Some(Arc::new(PoolRunner {
                pool: pool.clone(),
                secrets: self.ctx.secrets().clone(),
                default: name.clone(),
                log: self.ui.sql_log(),
            })),
            connections: Some(pool.connections.clone()),
            mode: Mode::Run,
            connection: name,
            source_type,
            sources: crate::parse::resolver(self.project, limited),
            process: self.ctx.clone(),
        })
        .map_err(|e| e.to_string())?;
        let r = Arc::new(r);
        self.renderers.insert(key, r.clone());
        Ok(r)
    }

    /// The parse pass's inputs for this run.
    fn inputs(&self) -> crate::parse::Inputs {
        crate::parse::Inputs {
            target: self.target.clone(),
            cli_vars: self.opts.vars.clone(),
            date: self.opts.date,
            scheduled_at: self.opts.scheduled_at,
            timezone: self.opts.timezone.clone(),
            schedule: self.opts.schedule.clone(),
            started_at: Some(self.started_at),
            process: self.ctx.clone(),
        }
    }

    /// Each query's connection, from the parse pass.
    fn connection_of(&self, query: &str) -> Result<String, Fail> {
        self.parsed
            .query(query)
            .and_then(|q| q.connection.clone())
            .ok_or_else(|| format!("query `{query}` has no connection").into())
    }

    /// `report` or `report (Set s)`, for messages.
    fn label(&self) -> String {
        match &self.b.set {
            Some(s) => format!("{} (Set {s})", self.report.name),
            None => self.report.name.clone(),
        }
    }

    /// Why the Binding's lock couldn't be taken, with how to clear it.
    fn lock_failure(&self, e: crate::runs::LockError) -> Fail {
        let unlock = match &self.b.set {
            Some(s) => format!("dre unlock {} --binding {s}", self.report.name),
            None => format!("dre unlock {}", self.report.name),
        };
        let message = match e {
            crate::runs::LockError::Held(h) => format!(
                "{} is already running ({}); this run changed nothing. Wait for it, or if it's gone, `{unlock}`",
                self.label(),
                h.describe()
            ),
            crate::runs::LockError::Unreadable(p) => format!(
                "{} has an unreadable lock at {}; if no run is going on, `{unlock}`",
                self.label(),
                p.display()
            ),
            crate::runs::LockError::Io(e) => format!("can't lock {}: {e}", self.label()),
        };
        Fail::new(Code::RunInProgress, message)
    }

    /// Whether the current run is for a later instant than this one (its `DRE_RUN_AT`, else
    /// its start): then this run doesn't replace it.
    fn older_than_current(&self) -> bool {
        let Some(dir) = self.runs.current_dir() else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(dir.join("run_results.json")) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<Json>(&text) else {
            return false;
        };
        let theirs = v
            .get("scheduled_at")
            .and_then(Json::as_str)
            .or_else(|| v.get("started_at").and_then(Json::as_str))
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        let mine = self.opts.scheduled_at.unwrap_or(self.started_at);
        theirs.is_some_and(|t| mine < t)
    }

    /// Fail once the run is cancelled, so no further statement, file or delivery starts.
    /// `--preview`'s row limit per query; none for a Binding with a per-row output, whose
    /// population the preview counts in full.
    fn row_limit(&self) -> Option<u64> {
        if self.b.outputs.iter().any(|o| o.for_each.is_some()) {
            None
        } else {
            self.opts.preview
        }
    }

    fn not_cancelled(&self) -> Result<(), Fail> {
        match self.opts.cancel.reason() {
            None => Ok(()),
            Some(crate::engine::CancelReason::Timeout) => {
                Err(Fail::new(Code::RunTimedOut, "the run timed out"))
            }
            Some(_) => Err(Fail::new(Code::RunCancelled, "the run was cancelled")),
        }
    }

    fn outcome(&self, status: Status, error: Option<Fail>) -> BindingOutcome {
        BindingOutcome {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            binding: self.b.dir_name().to_string(),
            status,
            error_code: error.as_ref().map(|e| e.code.clone()),
            error: error.map(|e| e.message),
            files: self.files().map(|(p, _)| p.clone()).collect(),
            summary: self.summary_line(),
            schedule: self.opts.schedule.clone(),
            schedule_vars: self.schedule_vars.clone(),
            vars: self.rendered_vars.clone(),
            timezone: self.calendar.tz.name().to_string(),
            elapsed: self.started.elapsed(),
        }
    }

    fn run(&mut self) -> BindingOutcome {
        let dry = self.opts.dry_run || self.opts.live_check;
        // A real run holds the Binding's lock: the same Binding never runs twice at once.
        let guard = if dry {
            None
        } else {
            match self.runs.lock(
                &self.run_id,
                self.started_at,
                &crate::runs::host_name(self.ctx.env()),
            ) {
                Ok(g) => Some(g),
                Err(e) => {
                    let error = self.lock_failure(e);
                    return self.outcome(Status::Error, Some(error));
                }
            }
        };
        if let Some(g) = &guard {
            if let Some(h) = &g.took_over {
                self.ui.warn(&format!(
                    "took over the lock of {} for {}: that process is gone (it crashed or was killed)",
                    h.describe(),
                    self.label()
                ));
            }
            match self.runs.migrate_legacy(self.started_at) {
                Ok(Some(id)) => self.ui.step(
                    Level::Debug,
                    "Moved",
                    &format!("the files of the run before run folders into runs/{id}/"),
                    None,
                ),
                Ok(None) => {}
                Err(e) => self
                    .ui
                    .warn(&format!("can't move the earlier run's files into runs/: {e}")),
            }
            // A retry redoes some rows of an earlier run: it never replaces the current one.
            self.becomes_current = !self.older_than_current() && self.opts.retry_failed.is_none();
            // The run's own log, in its folder.
            if std::fs::create_dir_all(&self.run_dir).is_ok() {
                self.ui.emit(RunEvent::BindingLog(self.run_dir.join("dre.log")));
            }
        }
        let result = self.run_inner();
        // A Binding that fails after the run was cancelled was stopped by it (a plugin's
        // cancelled reply, or a check between steps).
        let stopped =
            matches!(&result, Err(e) if e.code == Code::RunCancelled || e.code == Code::RunTimedOut);
        let result = match (result, self.opts.cancel.reason()) {
            (Err(e), Some(reason)) if !stopped => {
                let (code, what) = match reason {
                    crate::engine::CancelReason::Timeout => (Code::RunTimedOut, "timed out"),
                    _ => (Code::RunCancelled, "was cancelled"),
                };
                Err(Fail::new(code, format!("the run {what}: {}", e.message)))
            }
            (r, _) => r,
        };
        let status = match (&result, dry, self.opts.live_check) {
            (Err(e), _, _) if e.code == Code::RunCancelled => Status::Cancelled,
            (Err(e), _, _) if e.code == Code::RunTimedOut => Status::TimedOut,
            (Err(_), _, _) => Status::Error,
            (Ok(()), _, true) => Status::Checked,
            (Ok(()), true, _) => Status::DryRun,
            (Ok(()), false, _) => Status::Success,
        };
        let err = result.err();
        if !dry && let Err(e) = self.write_results(&status, err.as_ref()) {
            self.ui.warn(&format!("can't write run_results.json: {e}"));
        }
        if guard.is_some() {
            if self.becomes_current {
                if let Err(e) = self.runs.switch(&self.run_id) {
                    self.ui.warn(&format!("can't make this run the current one: {e}"));
                }
            } else {
                self.ui.warn(&format!(
                    "{}: a run for a later instant is current, so this one doesn't replace it; its files are in {}",
                    self.label(),
                    rel(self.project.root(), &self.run_dir).display()
                ));
            }
            for id in self
                .runs
                .prune(self.opts.keep_runs.unwrap_or(self.project.keep_runs()))
            {
                self.ui.step(Level::Debug, "Removed", &format!("run {id}"), None);
            }
        }
        // Spools are scratch space.
        let _ = std::fs::remove_dir_all(self.run_dir.join(".spool"));
        self.outcome(status, err)
    }

    fn run_inner(&mut self) -> Result<(), Fail> {
        let errors = crate::variables::check_run_values(&self.report.var_schema, &self.vars, &self.opts.vars);
        if !errors.is_empty() {
            return Err(errors.join("; ").into());
        }
        // The parse pass with this run's inputs: each query's connection and sources.
        self.parsed = crate::parse::binding(self.project, self.report, self.b, &self.vars, &self.inputs());
        if !self.parsed.errors.is_empty() {
            let msgs: Vec<String> = self
                .parsed
                .errors
                .iter()
                .map(|p| match p.line {
                    Some(l) => format!("{}:{l}: {}", p.file.display(), p.message),
                    None => format!("{}: {}", p.file.display(), p.message),
                })
                .collect();
            return Err(Fail::new(Code::ParseFailed, msgs.join("\n    ")));
        }
        // Sessions are opened only when something needs the database, so compiling a report
        // whose templates don't query it works without the plugin or credentials.
        let pool = Arc::new(Pool::new(
            self.connections(),
            self.project.root().to_path_buf(),
            self.ui.plugin_log(),
            !self.report.managed,
        ));
        self.pool = Some(pool.clone());

        if !self.report.managed {
            self.ui.warn(&format!(
                "`{}` is an unmanaged report ({}): for quick tests only — add a YAML to make it a managed report",
                self.report.name,
                self.report.file.display()
            ));
        }

        // 1. Render.
        std::fs::create_dir_all(&self.compiled_dir).map_err(|e| e.to_string())?;
        // How many queries run on each connection: one session holds them all.
        let mut per_connection: BTreeMap<String, usize> = BTreeMap::new();
        for q in &self.b.queries {
            *per_connection.entry(self.connection_of(&q.query)?).or_default() += 1;
        }
        // A real run of a managed report renders each query just before running it, so a
        // template can look at what earlier queries made (`columns()` of a temp table).
        // Compiling, checking and unmanaged reports render everything first.
        let interleave = !self.opts.dry_run && !self.opts.live_check && self.report.managed;
        let mut statements = Vec::new();
        // Reported after the unmanaged-report check, which matters more.
        let mut two_tabs: Option<String> = None;
        let retry_snapshot = match &self.opts.retry_failed {
            Some(id) => self.start_retry(id)?,
            None => None,
        };
        if let Some(dir) = retry_snapshot {
            self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
            self.replay_from(&dir, "the saved results")?;
        } else if let Some(dir) = self.opts.from_results.clone() {
            self.replay(&dir)?;
        } else if interleave {
            self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
            let mut checked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for (c, n) in &per_connection {
                if *n > 1 {
                    self.check_sessions(&pool, c, *n)?;
                    checked.insert(c.clone());
                }
            }
            std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
            let mut i = 0;
            let queries = self.b.queries.clone();
            for q in &queries {
                let conn = self.connection_of(&q.query)?;
                let renderer = self.renderer(Some(&conn))?;
                let sts = self.render_query(&renderer, q, &conn, &mut two_tabs)?;
                for w in renderer.take_warnings() {
                    self.ui.warn(&w);
                }
                if let Some(e) = two_tabs.take() {
                    return Err(e.into());
                }
                if sts.len() > 1 && checked.insert(conn.clone()) {
                    self.check_sessions(&pool, &conn, sts.len())?;
                }
                let session = pool.session(&conn)?;
                for st in &sts {
                    self.execute_statement(&session, i, st)?;
                    i += 1;
                }
            }
        } else {
            let queries = self.b.queries.clone();
            for q in &queries {
                let conn = self.connection_of(&q.query)?;
                let renderer = self.renderer(Some(&conn))?;
                statements.extend(self.render_query(&renderer, q, &conn, &mut two_tabs)?);
            }
            self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
            for r in self.renderers.values() {
                for w in r.take_warnings() {
                    self.ui.warn(&w);
                }
            }

            // 3. Unmanaged: every rendered statement must only read (or create temp objects).
            if !self.report.managed {
                if let Some(bad) = statements.iter().find(|s| !s.kind.is_read_only_safe()) {
                    return Err(format!(
                        "{}:{}: unmanaged report `{}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; nothing was run — rewrite the statement, or give the report a YAML to declare it",
                        bad.file.display(),
                        bad.line,
                        self.report.name,
                        masked_sql_summary(self.ctx.secrets(), &bad.text, 60, bad.sensitive)
                    ).into());
                }
                for conn in per_connection.keys() {
                    let session = pool.session(conn)?;
                    let creates_temp = statements
                        .iter()
                        .any(|s| &s.connection == conn && s.kind == StatementKind::TempCreate)
                        || session.lock().unwrap().loaded;
                    session.lock().unwrap().want_read_only(!creates_temp);
                }
            }
            if let Some(e) = two_tabs {
                return Err(e.into());
            }

            if self.opts.dry_run {
                let plan = self.plan();
                self.ui.compiled(&plan);
                return Ok(());
            }

            // The one-session-per-connection guarantee.
            for conn in per_connection.keys() {
                let n = statements.iter().filter(|s| &s.connection == conn).count();
                if n > 1 {
                    self.check_sessions(&pool, conn, n)?;
                }
            }

            if self.opts.live_check {
                return self.live_check(&statements);
            }

            // 4. Execute in YAML order, each on its connection's session.
            std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
            for (i, st) in statements.iter().enumerate() {
                let session = pool.session(&st.connection)?;
                self.execute_statement(&session, i, st)?;
            }
        }
        pool.close_all();

        self.name_result_sets()?;
        // A password option (`password:`, `zip: {password}`) read through `env_var()` is a secret.
        for o in &self.b.outputs {
            self.ctx.secrets().register_secret_fields(&o.options, &[]);
            let mut passwords: Vec<&str> = o
                .options
                .get("password")
                .and_then(Json::as_str)
                .into_iter()
                .collect();
            for d in &o.destinations {
                self.ctx.secrets().register_secret_fields(&d.options, &[]);
                passwords.extend(d.zip.as_ref().and_then(|z| z.password.as_deref()));
            }
            for pw in passwords {
                self.ctx
                    .secrets()
                    .register_secret_field(&Json::String(pw.to_string()));
                // A password written in the YAML is a secret too (validate warns about it).
                if !crate::preflight::is_templated(pw) {
                    self.ctx.secrets().register_values([pw.to_string()]);
                }
            }
        }
        if let Some(dir) = &self.opts.output_json {
            let dir = self
                .project
                .root()
                .join(dir)
                .join(&self.report.name)
                .join(self.b.dir_name());
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut index = crate::saved::Index {
                schema_version: crate::saved::SCHEMA_VERSION.into(),
                run_id: self.run_id.clone(),
                results: Vec::new(),
            };
            for result in &self.produced {
                // Secret columns are saved masked: dre-mcp and people read these files.
                let secret = self.secret_columns(&result.query);
                let masked = result.spool.with_extension("masked.arrow");
                let spool = if secret.is_empty() {
                    &result.spool
                } else {
                    crate::secret_columns::masked_copy(&result.spool, &masked, &secret)?;
                    &masked
                };
                crate::rows::write_spool(spool, &dir.join(format!("{}.json", result.name)), result.rows)?;
                // The exact Arrow stream too, for `--from-results`.
                let file = format!("{}.arrows", result.name);
                crate::saved::spool_to_stream(spool, &dir.join(&file))?;
                index.results.push(crate::saved::SavedResult {
                    query: result.query.clone(),
                    name: result.name.clone(),
                    connection: result.connection.clone(),
                    rows: result.rows,
                    sql_sha256: std::fs::read_to_string(
                        self.compiled_dir.join(format!("{}.sha256", result.query)),
                    )
                    .ok()
                    .map(|h| h.trim().to_string()),
                    file,
                    masked_columns: secret,
                });
            }
            crate::saved::write_index(&dir, &index).map_err(|e| e.to_string())?;
        }

        // 5. Format every file output into target/run/; one whose `when:` is false is skipped.
        let (file_outs, message_outs): (Vec<usize>, Vec<usize>) =
            (0..self.outs.len()).partition(|&i| !self.b.outputs[i].is_message());
        // A per-row output keeps the run's results, so `--retry-failed` redoes rows from them.
        if self.retry.is_none()
            && self.opts.from_results.is_none()
            && self.opts.preview.is_none()
            && self
                .b
                .outputs
                .iter()
                .any(|o| o.for_each.as_ref().is_some_and(|f| f.keep_results))
        {
            self.save_snapshot()?;
        }
        // `on_empty` first, for every output: a failure stops the run before anything is written.
        for oi in 0..self.outs.len() {
            self.has_data(oi)?;
        }
        if self.retry.is_some() {
            for oi in 0..self.outs.len() {
                if self.b.outputs[oi].for_each.is_none() {
                    self.skip(oi, "--retry-failed redoes per-row outputs only");
                    self.outs[oi].reason = Some("retry".into());
                }
            }
        }
        for &oi in &file_outs {
            if self.outs[oi].status == Some(OutputStatus::Skipped) || !self.passes_when(oi)? {
                continue;
            }
            // A per-row output is made and delivered row by row, after the drift check.
            if self.b.outputs[oi].for_each.is_some() {
                continue;
            }
            let filename = self.file_names(oi);
            self.format(oi, &filename).map_err(|e| e.or(Code::FormatFailed))?;
        }

        // Schema drift, before delivery.
        if self.opts.preview.is_none() {
            self.drift = self.schema_drift();
            if !self.drift.is_empty() {
                if self.opts.accept_schema_change {
                    self.ui.warn(&format!(
                        "  schema changed since the last successful run (accepted): {}",
                        self.drift.join("; ")
                    ));
                } else {
                    return Err(format!(
                        "schema drift since the last successful run: {}; the output is in {} but was not delivered — pass --accept-schema-change to deliver it and accept the new schema",
                        self.drift.join("; "),
                        rel(self.project.root(), &self.run_dir).display()
                    ).into());
                }
            }
        }

        // 6. Deliver the file outputs, then render and deliver each message, so a message can
        // link to what was delivered. A failed output doesn't stop the next one.
        let preview = self.opts.preview.is_some() || self.opts.no_deliver;
        let mut failures = Vec::new();
        for oi in file_outs.into_iter().chain(message_outs) {
            if self.outs[oi].status == Some(OutputStatus::Skipped) {
                continue;
            }
            if self.b.outputs[oi].for_each.is_some() {
                if let Err(e) = self.run_rows(oi) {
                    if e.code == Code::RunCancelled || e.code == Code::RunTimedOut {
                        return Err(e);
                    }
                    self.outs[oi].status = Some(OutputStatus::Failed);
                    self.outs[oi].error.get_or_insert_with(|| e.message.clone());
                    failures.push(e);
                }
                continue;
            }
            if self.b.outputs[oi].is_message() {
                match self.render_message(oi) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        let e = e.or(Code::RenderFailed);
                        let e = Fail::new(e.code.clone(), format!("{}: {e}", self.b.outputs[oi].label(oi)));
                        self.outs[oi].status = Some(OutputStatus::Failed);
                        self.outs[oi].error = Some(e.message.clone());
                        failures.push(e);
                        continue;
                    }
                }
            }
            if preview {
                self.outs[oi].delivery_note = Some(if self.opts.no_deliver {
                    "--no-deliver: output kept locally".into()
                } else {
                    "preview: not delivered".into()
                });
                self.outs[oi].status = Some(OutputStatus::Kept);
            } else if let Err(e) = self.deliver(oi) {
                failures.push(e.or(Code::DeliveryFailed));
            }
        }
        if preview {
            let dir = rel(self.project.root(), &self.run_dir);
            self.ui.step(
                Level::Info,
                "Preview",
                &format!("not delivered; output stays in {}", dir.display()),
                None,
            );
        }
        if let Some(first) = failures.first() {
            let messages: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
            return Err(Fail::new(first.code.clone(), messages.join("; ")));
        }

        // 7. Snapshot the schema for the next drift check (not from a run for an earlier instant
        // than the current one).
        if self.opts.preview.is_none()
            && self.becomes_current
            && let Err(error) = self.write_snapshot()
        {
            self.ui.warn(&format!(
                "delivery completed, but the schema snapshot could not be written: {error}; the next run may compare against an older schema"
            ));
        }
        Ok(())
    }

    /// Output `oi`'s queries' results, `rows` capped at `max_rows`.
    fn results_for(&mut self, oi: usize, max_rows: u64) -> Result<Vec<(String, Arc<QueryResult>)>, Fail> {
        let o = &self.b.outputs[oi];
        let mut out = Vec::new();
        for p in self.produced.iter().filter(|p| o.feeds(&p.query)) {
            let r = QueryResult::read(&p.spool, p.rows, max_rows, self.calendar)
                .map_err(|e| format!("can't read the result of `{}`: {e}", p.query))?;
            out.push((p.query.clone(), Arc::new(r)));
        }
        Ok(out)
    }

    /// What output `oi`'s templates read besides the usual context: `results` and `outputs`
    /// (every other named output: `location`, `files` and `status`).
    fn template_context(&self, oi: usize, results: &[(String, Arc<QueryResult>)]) -> minijinja::Value {
        use minijinja::Value;
        let results: BTreeMap<String, Value> = results
            .iter()
            .map(|(q, r)| (q.clone(), r.clone().value()))
            .collect();
        let mut outputs: BTreeMap<String, Value> = BTreeMap::new();
        for (j, (o, run)) in self.b.outputs.iter().zip(&self.outs).enumerate() {
            let Some(name) = &o.name else { continue };
            if j == oi {
                continue;
            }
            let locations: Vec<&str> = run
                .deliveries
                .iter()
                .filter_map(|d| d.location.as_deref())
                .collect();
            let files: Vec<String> = run
                .files
                .iter()
                .map(|(f, _)| record_path(self.project, f).to_string_lossy().to_string())
                .collect();
            let status = run.status.map_or("pending", OutputStatus::as_str);
            outputs.insert(
                name.clone(),
                Value::from(minijinja::value::Serde(json!({
                    "location": locations.join(", "),
                    "files": files,
                    "status": status,
                }))),
            );
        }
        Value::from(BTreeMap::from([
            ("results".to_string(), Value::from(results)),
            ("outputs".to_string(), Value::from(outputs)),
        ]))
    }

    /// Apply output `oi`'s `on_empty` (and its queries'). `Ok(false)`: skipped for no data;
    /// an error fails the run (`no-data`).
    fn has_data(&mut self, oi: usize) -> Result<bool, Fail> {
        let o = &self.b.outputs[oi];
        let label = o.label(oi);
        // A per-row output's population is its driver: each row's slices are checked per row.
        let queries: Vec<crate::no_data::Query<'_>> = self
            .produced
            .iter()
            .filter(|p| match &o.for_each {
                Some(f) => p.query == f.query,
                None => o.feeds(&p.query),
            })
            .map(|p| crate::no_data::Query {
                name: &p.query,
                rows: p.rows,
                on_empty: self
                    .b
                    .queries
                    .iter()
                    .find(|q| q.query == p.query)
                    .and_then(|q| q.on_empty),
            })
            .collect();
        let droppable = !(Format::new(&o.format).template_decides_sections() || o.for_each.is_some());
        let d = crate::no_data::decide(o.on_empty, &queries, droppable);
        self.outs[oi].no_data = d.empty.clone();
        for w in &d.warnings {
            self.ui.warn(&format!("  {label}: {w}"));
        }
        if let Some(info) = &d.info {
            self.ui
                .step(Level::Info, "No data", &format!("{label}: {info}"), None);
        }
        if let Some(msg) = d.fail {
            self.outs[oi].reason = Some("no_data".into());
            return Err(Fail::new(Code::NoData, format!("{label}: {msg}")));
        }
        if let Some(why) = d.skip {
            self.outs[oi].reason = Some("no_data".into());
            self.skip(oi, &why);
            return Ok(false);
        }
        self.outs[oi].dropped = d.drop;
        Ok(true)
    }

    /// Evaluate output `oi`'s `when:`. False: the output is skipped (recorded, not a failure).
    fn passes_when(&mut self, oi: usize) -> Result<bool, Fail> {
        if self.b.outputs[oi].when.is_none() {
            return Ok(true);
        }
        let results = self.results_for(oi, crate::message::DEFAULT_MAX_ROWS)?;
        self.when_holds(oi, &results)
    }

    /// Output `oi`'s `when:` over `results` (the same ones its message reads).
    fn when_holds(&mut self, oi: usize, results: &[(String, Arc<QueryResult>)]) -> Result<bool, Fail> {
        let Some(when) = self.b.outputs[oi].when.clone() else {
            return Ok(true);
        };
        let ctx = self.template_context(oi, results);
        let renderer = self.renderer(None)?;
        let label = self.b.outputs[oi].label(oi);
        let src = format!("{{% if {when} %}}true{{% endif %}}");
        let passed = renderer
            .render_with(&self.report.file, &src, ctx, false)
            .map_err(|e| format!("{label}: `when` `{when}`: {}", e.message))?
            == "true";
        self.outs[oi].when = Some(passed);
        if !passed {
            self.skip(oi, &format!("`when` is false ({when})"));
        }
        Ok(passed)
    }

    fn skip(&mut self, oi: usize, why: &str) {
        let label = self.b.outputs[oi].label(oi);
        self.outs[oi].status = Some(OutputStatus::Skipped);
        self.outs[oi].delivery_note = Some(format!("skipped: {why}"));
        self.ui
            .step(Level::Info, "Skipped", &format!("{label}: {why}"), None);
    }

    /// Render message output `oi` and write its `.md` file. `Ok(false)`: skipped (`when:` false,
    /// or the text rendered empty).
    fn render_message(&mut self, oi: usize) -> Result<bool, Fail> {
        let o = &self.b.outputs[oi];
        let opt = |k: &str| o.options.get(k).and_then(Json::as_str).map(str::to_string);
        let (text_src, file_src, title_src) = (opt("text"), opt("file"), opt("title"));
        let max_rows = o
            .options
            .get("max_rows")
            .and_then(Json::as_u64)
            .unwrap_or(crate::message::DEFAULT_MAX_ROWS);
        let results = self.results_for(oi, max_rows)?;
        if !self.when_holds(oi, &results)? {
            return Ok(false);
        }
        for (q, r) in results.iter().filter(|(_, r)| r.capped()) {
            self.ui.warn(&format!(
                "  results.{q}.rows holds the first {} of {} rows (`max_rows`); aggregate in SQL, or raise `max_rows`",
                thousands(r.rows.len() as u64),
                thousands(r.row_count)
            ));
        }
        let ctx = self.template_context(oi, &results);
        let renderer = self.renderer(None)?;
        let file = self.report.file.clone();
        let text = match (text_src, file_src) {
            (Some(t), _) => renderer
                .render_with(&file, &t, ctx.clone(), true)
                .map_err(|e| e.message)?,
            (None, Some(f)) => {
                let root = &self.project.root();
                let path = crate::project::find_template(root, &f)
                    .ok_or_else(|| format!("message file `{f}` doesn't exist"))?;
                let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let shown = rel(root, &path);
                renderer
                    .render_with(&shown, &src, ctx.clone(), true)
                    .map_err(|e| e.to_string())?
            }
            (None, None) => crate::message::default_text(&results, self.locale()),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            self.skip(oi, "the message is empty");
            return Ok(false);
        }
        let title = match title_src {
            Some(t) => renderer
                .render_with(&file, &t, ctx, false)
                .map_err(|e| e.message)?,
            None => format!("{}: {}", self.report.name, self.date),
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = self.file_names(oi);
        let path = self.run_dir.join(&name);
        if let Some(prev) = (0..oi).find(|&j| self.outs[j].files.iter().any(|(f, _)| f == &path)) {
            return Err(format!(
                "it writes {name}, as {} does; give them different `name:`s",
                self.b.outputs[prev].label(prev)
            )
            .into());
        }
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, crate::message::file_body(&title, &text)).map_err(|e| e.to_string())?;
        self.outs[oi].files.push((path, None));
        // Protect preview and metadata copies; report files and deliveries retain their values.
        let shown_title = self.ctx.secrets().mask_message_title(&title);
        let shown_text = self.ctx.secrets().mask_markdown(&text);
        if self.opts.preview.is_some() {
            let mut notes = Vec::new();
            if let Some(n) = self.opts.preview {
                notes.push(format!(
                    "numbers come from a sample of at most {} rows per query (--preview)",
                    thousands(n)
                ));
            }
            if let Some(w) = &self.b.outputs[oi].when {
                notes.push(format!("`when` passed ({w})"));
            }
            let length = text.chars().count();
            notes.push(format!("{} characters", thousands(length as u64)));
            notes.extend(self.destination_limits(oi, length));
            self.ui.message(&ShownMessage {
                output: self.b.outputs[oi].name.clone(),
                title: shown_title.into_owned(),
                text: shown_text.into_owned(),
                notes,
            });
        } else {
            self.ui.step(
                Level::Info,
                "Message",
                &crate::message::excerpt(&title, &text, 100, self.ctx.secrets()),
                None,
            );
        }
        self.outs[oi].message = Some((title, text));
        Ok(true)
    }

    /// For `--preview`: how each destination of message output `oi` takes it, and its limit.
    fn destination_limits(&self, oi: usize, length: usize) -> Vec<String> {
        let mut out = Vec::new();
        let Some(pool) = &self.pool else { return out };
        for d in &self.outs[oi].dests {
            let Some(t) = self.project.profiles().target(Role::Destination, &d.profile) else {
                out.push(format!("{}: delivers nowhere on this target", d.profile));
                continue;
            };
            let described = pool.connections.described(Role::Destination, &t.kind);
            let takes = described
                .as_ref()
                .is_some_and(|x| x.capabilities.iter().any(|c| c == CAP_MESSAGE));
            let limit = described.and_then(|x| x.message_limit);
            out.push(match (takes, limit) {
                (false, _) => format!("{} ({}): delivers the .md file", d.profile, t.kind),
                (true, Some(l)) if length as u64 > l => format!(
                    "{} ({}): {} of {} characters: over the limit, so it will be cut short",
                    d.profile,
                    t.kind,
                    thousands(length as u64),
                    thousands(l)
                ),
                (true, Some(l)) => format!(
                    "{} ({}): {} of {} characters",
                    d.profile,
                    t.kind,
                    thousands(length as u64),
                    thousands(l)
                ),
                (true, None) => format!("{} ({}): posts the message", d.profile, t.kind),
            });
        }
        out
    }

    /// The Binding's locale for the number filters.
    fn locale(&self) -> crate::numbers::Locale {
        self.b
            .locale
            .as_deref()
            .and_then(|l| crate::numbers::Locale::parse(l).ok())
            .unwrap_or_default()
    }

    /// Render one query file into its statements, writing it to `target/compiled/`.
    fn render_query(
        &mut self,
        renderer: &Renderer,
        q: &QueryEntry,
        connection: &str,
        two_tabs: &mut Option<String>,
    ) -> Result<Vec<Statement>, Fail> {
        let mut out = Vec::new();
        let src = std::fs::read_to_string(self.project.root().join(&q.path))
            .map_err(|e| format!("{}: {e}", q.path.display()))?;
        let _ = renderer.take_sources();
        let sql = renderer
            .render(&q.path, &src)
            .map_err(|e: RenderError| e.to_string())?;
        // The manifest's `depends_on` and the query's connection come from the parse pass, so
        // a `source()` it couldn't see would make both wrong.
        let known = self
            .parsed
            .query(&q.query)
            .map(|p| p.sources.clone())
            .unwrap_or_default();
        if let Some((s, t)) = renderer
            .take_sources()
            .into_iter()
            .find(|(s, t)| !known.contains(&format!("{s}.{t}")))
        {
            return Err(format!(
                "{}: `source('{s}', '{t}')` was reached while rendering `{}`, but the parse pass didn't find it. The parse pass renders without data (`run_query()` returns no rows, `connection.*` nothing), so it can't see a `source()` that depends on them; call it where it's reached either way",
                q.path.display(),
                q.query
            ).into());
        }
        let sensitive = self.ctx.secrets().contains_secret(&sql);
        std::fs::write(
            self.compiled_dir.join(format!("{}.sql", q.query)),
            self.ctx.secrets().mask(&sql).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        // Hash the exact rendered SQL before masking display artifacts. Integrations can
        // identify executed SQL without receiving any secret embedded in it.
        let hash = crate::checksum::sha256_hex(sql.as_bytes());
        std::fs::write(self.compiled_dir.join(format!("{}.sha256", q.query)), hash)
            .map_err(|e| e.to_string())?;
        self.ui
            .step(Level::Debug, "Rendered", &q.path.display().to_string(), None);
        // 2. Split. One file makes at most one tab, from its last statement, so any earlier
        // SELECT would be lost: that's an error, not a guess.
        let parts = sqlsplit::split(&sql);
        if q.tab && parts.is_empty() {
            return Err(format!(
                "{}: `{}` makes a tab but has no statements; add a query, or `tab: false` if it's meant to be empty",
                q.path.display(),
                q.query
            ).into());
        }
        let n = parts.len();
        for (i, st) in parts.into_iter().enumerate() {
            let body = sqlsplit::strip_leading_comments(&st.text);
            let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
            let kind = sqlsplit::classify(&st.text);
            let last = i + 1 == n;
            if q.tab && !last && kind == StatementKind::Read && two_tabs.is_none() {
                *two_tabs = Some(format!(
                    "{}:{line}: `{}` has more than one SELECT, but one .sql file makes one tab (from its last statement); put each tab's query in its own .sql file and list each in `queries:`",
                    q.path.display(),
                    q.query
                ));
            }
            out.push(Statement {
                query: q.query.clone(),
                connection: connection.to_string(),
                file: q.path.clone(),
                line,
                kind,
                text: st.text,
                sensitive,
                tab: q.tab && last,
            });
        }
        Ok(out)
    }

    /// Fail unless `connection`'s plugin can hold one session across `n` statements.
    fn check_sessions(&self, pool: &Pool, connection: &str, n: usize) -> Result<(), Fail> {
        let session = pool.session(connection)?;
        let mut s = session.lock().unwrap();
        if !s
            .get()
            .map_err(|e| Fail::new(Code::ConnectionFailed, e))?
            .has(CAP_SESSIONS)
        {
            let kind = pool.kind(connection).unwrap_or_default();
            return Err(format!(
                "this Binding runs {n} statements on connection `{connection}`, but the `{kind}` source plugin can't hold one session across them; nothing was run"
            ).into());
        }
        Ok(())
    }

    /// The `secret_columns` of `query` in this Binding.
    fn secret_columns(&self, query: &str) -> Vec<String> {
        self.b
            .queries
            .iter()
            .filter(|q| q.query == query)
            .flat_map(|q| q.secret_columns.iter().cloned())
            .collect()
    }

    /// Run statement `i`, spooling its result if it makes a tab.
    fn execute_statement(
        &mut self,
        session: &Arc<Mutex<Session>>,
        i: usize,
        st: &Statement,
    ) -> Result<(), Fail> {
        self.not_cancelled()?;
        let sql_log = self.ui.sql_log();
        sql_log(
            &format!("{}:{}", st.file.display(), st.line),
            &protected_sql(self.ctx.secrets(), &st.text, st.sensitive),
        );
        let spool_path = self.run_dir.join(".spool").join(format!("{i}.arrow"));
        let mut writer: Option<FileWriter<File>> = None;
        let t = Instant::now();
        let exec = {
            let mut s = session.lock().unwrap();
            let (p, _log_scope) = s
                .statement(st.sensitive)
                .map_err(|e| Fail::new(Code::ConnectionFailed, e))?;
            p.execute(&st.text, self.row_limit(), &mut |schema, batch| {
                if writer.is_none() {
                    let f = File::create(&spool_path).map_err(|e| e.to_string())?;
                    writer = Some(FileWriter::try_new(f, schema).map_err(|e| e.to_string())?);
                }
                writer
                    .as_mut()
                    .expect("the writer was created above")
                    .write(&batch)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| {
                let message = format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                );
                Fail::from_plugin(&e, message).or(Code::QueryFailed)
            })?
        };
        let what = match &exec {
            Execution::Result { rows, .. } => {
                format!("{} row{}", thousands(*rows), if *rows == 1 { "" } else { "s" })
            }
            Execution::NoResult {
                rows_affected: Some(n),
            } => format!("no result set ({n} affected)"),
            Execution::NoResult { .. } => "no result set".to_string(),
        };
        let at = format!("{}:{}", st.file.display(), st.line);
        self.ui.step(
            Level::Debug,
            "Executed",
            &format!("{at}  {what}"),
            Some(t.elapsed()),
        );
        // The YAML decides the tabs; the result only has to match it. A tab with no rows
        // still gets its column names.
        let (schema, rows) = match (st.tab, exec) {
            (true, Execution::Result { schema, rows }) => (schema, rows),
            (true, Execution::NoResult { .. }) => {
                return Err(Fail::new(
                    Code::NoResultSet,
                    format!(
                        "{at}: `{}` makes a tab, but its last statement returned no result set; if the query only prepares data (a temp view, a SET), add `tab: false` to it in the YAML",
                        st.query
                    ),
                ));
            }
            (false, _) => {
                drop(writer);
                let _ = std::fs::remove_file(&spool_path);
                return Ok(());
            }
        };
        {
            let mut w = match writer {
                Some(w) => w,
                None => FileWriter::try_new(File::create(&spool_path).map_err(|e| e.to_string())?, &schema)
                    .map_err(|e| e.to_string())?,
            };
            w.finish().map_err(|e| e.to_string())?;
            let secret = self.secret_columns(&st.query);
            for c in secret.iter().filter(|c| schema.field_with_name(c).is_err()) {
                self.ui.warn(&format!(
                    "  `{}` has no column `{c}`, which its `secret_columns` names: nothing of it is masked; check the spelling",
                    st.query
                ));
            }
            crate::secret_columns::register(&spool_path, &secret, self.ctx.secrets())?;
            self.produced.push(Produced {
                query: st.query.clone(),
                connection: st.connection.clone(),
                schema,
                rows,
                spool: spool_path,
                name: String::new(),
                anchor: None,
                header: None,
                columns: Default::default(),
                autofit: None,
                style: None,
            });
        }
        Ok(())
    }

    /// `--from-results`: this Binding's results from `dir` (saved by `--output-json`), as if its
    /// queries had run. No query runs and no connection opens; a query whose SQL changed since
    /// is warned about, when its SQL renders without the database.
    fn replay(&mut self, dir: &Path) -> Result<(), Fail> {
        let shown = dir.display().to_string();
        let dir = self
            .project
            .root()
            .join(dir)
            .join(&self.report.name)
            .join(self.b.dir_name());
        if let Err(e) = crate::saved::read_index(&dir) {
            return Err(format!(
                "--from-results {shown}: no saved results for this report and Set ({e}); save them with `dre run {} --no-deliver --output-json {shown}`",
                self.report.name
            )
            .into());
        }
        self.replay_from(&dir, &format!("--from-results {shown}"))
    }

    /// Replay the saved results in `dir` (an index and its streams); `shown` names them in
    /// messages.
    fn replay_from(&mut self, dir: &Path, shown: &str) -> Result<(), Fail> {
        let dir = dir.to_path_buf();
        let index = crate::saved::read_index(&dir).map_err(|e| format!("{shown}: {e}"))?;
        let queries: Vec<QueryEntry> = self.b.queries.iter().filter(|q| q.tab).cloned().collect();
        let missing: Vec<&str> = queries
            .iter()
            .filter(|q| index.find(&q.query).is_none())
            .map(|q| q.query.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{shown}: no saved result for {} (saved: {}); save them again with `--output-json`",
                missing
                    .iter()
                    .map(|q| format!("`{q}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                index
                    .results
                    .iter()
                    .map(|r| r.query.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into());
        }
        // Warn when a query's SQL changed since the results were saved.
        for q in &queries {
            let saved = index.find(&q.query).and_then(|r| r.sql_sha256.clone());
            let rendered = self
                .connection_of(&q.query)
                .and_then(|conn| {
                    let renderer = self.renderer(Some(&conn))?;
                    let mut two = None;
                    self.render_query(&renderer, q, &conn, &mut two)
                })
                .map(|_| {
                    std::fs::read_to_string(self.compiled_dir.join(format!("{}.sha256", q.query)))
                        .ok()
                        .map(|h| h.trim().to_string())
                });
            if let Some(r) = index.find(&q.query)
                && !r.masked_columns.is_empty()
            {
                self.ui.warn(&format!(
                    "  `{}`'s saved results have its `secret_columns` ({}) as `*****`: what they feed (a password, a message) renders as `*****`, so don't deliver this replay",
                    q.query,
                    r.masked_columns.join(", ")
                ));
            }
            match (rendered, saved) {
                (Ok(Some(now)), Some(then)) if now != then => self.ui.warn(&format!(
                    "  `{}`'s SQL changed since its results were saved; re-run with `--output-json` to refresh them",
                    q.query
                )),
                (Err(e), _) => self.ui.warn(&format!(
                    "  `{}`: can't render its SQL to compare with the saved results ({}); using them as saved",
                    q.query, e.message
                )),
                _ => {}
            }
        }
        std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        for (i, q) in queries.iter().enumerate() {
            let r = index.find(&q.query).expect("checked above");
            let spool = self.run_dir.join(".spool").join(format!("{i}.arrow"));
            let (schema, rows) = crate::saved::stream_to_spool(&dir.join(&r.file), &spool)?;
            crate::secret_columns::register(&spool, &self.secret_columns(&q.query), self.ctx.secrets())?;
            self.produced.push(Produced {
                query: q.query.clone(),
                connection: r.connection.clone(),
                schema,
                rows,
                spool,
                name: String::new(),
                anchor: None,
                header: None,
                columns: Default::default(),
                autofit: None,
                style: None,
            });
        }
        self.ui.step(
            Level::Info,
            "Replayed",
            &format!("{} result(s) from {shown}; no query ran", queries.len()),
            None,
        );
        Ok(())
    }

    /// Each destination's rendered `profile`, `path` and options; `destination.*` is that
    /// destination while its values render.
    fn render_destinations(&mut self) -> Result<(), Fail> {
        if self.opts.no_deliver {
            return Ok(());
        }
        if self.b.destinations().next().is_none() {
            return Ok(());
        }
        let renderer = self.renderer(None)?;
        let connections = self.pool.as_ref().expect("pool").connections.clone();
        for (key, d) in self.b.keyed_destinations() {
            let oi = key.output;
            // A per-row output's destinations are rendered for each row, with its `each`.
            if self.b.outputs[oi].for_each.is_some() {
                continue;
            }
            let profile = self
                .parsed
                .destination(key)
                .map(str::to_string)
                .ok_or_else(|| format!("destination `{}` has no profile", d.profile))?;
            // Nothing is delivered, so there's no path or options to render.
            if matches!(
                self.project.profiles().entry(Role::Destination, &profile),
                Entry::Nowhere
            ) {
                self.outs[oi].dests.push(RenderedDest {
                    profile,
                    path: None,
                    options: JsonMap::new(),
                    attach: Vec::new(),
                    hold: d.hold,
                    rate: self.opts.rate.or(d.rate),
                    zip: None,
                    zip_file: None,
                });
                continue;
            }
            renderer.set_destination(connections.profile(&profile, Some("destination")).ok());
            let rendered = (|| {
                let path = match &d.path {
                    Some(p) => Some(
                        renderer
                            .render(&self.report.file, p)
                            .map_err(|e| format!("output path: {e}"))?,
                    ),
                    None => None,
                };
                let mut options = JsonMap::new();
                for (k, v) in &d.options {
                    let v = render_json(&renderer, &self.report.file, v)
                        .map_err(|e| format!("destination `{profile}` option `{k}`: {e}"))?;
                    options.insert(k.clone(), v);
                }
                let zip = match &d.zip {
                    Some(z) => Some(crate::zip_pack::ZipSpec {
                        password: match &z.password {
                            Some(pw) => Some(
                                renderer
                                    .render(&self.report.file, pw)
                                    .map_err(|e| format!("destination `{profile}` `zip.password`: {e}"))?,
                            ),
                            None => None,
                        },
                        encryption: z.encryption,
                    }),
                    None => None,
                };
                Ok::<_, Fail>(RenderedDest {
                    profile: profile.clone(),
                    path,
                    options,
                    attach: d.attach.clone(),
                    hold: d.hold,
                    rate: self.opts.rate.or(d.rate),
                    zip,
                    zip_file: None,
                })
            })();
            renderer.set_destination(None);
            self.outs[oi].dests.push(rendered?);
        }
        Ok(())
    }

    /// Tab names: `tab_name`, else the query's basename.
    fn name_result_sets(&mut self) -> Result<(), Fail> {
        let entries: BTreeMap<&str, &QueryEntry> =
            self.b.queries.iter().map(|q| (q.query.as_str(), q)).collect();
        for p in &mut self.produced {
            // Every result set comes from one of the Binding's queries.
            let q = entries[p.query.as_str()];
            p.name = q.tab_name.clone().unwrap_or_else(|| q.query.clone());
            p.anchor = q.anchor.clone();
            p.header = q.header;
            p.columns = q.columns.clone();
            p.autofit = q.autofit;
            p.style = q.style.clone();
        }
        for o in self
            .b
            .outputs
            .iter()
            .filter(|o| Format::new(&o.format).is_workbook())
        {
            let sheets = self.produced.iter().filter(|p| o.feeds(&p.query));
            formats::check_sheets(sheets.map(|p| (p.name.as_str(), p.query.as_str())))?;
        }
        Ok(())
    }

    /// What a real run of this Binding would use and produce, for `dre compile` and `validate`.
    fn plan(&mut self) -> BindingPlan {
        let compiled = self
            .b
            .queries
            .iter()
            .map(|q| {
                rel(
                    self.project.root(),
                    &self.compiled_dir.join(format!("{}.sql", q.query)),
                )
            })
            .collect();
        let mut outputs = Vec::new();
        for oi in 0..self.outs.len() {
            let file = self.file_names(oi);
            let o = &self.b.outputs[oi];
            let destinations = self.outs[oi]
                .dests
                .iter()
                .map(|d| {
                    let out = self.project.profiles().target(Role::Destination, &d.profile);
                    PlannedDestination {
                        profile: d.profile.clone(),
                        kind: out.map(|o| o.kind.clone()),
                        target: Some(self.project.profiles().target_of(Role::Destination, &d.profile)),
                        path: d.path.clone(),
                        delivers: out.is_some(),
                    }
                })
                .collect();
            outputs.push(PlannedOutput {
                name: o.name.clone(),
                format: o.format.clone(),
                output: rel(self.project.root(), &self.run_dir.join(file)),
                destinations,
            });
        }
        let first = outputs.first().cloned();
        let pool = self.pool.clone().expect("pool");
        let queries = self
            .parsed
            .queries
            .iter()
            .map(|q| {
                let connection = q.connection.clone().unwrap_or_default();
                PlannedQuery {
                    query: q.query.clone(),
                    kind: pool.kind(&connection).unwrap_or_default(),
                    target: self.project.profiles().target_of(Role::Connection, &connection),
                    connection,
                    sources: q.sources.clone(),
                }
            })
            .collect();
        BindingPlan {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            compiled,
            profile: self.parsed.inherited.clone(),
            queries,
            target: self.target.clone(),
            format: first.as_ref().map(|o| o.format.clone()).unwrap_or_default(),
            output: first.as_ref().map(|o| o.output.clone()).unwrap_or_default(),
            destinations: first.map(|o| o.destinations).unwrap_or_default(),
            outputs,
            schedules: self.b.schedules.clone(),
        }
    }

    /// Apply `--output-path`/`--output-name` to every destination of the first output that has a
    /// path, and return output `oi`'s local file name: `--output-name` (first output only), else
    /// the first destination path's file name, else `<name>.<ext>` (the output's `name:`, else
    /// the report's).
    fn file_names(&mut self, oi: usize) -> String {
        let first = oi == 0;
        let (output_path, output_name) = if first {
            (self.opts.output_path.clone(), self.opts.output_name.clone())
        } else {
            (None, None)
        };
        for d in self.outs[oi].dests.iter_mut().filter(|d| d.path.is_some()) {
            if let Some(p) = &output_path {
                d.path = Some(p.clone());
            }
            if let (Some(n), Some(r)) = (&output_name, d.path.as_mut()) {
                *r = match r.rfind(['/', '\\']) {
                    Some(i) => format!("{}{n}", &r[..=i]),
                    None => n.clone(),
                };
            }
        }
        let o = &self.b.outputs[oi];
        let stem = o.name.as_deref().unwrap_or(&self.report.name);
        // A zipping entry's path names the zip, not the files in it.
        let paths = self.outs[oi]
            .dests
            .iter()
            .filter(|d| d.zip.is_none())
            .filter_map(|d| d.path.as_deref());
        o.files().base_name(stem, output_name.as_deref(), paths)
    }

    /// Write output `oi`'s files from the result sets of its queries.
    fn format(&mut self, oi: usize, filename: &str) -> Result<(), Fail> {
        self.not_cancelled()?;
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        let b = self.b;
        let out = &b.outputs[oi];
        let mine: Vec<usize> = (0..self.produced.len())
            .filter(|&i| {
                out.feeds(&self.produced[i].query) && !self.outs[oi].dropped.contains(&self.produced[i].query)
            })
            .collect();
        if mine.is_empty() {
            let what = if self.outs.len() > 1 {
                format!("{} gets no tab", out.label(oi))
            } else {
                "no query makes a tab (every query has `tab: false`)".to_string()
            };
            self.ui.warn(&format!("  {what}; no output file was written"));
            return Ok(());
        }
        let tabs: Vec<&str> = mine.iter().map(|&i| self.produced[i].name.as_str()).collect();
        let groups: Vec<(String, Vec<usize>)> = out
            .files()
            .plan(filename, &tabs)?
            .into_iter()
            .map(|(name, of)| (name, of.into_iter().map(|t| mine[t]).collect()))
            .collect();
        // Two outputs writing one file would overwrite each other.
        for (name, _) in &groups {
            let path = self.run_dir.join(name);
            if let Some(prev) = (0..oi).find(|&j| self.outs[j].files.iter().any(|(f, _)| f == &path)) {
                return Err(format!(
                    "{} and {} both write {name}; give them different `name:`s or destination paths",
                    self.b.outputs[prev].label(prev),
                    out.label(oi)
                )
                .into());
            }
        }
        let plugin = find_plugin(&self.ctx, self.project, PluginKind::Format, &out.format)
            .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?;
        let mut p = self
            .ctx
            .host()
            .start(&plugin, self.ui.plugin_log(), Some(self.project.root()))
            .map_err(|e| e.to_string())?;
        let template = match &out.template {
            _ if Format::new(&out.format).is_document() => {
                Some(self.document_payload(out.template.as_ref())?)
            }
            Some(t) => Some(self.template_payload(t)?),
            None => None,
        };
        // Format options may hold Jinja (a pdf `password:`): rendered once for the output.
        let options = if out.options.values().any(per_row::has_template) {
            let renderer = self.renderer(None)?;
            let mut m = JsonMap::new();
            for (k, v) in &out.options {
                let v = if per_row::has_template(v) {
                    render_json(&renderer, &self.report.file, v).map_err(|e| {
                        Fail::new(Code::RenderFailed, format!("{} option `{k}`: {e}", out.label(oi)))
                    })?
                } else {
                    v.clone()
                };
                m.insert(k.clone(), v);
            }
            m
        } else {
            out.options.clone()
        };
        for (name, idxs) in groups {
            let t = Instant::now();
            let path = self.run_dir.join(&name);
            let metas: Vec<ResultSetMeta> = idxs
                .iter()
                .map(|&i| {
                    let r = &self.produced[i];
                    ResultSetMeta {
                        name: r.name.clone(),
                        query: r.query.clone(),
                        result_index: 1,
                        anchor: r.anchor.clone(),
                        header: r.header,
                        columns: r.columns.clone(),
                        autofit: r.autofit,
                        style: r.style.clone(),
                    }
                })
                .collect();
            p.write(WriteRequest {
                path: path.to_string_lossy().into_owned(),
                format: out.format.clone(),
                options: options.clone(),
                result_sets: metas,
                template: template.clone(),
            })
            .map_err(|e| e.to_string())?;
            for &i in &idxs {
                let r = &self.produced[i];
                let reader = FileReader::try_new(File::open(&r.spool).map_err(|e| e.to_string())?, None)
                    .map_err(|e| e.to_string())?;
                // Streamed batch by batch so memory stays flat; a spool that can't be read
                // ends the stream, and fails the output.
                let mut unreadable = None;
                let batches = reader.map_while(|b| b.map_err(|e| unreadable = Some(e.to_string())).ok());
                crate::plugin_host::write_result_set(p.as_mut(), &r.schema, batches)
                    .map_err(|e| format!("{} format: {e}", out.format))?;
                if let Some(e) = unreadable {
                    return Err(e.into());
                }
            }
            let crate::plugin_host::Written { files, warnings } = p
                .finish()
                .map_err(|e| Fail::from_plugin(&e, format!("{} format: {e}", out.format)))?;
            for w in warnings {
                self.ui.warn(&format!("  {} format: {w}", out.format));
            }
            for f in files {
                let f = PathBuf::from(f);
                let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                let shown = rel(self.project.root(), &f);
                self.ui.step(
                    Level::Debug,
                    "Wrote",
                    &format!("{} ({})", shown.display(), human_bytes(size)),
                    Some(t.elapsed()),
                );
                self.outs[oi].files.push((f, None));
            }
        }
        p.close();
        Ok(())
    }

    fn template_payload(&mut self, t: &crate::project::Template) -> Result<Json, Fail> {
        let file = crate::project::resolve_template(self.project.root(), &self.report.file, &t.file)?;
        let renderer = self.renderer(None)?;
        let mut values = JsonMap::new();
        for b in &t.bindings {
            if let (Some(cell), Some(v)) = (&b.cell, &b.value) {
                let rendered = renderer
                    .render(&self.report.file, v)
                    .map_err(|e| format!("template value: {e}"))?;
                values.insert(format!("{}!{}", b.sheet, cell), Json::String(rendered));
            }
        }
        Ok(json!({"file": file.to_string_lossy(), "bindings": t.bindings, "values": values}))
    }

    /// An html or pdf output's `template`: the template file (if any) and the context it's rendered with
    /// ([`crate::document::Context`]).
    fn document_payload(&mut self, t: Option<&crate::project::Template>) -> Result<Json, Fail> {
        let file = match t {
            Some(t) => Some(crate::project::resolve_template(
                self.project.root(),
                &self.report.file,
                &t.file,
            )?),
            None => None,
        };
        let run =
            crate::parse::run_context(self.project, self.report, self.b, &self.inputs(), self.started_at);
        let ctx = crate::document::Context::new(
            file.as_deref(),
            self.project.root(),
            &run,
            self.vars.clone(),
            self.opts.vars.clone(),
        );
        Ok(serde_json::to_value(ctx).map_err(|e| e.to_string())?)
    }

    /// Deliver output `oi` to each of its destinations in order. A failure is recorded and the
    /// rest are still attempted; the output fails if any did.
    fn deliver(&mut self, oi: usize) -> Result<(), Fail> {
        let result = self.deliver_output(oi);
        let o = &mut self.outs[oi];
        o.status = Some(match &result {
            Err(_) => OutputStatus::Failed,
            Ok(()) if o.deliveries.iter().any(|d| d.status == "delivered") => OutputStatus::Delivered,
            Ok(()) if o.deliveries.iter().any(|d| d.status == "held") => OutputStatus::Held,
            Ok(()) => OutputStatus::Kept,
        });
        o.error = result.as_ref().err().map(|f| f.message.clone());
        result
    }

    fn deliver_output(&mut self, oi: usize) -> Result<(), Fail> {
        if self.outs[oi].dests.is_empty() {
            self.outs[oi].delivery_note = Some("no destination declared; output stays in target/".into());
            let dir = rel(self.project.root(), &self.run_dir);
            self.ui.step(
                Level::Debug,
                "Kept",
                &format!("no destination declared: output stays in {}", dir.display()),
                None,
            );
            return Ok(());
        }
        if self.outs[oi].files.is_empty() {
            return Ok(());
        }
        let mut dests = std::mem::take(&mut self.outs[oi].dests);
        for d in dests.iter_mut().filter(|d| d.zip.is_some()) {
            match self.build_zip(oi, d) {
                Ok(z) => d.zip_file = Some(z),
                Err(e) => {
                    self.outs[oi].dests = dests;
                    return Err(e);
                }
            }
        }
        let mut failures = Vec::new();
        let mut nowhere = Vec::new();
        let mut deliverer = self.deliverer.take().unwrap_or_else(|| {
            Deliverer::new(&self.ctx, self.project, self.ui.plugin_log()).with_steps(self.ui.clone())
        });
        for d in &dests {
            let profiles = self.project.profiles();
            let target = profiles.target_of(Role::Destination, &d.profile);
            // `None` for an entry that delivers nowhere.
            let kind = profiles
                .target(Role::Destination, &d.profile)
                .map(|o| o.kind.clone());
            if d.hold || self.opts.hold {
                self.hold(oi, d);
                self.outs[oi].deliveries.push(Delivery {
                    profile: d.profile.clone(),
                    kind,
                    target,
                    status: "held",
                    location: None,
                    attempts: None,
                    error_code: None,
                    error: None,
                });
                continue;
            }
            let (status, location, attempts, error) = match self.deliver_one(&mut deliverer, oi, d) {
                Ok(Some(sent)) => ("delivered", Some(sent.location), sent.attempts, None),
                Ok(None) => {
                    nowhere.push(crate::delivery::nowhere_note(&d.profile, &target));
                    ("not_delivered", None, 1, None)
                }
                Err(e) => {
                    let e = e.or(Code::DeliveryFailed);
                    failures.push(e.clone());
                    ("failed", None, 1, Some(e))
                }
            };
            let record = Delivery {
                profile: d.profile.clone(),
                kind,
                target,
                status,
                location,
                attempts: Some(attempts).filter(|n| *n > 1),
                error_code: error.as_ref().map(|e| e.code.clone()),
                error: error.map(|e| e.message),
            };
            self.outs[oi].deliveries.push(record);
        }
        self.deliverer = Some(deliverer);
        let o = &mut self.outs[oi];
        o.dests = dests;
        if !o.deliveries.iter().any(|d| d.status == "delivered") && !nowhere.is_empty() {
            o.delivery_note = Some(nowhere.join("; "));
        }
        match failures.len() {
            0 => Ok(()),
            1 if o.dests.len() == 1 => Err(failures.remove(0)),
            n => Err(Fail::new(
                failures[0].code.clone(),
                format!(
                    "{n} of {} destinations failed: {}",
                    o.dests.len(),
                    failures
                        .iter()
                        .map(|f| f.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )),
        }
    }

    /// Build (or reuse) the zip for output `oi`'s entry `d`: its files and every attached
    /// output's, in `<run>/zips/`. Entries with the same files, name and password share one.
    fn build_zip(&mut self, oi: usize, d: &RenderedDest) -> Result<PathBuf, Fail> {
        let z = d.zip.as_ref().expect("a zipping entry");
        let label = self.b.outputs[oi].label(oi);
        let output = self.b.outputs[oi]
            .name
            .clone()
            .unwrap_or_else(|| format!("output{}", oi + 1));
        let named = d
            .path
            .as_deref()
            .or(d.options.get("attachment_name").and_then(Json::as_str));
        let name = crate::zip_pack::zip_name(named, &output).map_err(|e| {
            Fail::new(
                Code::DeliveryFailed,
                format!("{label}: destination `{}`: {e}", d.profile),
            )
        })?;
        let mut files: Vec<PathBuf> = self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect();
        for a in &d.attach {
            if let Some(j) = self
                .b
                .outputs
                .iter()
                .position(|o| o.name.as_deref() == Some(a.as_str()))
            {
                files.extend(self.outs[j].files.iter().map(|(f, _)| f.clone()));
            }
        }
        let key = json!([name, files, z.password, z.encryption]).to_string();
        if let Some(p) = self.zips.get(&key) {
            return Ok(p.clone());
        }
        let to = self
            .run_dir
            .join("zips")
            .join(self.zips.len().to_string())
            .join(&name);
        let t = Instant::now();
        crate::zip_pack::build(&to, &files, z.password.as_deref(), z.encryption).map_err(|e| {
            Fail::new(
                Code::DeliveryFailed,
                format!("{label}: destination `{}`: {e}", d.profile),
            )
        })?;
        self.ui.step(
            Level::Info,
            "Zipped",
            &format!(
                "{} file(s) into {}",
                files.len(),
                rel(self.project.root(), &to).display()
            ),
            Some(t.elapsed()),
        );
        self.zips.insert(key, to.clone());
        Ok(to)
    }

    /// Record a held delivery of output `oi` to `d`, for `dre release`.
    fn hold(&mut self, oi: usize, d: &RenderedDest) {
        let zipped = d.zip_file.is_some();
        let attach: Vec<crate::release::HeldFile> = d
            .attach
            .iter()
            .filter(|_| !zipped)
            .filter_map(|name| {
                self.b
                    .outputs
                    .iter()
                    .position(|o| o.name.as_deref() == Some(name))
            })
            .flat_map(|j| {
                self.outs[j]
                    .files
                    .iter()
                    .map(|(f, _)| crate::release::held_file(f))
            })
            .collect();
        let idx = self.outs[oi].deliveries.len();
        self.held.push(crate::release::HeldEntry {
            id: format!(
                "{}/{idx}",
                self.b.outputs[oi]
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("output{}", oi + 1))
            ),
            output: self.b.outputs[oi].name.clone(),
            key: None,
            profile: d.profile.clone(),
            path: d.path.clone(),
            options: d.options.clone(),
            attach,
            rate: d.rate,
            message: self.outs[oi].message.clone(),
            files: match &d.zip_file {
                Some(z) => vec![z.clone()],
                None => self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect(),
            }
            .into_iter()
            .map(|f| crate::release::HeldFile {
                sha256: crate::checksum::sha256_file(&f).unwrap_or_default(),
                path: f,
            })
            .collect(),
        });
        self.ui.step(
            Level::Info,
            "Held",
            &format!(
                "{} to `{}`: held for `dre release`",
                self.b.outputs[oi].label(oi),
                d.profile
            ),
            None,
        );
    }

    /// Deliver output `oi`'s files (or its message, or the entry's zip) to one destination.
    /// `Ok(None)`: its entry for this run is `deliver: false`, so nothing was sent.
    fn deliver_one(
        &mut self,
        deliverer: &mut Deliverer<'a>,
        oi: usize,
        d: &RenderedDest,
    ) -> Result<Option<Sent>, Fail> {
        // An output is never delivered once the run is cancelled.
        self.not_cancelled()?;
        let files: Vec<PathBuf> = match &d.zip_file {
            Some(z) => vec![z.clone()],
            None => self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect(),
        };
        let o = &self.b.outputs[oi];
        let message = if o.is_message() {
            self.outs[oi].message.clone()
        } else {
            None
        };
        let attach: Vec<PathBuf> = d
            .attach
            .iter()
            .filter_map(|name| {
                self.b
                    .outputs
                    .iter()
                    .position(|o| o.name.as_deref() == Some(name))
            })
            .flat_map(|j| self.outs[j].files.iter().map(|(f, _)| f.clone()))
            .collect();
        let (store, label) = (self.store, self.label());
        // Bindings running at once never deliver to the same file.
        let claim = |remote: &str| match store.claim(&d.profile, remote, &label) {
            Some(other) => Err(Fail::new(
                Code::DeliveryFailed,
                format!(
                    "`{other}` delivers to `{remote}` on destination `{}` in this run too; Bindings running at once (`threads:`) need different paths (put the Set or a var in the path)",
                    d.profile
                ),
            )),
            None => Ok(()),
        };
        let request = Request::new(&d.profile, d.path.as_deref(), &d.options, &files)
            .message(message.as_ref(), &attach)
            .format(&o.format)
            .claim(&claim);
        let sent = deliverer.deliver(request)?;
        // With `zip:` the one file sent is the zip: the files in it keep no location of their
        // own, and the entry's delivery records the zip's.
        if let Some(sent) = &sent
            && d.zip_file.is_none()
        {
            for (at, (_, first)) in sent.files.iter().zip(self.outs[oi].files.iter_mut()) {
                if let Some(at) = at {
                    first.get_or_insert_with(|| at.clone());
                }
            }
        }
        Ok(sent)
    }

    fn snapshot(&self) -> Json {
        let sets: Vec<Json> = self
            .produced
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "columns": p.schema.fields().iter().map(|f| json!({
                        "name": f.name(),
                        "type": f.data_type().to_string(),
                        "type_shape": crate::schema::type_shape(f.data_type()),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "result_sets": sets })
    }

    fn schema_drift(&mut self) -> Vec<String> {
        let path = self.schema_dir.join("last_success.json");
        let mut current = self.snapshot();
        let Ok(prev) = std::fs::read_to_string(&path) else {
            crate::schema::redact(&mut current, self.ctx.secrets());
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let Ok(mut prev) = serde_json::from_str::<Json>(&prev) else {
            crate::schema::redact(&mut current, self.ctx.secrets());
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let original = prev.clone();
        crate::schema::redact(&mut prev, self.ctx.secrets());
        crate::schema::redact(&mut current, self.ctx.secrets());
        if self.ctx.secrets().enabled() {
            crate::schema::align_protection(&mut prev, &mut current);
        }
        if prev != original {
            let protected =
                serde_json::to_string_pretty(&prev).expect("serializing a serde_json::Value cannot fail");
            if let Err(error) = crate::fs::write_atomic(&path, protected.as_bytes()) {
                self.ui.warn(&format!(
                    "can't protect the existing schema snapshot at {}: {error}; continuing without rewriting it",
                    path.display()
                ));
            }
        }
        let drift = crate::schema::drift(&prev, &current);
        self.protected_snapshot = Some(current);
        drift
    }

    fn write_snapshot(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.schema_dir)?;
        let snapshot = self.protected_snapshot.clone().unwrap_or_else(|| {
            let mut snapshot = self.snapshot();
            crate::schema::redact(&mut snapshot, self.ctx.secrets());
            snapshot
        });
        crate::fs::write_atomic(
            &self.schema_dir.join("last_success.json"),
            serde_json::to_string_pretty(&snapshot)?.as_bytes(),
        )
    }

    fn write_results(&self, status: &Status, error: Option<&Fail>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.run_dir)?;
        if !self.held.is_empty() {
            crate::release::write_held(&self.run_dir, &self.run_id, &self.held)?;
        }
        // `outputs` lists every file (as before 0.3); `output_results` has one entry per output.
        let mut outputs: Vec<run_results::OutputFile> = Vec::new();
        let mut output_results: Vec<run_results::OutputResult> = Vec::new();
        for (o, run) in self.b.outputs.iter().zip(&self.outs) {
            let files: Vec<run_results::OutputFileRef> = run
                .files
                .iter()
                .map(|(f, delivered)| run_results::OutputFileRef {
                    path: record_path(self.project, f),
                    size: std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
                    delivered_to: delivered.clone(),
                })
                .collect();
            outputs.extend(files.iter().map(|f| run_results::OutputFile {
                path: f.path.clone(),
                size: f.size,
                delivered_to: f.delivered_to.clone(),
                output: o.name.clone(),
            }));
            let queries: Vec<String> = self
                .b
                .queries
                .iter()
                .filter(|q| q.tab && o.feeds(&q.query))
                .map(|q| q.query.clone())
                .collect();
            let status = match (run.status, status) {
                (Some(s), _) => s,
                (None, Status::Error) => OutputStatus::Failed,
                (None, _) => OutputStatus::Kept,
            }
            .as_str();
            output_results.push(run_results::OutputResult {
                name: o.name.clone(),
                format: o.format.clone(),
                queries,
                status,
                error: run.error.clone(),
                files,
                delivery: run.delivery_note.clone(),
                deliveries: run.deliveries.clone(),
                when: run.when,
                reason: run.reason.clone(),
                no_data: run.no_data.clone(),
                dropped: run.dropped.clone(),
                for_each: run.for_each.clone(),
                message: run.message.clone().map(|(title, text)| run_results::Message {
                    title: self.ctx.secrets().mask_message_title(&title).into_owned(),
                    text: self.ctx.secrets().mask_markdown(&text).into_owned(),
                }),
            });
        }
        // In delivery order: file outputs, then messages.
        let order = (0..self.outs.len())
            .filter(|&i| !self.b.outputs[i].is_message())
            .chain((0..self.outs.len()).filter(|&i| self.b.outputs[i].is_message()));
        let deliveries: Vec<Delivery> = order
            .flat_map(|i| self.outs[i].deliveries.iter().cloned())
            .collect();
        let notes: Vec<&str> = self
            .outs
            .iter()
            .filter_map(|o| o.delivery_note.as_deref())
            .collect();
        let delivery = (!notes.is_empty()).then(|| notes.join("; "));
        let results = RunResults {
            schema_version: run_results::SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            binding: self.b.dir_name().to_string(),
            managed: self.report.managed,
            profile: self.parsed.inherited.clone(),
            connections: self
                .parsed
                .connections()
                .into_iter()
                .map(str::to_string)
                .collect(),
            target: self.target.clone(),
            schedule: self.opts.schedule.clone(),
            schedule_vars: self.schedule_vars.clone(),
            vars: self.rendered_vars.clone(),
            run_date: self.date.to_string(),
            scheduled_at: self.opts.scheduled_at.map(rfc3339),
            timezone: self.calendar.tz.name().to_string(),
            params: self.opts.params(self.date),
            status: *status,
            error: error.map(|e| e.message.clone()),
            error_code: error.map(|e| e.code.clone()),
            error_kind: error.map(|e| e.code.kind()),
            preview: self.opts.preview.is_some(),
            row_limit: self.opts.preview,
            started_at: self.started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            duration_ms: self.started.elapsed().as_millis() as u64,
            result_sets: self
                .produced
                .iter()
                .map(|p| run_results::ResultSet {
                    name: p.name.clone(),
                    query: p.query.clone(),
                    connection: p.connection.clone(),
                    rows: p.rows,
                    columns: p.schema.fields().iter().map(|f| f.name().to_string()).collect(),
                })
                .collect(),
            outputs,
            output_results,
            delivery,
            deliveries,
            schema_drift: self.drift.clone(),
            target_path: self.project.target_dir().to_path_buf(),
            settings: self.project.settings().clone(),
            manifest_checksum: self.opts.manifest_checksum.clone(),
            retry_of: self.retry.as_ref().map(|r| r.original.clone()),
            release_of: None,
            released_by: None,
            released: Vec::new(),
        };
        self.store.write_results(&self.run_dir, &results)
    }

    /// `dre validate --live`: execute temp creates, `check` everything else, each on its
    /// connection; then check declared source columns against the database.
    fn live_check(&mut self, statements: &[Statement]) -> Result<(), Fail> {
        let pool = self.pool.clone().expect("pool");
        let mut failures = Vec::new();
        let mut unexecuted_setup: BTreeMap<String, (PathBuf, usize)> = BTreeMap::new();
        let mut uncheckable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let sql_log = self.ui.sql_log();
        for st in statements {
            if uncheckable.contains(&st.connection) {
                continue;
            }
            let session = pool.session(&st.connection)?;
            let mut s = session.lock().unwrap();
            if !s.get()?.has(dre_protocol::CAP_CHECK) {
                let kind = pool.kind(&st.connection).unwrap_or_default();
                self.ui.warn(&format!(
                    "  not checkable: the `{kind}` source plugin (connection `{}`) can't check statements without running them",
                    st.connection
                ));
                uncheckable.insert(st.connection.clone());
                continue;
            }
            let verb = if st.kind == StatementKind::TempCreate {
                ""
            } else {
                " (check)"
            };
            sql_log(
                &format!("{}:{}{verb}", st.file.display(), st.line),
                &protected_sql(self.ctx.secrets(), &st.text, st.sensitive),
            );
            if st.kind == StatementKind::TempCreate {
                let result = {
                    let (process, _log_scope) = s.statement(st.sensitive)?;
                    process.execute(&st.text, None, &mut |_, _| Ok(()))
                };
                if let Err(e) = result {
                    failures.push(format!(
                        "{}:{}: {}",
                        st.file.display(),
                        st.line,
                        masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                    ));
                }
                continue;
            }
            let result = {
                let (process, _log_scope) = s.statement(st.sensitive)?;
                process.check(&st.text)
            };
            if let Err(e) = result {
                let mut msg = format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                );
                if let Some((f, l)) = unexecuted_setup.get(&st.connection) {
                    msg.push_str(&format!(
                        " (may be a false positive: the setup statement at {}:{l} wasn't executed during the check)",
                        f.display()
                    ));
                }
                failures.push(msg);
            }
            if st.kind == StatementKind::Other {
                unexecuted_setup.insert(st.connection.clone(), (st.file.clone(), st.line));
            }
        }
        self.check_source_columns(&pool, &mut failures);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Fail::new(Code::QueryFailed, failures.join("\n    ")))
        }
    }

    /// Declared source `columns` against the real table, once per source table, connection and
    /// target in this process: each must exist, and match its `data_type` (loosely) where given.
    fn check_source_columns(&mut self, pool: &Arc<Pool>, failures: &mut Vec<String>) {
        for q in self.parsed.queries.clone() {
            let Some(conn) = q.connection.clone() else {
                continue;
            };
            for key in &q.sources {
                let Some(r) = self.parsed.sources.get(key).cloned() else {
                    continue;
                };
                let Some(declared) = self
                    .project
                    .sources()
                    .get(&r.source)
                    .and_then(|s| s.table(&r.table))
                    .map(|t| t.columns.clone())
                    .filter(|c| !c.is_empty())
                else {
                    continue;
                };
                let id = (
                    self.project.root().to_path_buf(),
                    key.clone(),
                    conn.clone(),
                    self.project.profiles().target_of(Role::Connection, &conn),
                );
                if !self.ctx.first_check(id) {
                    continue;
                }
                let quote = if r.quoting.any() {
                    let kind = pool.kind(&conn).unwrap_or_default();
                    match pool.connections.identifier_quote(&kind) {
                        Ok(q) => q,
                        Err(e) => {
                            failures.push(format!("source `{key}`: can't quote its name: {e}"));
                            continue;
                        }
                    }
                } else {
                    None
                };
                let rel = r.relation(quote.as_deref());
                let runner = PoolRunner {
                    pool: pool.clone(),
                    secrets: self.ctx.secrets().clone(),
                    default: Some(conn.clone()),
                    log: self.ui.sql_log(),
                };
                let actual = match runner.columns(&format!("select * from {rel} where 1=0"), None) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!("source `{key}` ({rel} on connection `{conn}`): {e}"));
                        continue;
                    }
                };
                for c in &declared {
                    let Some(a) = actual.iter().find(|a| a.name.eq_ignore_ascii_case(&c.name)) else {
                        failures.push(format!(
                            "source `{key}`: declared column `{}` isn't in {rel} on connection `{conn}` (it has: {})",
                            c.name,
                            actual.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
                        ));
                        continue;
                    };
                    let Some(dt) = &c.data_type else { continue };
                    match crate::coltypes::matches(dt, &a.data_type) {
                        Some(true) => {}
                        Some(false) => failures.push(format!(
                            "source `{key}`: column `{}` is declared `{dt}`, but {rel} on connection `{conn}` returns {}",
                            c.name, a.data_type
                        )),
                        None => self.ui.warn(&format!(
                            "  source `{key}`: column `{}`'s `data_type: {dt}` isn't a type DRE can compare, so only its presence was checked",
                            c.name
                        )),
                    }
                }
            }
        }
    }
}

/// The source session, started on first use (rendering may need it for `run_query()`).
struct Session {
    host: Arc<dyn PluginHost>,
    secrets: Secrets,
    path: Result<crate::plugins::Located, String>,
    connection: Result<JsonMap<String, Json>, String>,
    cwd: PathBuf,
    log: LogSink,
    /// Whether plugin diagnostics for the active statement must be hidden rather than exactly
    /// masked. Connection diagnostics always use exact-value masking.
    opaque_log: Arc<AtomicBool>,
    /// Unmanaged reports run read-only unless they create temp objects.
    unmanaged: bool,
    read_only: bool,
    /// A lookup was loaded into a temp table, so the session must stay open as it is.
    loaded: bool,
    proc_: Option<Box<dyn Plugin>>,
}

impl Session {
    fn new(
        ctx: &Context,
        path: Result<crate::plugins::Located, String>,
        connection: Result<JsonMap<String, Json>, String>,
        cwd: PathBuf,
        log: LogSink,
        unmanaged: bool,
    ) -> Session {
        let opaque_log = Arc::new(AtomicBool::new(false));
        let protect = Arc::clone(&opaque_log);
        let downstream = log;
        let secrets = ctx.secrets().clone();
        let log: LogSink = Arc::new(move |plugin, line| {
            let line = secrets.mask_source_error(line, protect.load(Ordering::SeqCst));
            downstream(plugin, &line);
        });
        Session {
            host: ctx.host().clone(),
            secrets: ctx.secrets().clone(),
            path,
            connection,
            cwd,
            log,
            opaque_log,
            unmanaged,
            read_only: unmanaged,
            loaded: false,
            proc_: None,
        }
    }

    fn want_read_only(&mut self, ro: bool) {
        if self.read_only != ro {
            // Reopen if a session was already started in the other mode.
            self.close();
            self.read_only = ro;
        }
    }

    fn get(&mut self) -> Result<&mut dyn Plugin, String> {
        if self.proc_.is_none() {
            let plugin = self.path.clone()?;
            let connection = self.connection.clone()?;
            let mut p = self
                .host
                .start(&plugin, self.log.clone(), Some(&self.cwd))
                .map_err(|e| masked_source_error(&self.secrets, &e, false))?;
            let open = if self.unmanaged && self.read_only && p.has(CAP_READ_ONLY) {
                Open::ReadOnly
            } else {
                Open::ReadWrite
            };
            p.open(connection, open).map_err(|e| {
                let protected = masked_source_error(&self.secrets, &e, false);
                format!("can't open the source connection: {protected}")
            })?;
            self.proc_ = Some(p);
        }
        Ok(self.proc_.as_deref_mut().expect("the process was started above"))
    }

    fn statement(&mut self, sensitive: bool) -> Result<(&mut dyn Plugin, OpaqueLogScope), String> {
        let opaque_log = Arc::clone(&self.opaque_log);
        let process = self.get()?;
        let stderr = if sensitive { process.stderr_sync() } else { None };
        let scope = OpaqueLogScope::new(opaque_log, sensitive, stderr);
        Ok((process, scope))
    }

    fn close(&mut self) {
        if let Some(p) = self.proc_.take() {
            p.close();
        }
    }
}

/// Hides a sensitive statement's plugin diagnostics while it runs. The plugin's stderr is logged
/// from another thread, so the scope waits for the lines the plugin wrote before showing
/// diagnostics again; if the plugin keeps writing, they stay hidden until the next statement.
#[must_use = "dropping the scope shows diagnostics again at once"]
struct OpaqueLogScope(Arc<AtomicBool>, Option<dre_protocol::host::StderrSync>);

impl OpaqueLogScope {
    fn new(active: Arc<AtomicBool>, sensitive: bool, stderr: Option<dre_protocol::host::StderrSync>) -> Self {
        active.store(sensitive, Ordering::SeqCst);
        Self(active, stderr)
    }
}

impl Drop for OpaqueLogScope {
    fn drop(&mut self) {
        if let Some(stderr) = &self.1
            && !stderr.wait(Duration::from_secs(1))
        {
            return;
        }
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The Binding's connections: each one's session, opened when a query first needs it and held
/// until the report ends.
struct Pool {
    connections: Arc<ProfileConnections>,
    cwd: PathBuf,
    log: LogSink,
    unmanaged: bool,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Session>>>>,
}

impl Pool {
    fn new(connections: Arc<ProfileConnections>, cwd: PathBuf, log: LogSink, unmanaged: bool) -> Pool {
        Pool {
            connections,
            cwd,
            log,
            unmanaged,
            sessions: Mutex::default(),
        }
    }

    /// A connection's settings for this run.
    fn target(&self, name: &str) -> Result<ProfileTarget, String> {
        let profiles = &self.connections.profiles;
        match profiles.entry(Role::Connection, name) {
            Entry::Use(o) => Ok(o.clone()),
            Entry::Unknown => Err(format!(
                "connection `{name}` isn't under `{}:` in {}",
                profiles.section_key(Role::Connection),
                profiles.path.display()
            )),
            // A connection never has `deliver: false`.
            Entry::Missing | Entry::Nowhere => Err(profiles.missing_entry(Role::Connection, name)),
        }
    }

    /// A connection's plugin type.
    fn kind(&self, name: &str) -> Result<String, String> {
        Ok(self.target(name)?.kind)
    }

    fn session(&self, name: &str) -> Result<Arc<Mutex<Session>>, String> {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            return Ok(s.clone());
        }
        let output = self.target(name)?;
        let id = crate::project::PluginId::new(PluginKind::Source, output.kind.clone());
        let ctx = &self.connections.ctx;
        let path = ctx
            .host()
            .locate(&self.connections.root, &self.connections.plugins, &id)
            .map_err(String::from);
        let session = Arc::new(Mutex::new(Session::new(
            ctx,
            path,
            render_connection(&output, ctx.env()),
            self.cwd.clone(),
            self.log.clone(),
            self.unmanaged,
        )));
        self.sessions
            .lock()
            .unwrap()
            .insert(name.to_string(), session.clone());
        Ok(session)
    }

    fn close_all(&self) {
        for s in self.sessions.lock().unwrap().values() {
            if let Ok(mut s) = s.lock() {
                s.close();
            }
        }
    }
}

/// `run_query()`, `columns()` and lookups for one renderer: on the named connection's session,
/// else on the renderer's own (`default`).
struct PoolRunner {
    pool: Arc<Pool>,
    secrets: Secrets,
    default: Option<String>,
    log: LogSink,
}

impl PoolRunner {
    fn session(&self, profile: Option<&str>) -> Result<Arc<Mutex<Session>>, String> {
        let name = profile.or(self.default.as_deref()).ok_or(
            "no connection here: pass `profile=`, or give the report a `profile:` (this isn't a query's SQL)",
        )?;
        self.pool.session(name)
    }
}

impl QueryRunner for PoolRunner {
    fn run_query(&self, sql: &str, max_rows: u64, profile: Option<&str>) -> Result<QueryRows, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = self.secrets.contains_secret(sql);
        // An unmanaged report may only read, and run_query() runs before the file's own
        // statements are checked, so it gets the same rule up front.
        if s.unmanaged
            && let Some(bad) = sqlsplit::split(sql)
                .into_iter()
                .find(|st| sqlsplit::classify(&st.text) != StatementKind::Read)
        {
            return Err(format!(
                "run_query() in an unmanaged report may only read, but got `{}`; give the report a YAML to declare it",
                masked_sql_summary(&self.secrets, &bad.text, 60, sensitive)
            ));
        }
        (self.log)("run_query()", &protected_sql(&self.secrets, sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut out = QueryRows::default();
        let mut too_many = false;
        let exec = p
            .execute(sql, Some(max_rows + 1), &mut |schema, batch| {
                if out.columns.is_empty() {
                    out.columns = schema.fields().iter().map(|f| f.name().to_string()).collect();
                }
                if out.rows.len() as u64 + batch.num_rows() as u64 > max_rows {
                    too_many = true;
                    return Ok(());
                }
                out.rows.extend(crate::values::batch_rows(&batch));
                Ok(())
            })
            .map_err(|e| {
                format!(
                    "run_query() failed: {}",
                    masked_source_error(&self.secrets, &e, sensitive)
                )
            })?;
        if too_many {
            return Err(format!(
                "run_query() returned more than {max_rows} rows; it's meant for small lookups — raise the cap with `run_query(sql, max_rows=N)` or `run_query_max_rows` in dre_project.yml"
            ));
        }
        if let Execution::Result { schema, .. } = exec
            && out.columns.is_empty()
        {
            out.columns = schema.fields().iter().map(|f| f.name().to_string()).collect();
        }
        Ok(out)
    }

    fn columns(&self, sql: &str, profile: Option<&str>) -> Result<Vec<Column>, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = self.secrets.contains_secret(sql);
        (self.log)("columns()", &protected_sql(&self.secrets, sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut schema: Option<SchemaRef> = None;
        let exec = p
            .execute(sql, Some(1), &mut |sch, _| {
                schema.get_or_insert_with(|| sch.clone());
                Ok(())
            })
            .map_err(|e| masked_source_error(&self.secrets, &e, sensitive))?;
        if let Execution::Result { schema: sch, .. } = exec {
            schema.get_or_insert(sch);
        }
        let schema = schema.ok_or("the query returned no result set")?;
        Ok(schema
            .fields()
            .iter()
            .map(|f| Column {
                name: f.name().to_string(),
                data_type: f.data_type().to_string(),
            })
            .collect())
    }

    fn load(&self, name: &str, table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        let session = self.session(None)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        // A temp table is allowed even in an unmanaged report; it needs a writable session.
        s.want_read_only(false);
        if !s.get()?.has(CAP_LOAD) {
            return Ok(None);
        }
        let batch = table.to_batch()?;
        (self.log)(
            &format!("lookup `{name}`"),
            &format!(
                "-- {} rows loaded through the plugin's `load` request",
                batch.num_rows()
            ),
        );
        let r = s.get()?.load(name, batch).map_err(|e| e.to_string())?;
        s.loaded = true;
        Ok(Some(r))
    }
}

/// Render every string inside a destination option value.
fn render_json(renderer: &Renderer, file: &Path, v: &Json) -> Result<Json, RenderError> {
    Ok(match v {
        Json::String(s) => Json::String(renderer.render(file, s)?),
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| render_json(renderer, file, v))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), render_json(renderer, file, v)?)))
                .collect::<Result<_, RenderError>>()?,
        ),
        other => other.clone(),
    })
}

/// Render `env_var()` (and only that) inside a profile output's string fields.
pub(crate) fn render_connection(
    output: &ProfileTarget,
    process: &crate::Env,
) -> Result<JsonMap<String, Json>, String> {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    let process = process.clone();
    env.add_function("env_var", move |name: String, default: Option<String>| -> Result<String, minijinja::Error> {
        process.var(&name).map(str::to_string).or(default).ok_or_else(|| {
            minijinja::Error::new(
                minijinja::ErrorKind::UndefinedError,
                format!("`env_var('{name}')`: environment variable `{name}` is not set and no default is given"),
            )
        })
    });
    fn walk(env: &minijinja::Environment<'_>, v: &Json) -> Result<Json, String> {
        Ok(match v {
            Json::String(s) if crate::preflight::is_templated(s) => Json::String(
                env.render_str(s, ())
                    .map_err(|e| format!("profiles.yml: {}", e.detail().unwrap_or("render error")))?,
            ),
            Json::Array(a) => Json::Array(a.iter().map(|x| walk(env, x)).collect::<Result<_, _>>()?),
            Json::Object(o) => Json::Object(
                o.iter()
                    .map(|(k, x)| Ok((k.clone(), walk(env, x)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }
    match walk(&env, &Json::Object(output.fields.clone()))? {
        Json::Object(o) => Ok(o),
        _ => unreachable!(),
    }
}

/// Locate the plugin for a declared type among the project's packages, honouring `dre.lock`
/// pins and declared constraints.
pub fn find_plugin(
    ctx: &Context,
    project: &Project,
    kind: PluginKind,
    name: &str,
) -> Result<crate::plugins::Located, crate::plugins::LocateError> {
    ctx.host().locate(
        project.root(),
        project.plugins(),
        &crate::project::PluginId::new(kind, name),
    )
}

/// `connection`, `destination` and `profile()` from `profiles.yml`, for the run's target. A
/// field is secret when the plugin's `describe` says so; when the plugin can't be asked, when its
/// name looks like one; and always when its value comes from a `DRE_SECRET_*` variable.
struct ProfileConnections {
    /// The run's plugin host, secrets and `describe` cache.
    ctx: Context,
    /// Each profile's entry for the run comes from here.
    profiles: Profiles,
    root: PathBuf,
    plugins: Vec<crate::project::PluginRequirement>,
    log: LogSink,
}

const SECRET_NAME_WORDS: &[&str] = &["password", "secret", "token", "key", "credential"];

impl ProfileConnections {
    fn view(&self, role: Role, name: &str) -> Result<Connection, String> {
        let target = self.profiles.target_of(role, name);
        let out: ProfileTarget = match self.profiles.entry(role, name) {
            Entry::Use(o) => o.clone(),
            Entry::Nowhere => {
                return Err(format!(
                    "destination `{name}`: `{target}` delivers nowhere (`deliver: false`), so it has no settings"
                ));
            }
            Entry::Missing => return Err(self.profiles.missing_entry(role, name)),
            Entry::Unknown => {
                return Err(format!(
                    "no {} profile `{name}` in {}",
                    role.as_str(),
                    self.profiles.path.display()
                ));
            }
        };
        let mut secrets: Vec<String> = out
            .fields
            .iter()
            .filter(|(_, v)| v.as_str().is_some_and(|s| s.contains(crate::secrets::PREFIX)))
            .map(|(k, _)| k.clone())
            .collect();
        match self.described(role, &out.kind) {
            Some(d) => secrets.extend(d.secrets),
            None => secrets.extend(
                out.fields
                    .keys()
                    .filter(|k| {
                        let k = k.to_lowercase();
                        SECRET_NAME_WORDS.iter().any(|w| k.contains(w))
                    })
                    .cloned(),
            ),
        }
        self.ctx.secrets().register_secret_fields(&out.fields, &secrets);
        let fields = render_connection(&out, self.ctx.env())?;
        Ok(Connection {
            profile: name.to_string(),
            target,
            kind: out.kind.clone(),
            fields,
            secrets,
        })
    }

    /// The plugin's `describe` reply, asked once per plugin type in a run.
    fn described(&self, role: Role, kind: &str) -> Option<Described> {
        let key = (self.root.clone(), role, kind.to_string());
        self.ctx.described(key, || self.describe(role, kind))
    }

    /// Ask the plugin for its `describe` reply; `None` when it can't be asked.
    fn describe(&self, role: Role, kind: &str) -> Option<Described> {
        let plugin_kind = match role {
            Role::Connection => PluginKind::Source,
            Role::Destination => PluginKind::Destination,
        };
        {
            if kind == LOCAL_TYPE {
                return Some(Described {
                    secrets: Vec::new(),
                    identifier_quote: None,
                    string_literal: None,
                    capabilities: Vec::new(),
                    message_limit: None,
                });
            }
            let id = crate::project::PluginId::new(plugin_kind, kind);
            let host = self.ctx.host();
            let plugin = host.locate(&self.root, &self.plugins, &id).ok()?;
            let mut p = host.start(&plugin, self.log.clone(), Some(&self.root)).ok()?;
            let capabilities = p.info().capabilities.clone();
            let d = p.describe().ok();
            p.close();
            let d = d?;
            Some(Described {
                secrets: d
                    .connection_fields
                    .into_iter()
                    .filter(|f| f.secret)
                    .map(|f| f.name)
                    .collect(),
                identifier_quote: d.identifier_quote,
                string_literal: d.string_literal,
                capabilities,
                message_limit: d.message_limit,
            })
        }
    }
}

impl Connections for ProfileConnections {
    fn profile(&self, name: &str, role: Option<&str>) -> Result<Connection, String> {
        let role = match role {
            Some("connection") => Role::Connection,
            Some(_) => Role::Destination,
            None => {
                let c = self.profiles.get(Role::Connection, name).is_some();
                let d = self.profiles.get(Role::Destination, name).is_some();
                match (c, d) {
                    (true, true) => {
                        return Err(format!(
                            "`{name}` is both a connection and a destination profile; say which with `profile('{name}', role='connection')` or `role='destination'`"
                        ));
                    }
                    (true, false) => Role::Connection,
                    _ => Role::Destination,
                }
            }
        };
        self.view(role, name)
    }

    fn identifier_quote(&self, kind: &str) -> Result<Option<String>, String> {
        match self.described(Role::Connection, kind) {
            Some(d) => Ok(d.identifier_quote),
            None => Err(format!(
                "the `{kind}` source plugin couldn't be asked for its identifier quote character"
            )),
        }
    }

    fn string_literal(&self, kind: &str) -> Result<Option<String>, String> {
        Ok(self
            .described(Role::Connection, kind)
            .and_then(|d| d.string_literal))
    }
}

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

impl BindingRun<'_> {
    /// What this Binding produced, for its end-of-run line.
    fn summary_line(&self) -> String {
        let rows: u64 = self.produced.iter().map(|p| p.rows).sum();
        let mut parts = Vec::new();
        if !self.produced.is_empty() {
            let n = self.produced.len();
            parts.push(format!(
                "{n} result set{}, {} row{}",
                if n == 1 { "" } else { "s" },
                thousands(rows),
                if rows == 1 { "" } else { "s" }
            ));
        }
        let names: Vec<String> = self
            .files()
            .map(|(f, _)| f.file_name().unwrap_or_default().to_string_lossy().to_string())
            .collect();
        if !names.is_empty() {
            parts.push(format!("→ {}", names.join(", ")));
        }
        // Every destination's location, not just the first one each file reached.
        let delivered: Vec<&str> = self
            .outs
            .iter()
            .flat_map(|o| o.deliveries.iter())
            .filter_map(|d| d.location.as_deref())
            .collect();
        let notes: Vec<&str> = self
            .outs
            .iter()
            .filter(|o| !o.dests.is_empty())
            .filter_map(|o| o.delivery_note.as_deref())
            .collect();
        if !delivered.is_empty() {
            parts.push(format!("→ {}", delivered.join(", ")));
        } else if !notes.is_empty() && self.opts.preview.is_none() {
            parts.push(format!("({})", notes.join("; ")));
        }
        parts.join(" ")
    }

    /// Every file the Binding's outputs wrote, and where each was first delivered.
    fn files(&self) -> impl Iterator<Item = &(PathBuf, Option<String>)> {
        self.outs.iter().flat_map(|o| o.files.iter())
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
