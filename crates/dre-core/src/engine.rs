//! The run engine's parts around a Binding's run ([`crate::run`]):
//!
//! - the **plan**: which Bindings run, with every profile's entry checked ([`crate::run::plan`]);
//! - the **executor**: runs a plan on a worker thread, cancellable through a [`CancelToken`],
//!   reporting what happens as [`RunEvent`]s on a channel. The terminal UI, `--log-format json`,
//!   the log file and `run_results.json` are consumers; whoever drives the run decides how
//!   events look ([`dispatch`] replays them into a [`Ui`]);
//! - the **[`RunStore`]**: where a run's files go in the target path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use dre_protocol::host::LogSink;
use serde_json::{Map as JsonMap, Value as Json};

use crate::run::{BindingOutcome, BindingPlan, Level, RunTargets, ShownMessage, Ui};

/// Something that happened during a run.
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// The run's target and each used profile's entry, once they're all known to exist.
    Targets(RunTargets),
    /// The number of Bindings about to run.
    Plan(usize),
    BindingStart {
        report: String,
        set: Option<String>,
    },
    /// The schedule a Binding runs under (if any) and every var it uses.
    BindingVars {
        schedule: Option<String>,
        schedule_vars: Option<JsonMap<String, Json>>,
        vars: JsonMap<String, Json>,
    },
    /// One step inside the current Binding.
    Step {
        level: Level,
        verb: String,
        detail: String,
        elapsed: Option<Duration>,
    },
    Warn(String),
    BindingEnd(BindingOutcome),
    /// A Binding compiled: what it would do.
    Compiled(BindingPlan),
    /// `--preview`: a rendered message, shown instead of sent.
    Message(ShownMessage),
    /// A line a plugin wrote to stderr: `(plugin, line)`.
    PluginLog(String, String),
    /// The full text of a statement sent to a source: `(label, sql)`.
    Sql(String, String),
}

/// Where a running Binding sends its events. Cheap to clone, and `Send`: the executor's worker
/// and plugins' stderr readers hold one each.
#[derive(Clone)]
pub struct Events(Sender<RunEvent>);

impl Events {
    pub fn new(sender: Sender<RunEvent>) -> Events {
        Events(sender)
    }

    /// Send an event. Nobody listening (the run was abandoned) isn't an error.
    pub fn emit(&self, e: RunEvent) {
        let _ = self.0.send(e);
    }

    pub fn step(&self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>) {
        self.emit(RunEvent::Step {
            level,
            verb: verb.to_string(),
            detail: detail.to_string(),
            elapsed,
        });
    }

    pub fn warn(&self, msg: &str) {
        self.emit(RunEvent::Warn(msg.to_string()));
    }

    pub fn compiled(&self, plan: &BindingPlan) {
        self.emit(RunEvent::Compiled(plan.clone()));
    }

    pub fn message(&self, message: &ShownMessage) {
        self.emit(RunEvent::Message(message.clone()));
    }

    pub fn binding_vars(
        &self,
        schedule: Option<&str>,
        schedule_vars: Option<&JsonMap<String, Json>>,
        vars: &JsonMap<String, Json>,
    ) {
        self.emit(RunEvent::BindingVars {
            schedule: schedule.map(str::to_string),
            schedule_vars: schedule_vars.cloned(),
            vars: vars.clone(),
        });
    }

    /// Where plugin stderr goes: into the event stream.
    pub fn plugin_log(&self) -> LogSink {
        let events = self.clone();
        Arc::new(move |plugin: &str, line: &str| events.emit(RunEvent::PluginLog(plugin.into(), line.into())))
    }

    /// Where every statement's full text goes: into the event stream.
    pub fn sql_log(&self) -> LogSink {
        let events = self.clone();
        Arc::new(move |label: &str, sql: &str| events.emit(RunEvent::Sql(label.into(), sql.into())))
    }
}

/// Replay an event into a [`Ui`]. `plugin_log` and `sql_log` are the UI's own sinks, taken once
/// before the run.
pub fn dispatch(ui: &mut dyn Ui, e: RunEvent, plugin_log: &LogSink, sql_log: &LogSink) {
    match e {
        RunEvent::Targets(t) => ui.targets(&t),
        RunEvent::Plan(n) => ui.plan(n),
        RunEvent::BindingStart { report, set } => ui.binding_start(&report, set.as_deref()),
        RunEvent::BindingVars {
            schedule,
            schedule_vars,
            vars,
        } => ui.binding_vars(schedule.as_deref(), schedule_vars.as_ref(), &vars),
        RunEvent::Step {
            level,
            verb,
            detail,
            elapsed,
        } => ui.step(level, &verb, &detail, elapsed),
        RunEvent::Warn(msg) => ui.warn(&msg),
        RunEvent::BindingEnd(o) => ui.binding_end(&o),
        RunEvent::Compiled(p) => ui.compiled(&p),
        RunEvent::Message(m) => ui.message(&m),
        RunEvent::PluginLog(plugin, line) => plugin_log(&plugin, &line),
        RunEvent::Sql(label, sql) => sql_log(&label, &sql),
    }
}

/// Asks a run to stop. Clones share the request.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Where a run's files go in the target path, for one Binding (`<report>/<binding>`):
/// `compiled/` (rendered SQL), `run/` (outputs and `run_results.json`) and `schema/` (the schema
/// snapshot drift is checked against).
#[derive(Debug, Clone)]
pub struct RunStore {
    target: PathBuf,
}

impl RunStore {
    pub fn new(target: &Path) -> RunStore {
        RunStore {
            target: target.to_path_buf(),
        }
    }

    fn binding(&self, kind: &str, report: &str, binding: &str) -> PathBuf {
        self.target.join(kind).join(report).join(binding)
    }

    /// Where a Binding's rendered SQL goes.
    pub fn compiled_dir(&self, report: &str, binding: &str) -> PathBuf {
        self.binding("compiled", report, binding)
    }

    /// Where a Binding's outputs and `run_results.json` go.
    pub fn run_dir(&self, report: &str, binding: &str) -> PathBuf {
        self.binding("run", report, binding)
    }

    /// Where a Binding's schema snapshot goes.
    pub fn schema_dir(&self, report: &str, binding: &str) -> PathBuf {
        self.binding("schema", report, binding)
    }

    /// Write a Binding's `run_results.json` into its run folder (secrets masked).
    pub fn write_results(
        &self,
        run_dir: &Path,
        results: &crate::run_results::RunResults,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(run_dir)?;
        std::fs::write(
            run_dir.join("run_results.json"),
            (crate::secrets::to_json_pretty(results)? + "\n").as_bytes(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_is_seen_by_every_clone() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }

    #[test]
    fn the_store_lays_out_a_bindings_folders() {
        let s = RunStore::new(Path::new("/t"));
        assert_eq!(s.run_dir("daily", "default"), Path::new("/t/run/daily/default"));
        assert_eq!(s.compiled_dir("daily", "eu"), Path::new("/t/compiled/daily/eu"));
        assert_eq!(s.schema_dir("daily", "eu"), Path::new("/t/schema/daily/eu"));
    }

    #[test]
    fn events_arrive_in_order_and_log_sinks_feed_the_stream() {
        let (tx, rx) = std::sync::mpsc::channel();
        let events = Events::new(tx);
        events.warn("one");
        events.sql_log()("q.sql:1", "select 1");
        events.plugin_log()("duckdb", "hello");
        drop(events);
        let got: Vec<String> = rx.iter().map(|e| format!("{e:?}")).collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("Warn") && got[1].starts_with("Sql") && got[2].starts_with("PluginLog"));
    }
}
