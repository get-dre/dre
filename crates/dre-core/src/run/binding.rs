//! One Binding's run: its lock and run folder around the stages, and its outcome.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde_json::Value as Json;

use super::mode::{Deliver, Source, Stages};
use super::session::{Pool, PoolRunner, ProfileConnections};
use super::{
    BindingOutcome, BindingRun, Fail, OutputRun, OutputStatus, Planned, RunOptions, Status, rel, thousands,
};
use crate::codes::Code;
use crate::context::Context;
use crate::engine::{CancelReason, Events, Level, RunEvent, RunStore};
use crate::project::Project;
use crate::render::{Mode as RenderMode, Renderer, RendererConfig};

impl<'a> BindingRun<'a> {
    pub(super) fn new(
        ctx: &Context,
        project: &'a Project,
        planned: &'a Planned<'a>,
        opts: &'a RunOptions,
        ui: Events,
        store: &'a RunStore,
        processes: super::processes::Pool,
    ) -> Self {
        let (report, b) = (planned.report, &planned.binding);
        let schedule = opts
            .schedule
            .as_ref()
            .and_then(|n| project.schedules().iter().find(|e| &e.name == n));
        let schedule_vars = schedule.map(|e| e.vars.clone());
        // Names were checked when the project and the command line were read.
        let calendar = crate::parse::calendar(
            project,
            report,
            opts.timezone.as_deref(),
            opts.schedule.as_deref(),
        );
        let date = crate::parse::run_date(&calendar, opts.date, opts.scheduled_at);
        // Binding vars, then the schedule's, then `--var` on top.
        let vars = planned.vars.clone();
        let mut rendered_vars = vars.clone();
        rendered_vars.extend(
            opts.vars
                .iter()
                .map(|(k, x)| (k.clone(), Json::String(x.clone()))),
        );
        ui.binding_vars(opts.schedule.as_deref(), schedule_vars.as_ref(), &rendered_vars);
        let stages = Stages::select(
            &opts.mode,
            report.managed,
            b.outputs.iter().any(|o| o.for_each.is_some()),
            opts.rows.limit,
            opts.rows.only_keys.is_some(),
        );
        let started_at = chrono::Utc::now();
        let runs = crate::runs::BindingRuns::new(&store.run_dir(&report.name, b.dir_name()));
        // A dry run (compile, validate) makes no run folder: its plan shows where files would go.
        let run_id = if stages.record {
            crate::runs::new_run_id(started_at)
        } else {
            "<run id>".to_string()
        };
        BindingRun {
            ctx: ctx.clone(),
            project,
            report,
            b,
            stages,
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
            parsed: &planned.parsed,
            query_failed: Arc::new(AtomicBool::new(false)),
            pool: None,
            renderers: BTreeMap::new(),
            started: Instant::now(),
            started_at,
            produced: Vec::new(),
            outs: b.outputs.iter().map(|_| OutputRun::default()).collect(),
            processes,
            drift: Vec::new(),
            protected_snapshot: None,
            retry: None,
            held: Vec::new(),
            zips: Default::default(),
        }
    }

    /// The Binding's connections, with sessions opened only when something needs the database,
    /// so compiling a report whose templates don't query it works without the plugin or
    /// credentials.
    fn open_pool(&mut self) -> Arc<Pool> {
        let connections = Arc::new(ProfileConnections::new(
            &self.ctx,
            self.project,
            self.ui.plugin_log(),
        ));
        let pool = Arc::new(Pool::new(
            connections,
            self.project.root().to_path_buf(),
            self.ui.plugin_log(),
            !self.report.managed,
        ));
        self.pool = Some(pool.clone());
        pool
    }

    /// The renderer for SQL on `connection` (`None`: the inherited connection, for output paths
    /// and template values), made on first use.
    pub(super) fn renderer(&mut self, connection: Option<&str>) -> Result<Arc<Renderer>, Fail> {
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
        let context = self.run_context();
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
                failed: self.query_failed.clone(),
            })),
            connections: Some(pool.connections.clone()),
            mode: RenderMode::Run,
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

    /// The `run.*` context this Binding renders with.
    pub(super) fn run_context(&self) -> crate::render::RunContext {
        let inputs = super::plan::inputs(&self.ctx, self.project, self.opts, self.started_at);
        crate::parse::run_context(self.project, self.report, self.b, &inputs, self.started_at)
    }

    /// Each query's connection, from the parse pass.
    pub(super) fn connection_of(&self, query: &str) -> Result<String, Fail> {
        self.parsed
            .query(query)
            .and_then(|q| q.connection.clone())
            .ok_or_else(|| format!("query `{query}` has no connection").into())
    }

    /// `report` or `report (Set s)`, for messages.
    pub(super) fn label(&self) -> String {
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
    pub(super) fn not_cancelled(&self) -> Result<(), Fail> {
        match self.opts.cancel.reason() {
            None => Ok(()),
            Some(CancelReason::Timeout) => Err(Fail::new(Code::RunTimedOut, "the run timed out")),
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

    /// Run the Binding: its stages, inside its lock and run folder when the run records.
    pub(super) fn run(&mut self) -> BindingOutcome {
        // A real run holds the Binding's lock: the same Binding never runs twice at once.
        let guard = if self.stages.record {
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
        } else {
            None
        };
        if let Some(g) = &guard {
            self.open_run_folder(g.took_over.as_ref());
        }
        let result = self.run_stages();
        let result = self.stopped_by_cancel(result);
        let status = match (&result, &self.stages.source) {
            (Err(e), _) if e.code == Code::RunCancelled => Status::Cancelled,
            (Err(e), _) if e.code == Code::RunTimedOut => Status::TimedOut,
            (Err(_), _) => Status::Error,
            (Ok(()), Source::Check) => Status::Checked,
            (Ok(()), Source::Compile) => Status::DryRun,
            (Ok(()), _) => Status::Success,
        };
        let err = result.err();
        if self.stages.record
            && let Err(e) = self.write_results(&status, err.as_ref())
        {
            self.ui.warn(&format!("can't write run_results.json: {e}"));
        }
        if guard.is_some() {
            self.close_run_folder();
        }
        // Spools are scratch space.
        let _ = std::fs::remove_dir_all(self.run_dir.join(".spool"));
        self.outcome(status, err)
    }

    /// A Binding that fails after the run was cancelled was stopped by it (a plugin's cancelled
    /// reply, or a check between steps).
    fn stopped_by_cancel(&self, result: Result<(), Fail>) -> Result<(), Fail> {
        let stopped =
            matches!(&result, Err(e) if e.code == Code::RunCancelled || e.code == Code::RunTimedOut);
        match (result, self.opts.cancel.reason()) {
            (Err(e), Some(reason)) if !stopped => {
                let (code, what) = match reason {
                    CancelReason::Timeout => (Code::RunTimedOut, "timed out"),
                    _ => (Code::RunCancelled, "was cancelled"),
                };
                Err(Fail::new(code, format!("the run {what}: {}", e.message)))
            }
            (r, _) => r,
        }
    }

    /// With the lock taken: say whose lock was taken over, move a run from before run folders,
    /// decide whether this run becomes the current one, and start its log.
    fn open_run_folder(&mut self, took_over: Option<&crate::runs::Holder>) {
        if let Some(h) = took_over {
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
        self.becomes_current = !self.older_than_current() && !matches!(self.stages.source, Source::Retry(_));
        // The run's own log, in its folder.
        if std::fs::create_dir_all(&self.run_dir).is_ok() {
            self.ui.emit(RunEvent::BindingLog(self.run_dir.join("dre.log")));
        }
    }

    /// Make this run the current one (unless a later one is), and prune old runs.
    fn close_run_folder(&mut self) {
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

    /// The stages: render and query (or replay, compile or check), then format, deliver and
    /// record.
    fn run_stages(&mut self) -> Result<(), Fail> {
        self.check_inputs()?;
        let pool = self.open_pool();
        if !self.report.managed {
            self.ui.warn(&format!(
                "`{}` is an unmanaged report ({}): for quick tests only — add a YAML to make it a managed report",
                self.report.name,
                self.report.file.display()
            ));
        }
        std::fs::create_dir_all(&self.compiled_dir).map_err(|e| e.to_string())?;
        // How many queries run on each connection: one session holds them all.
        let mut per_connection: BTreeMap<String, usize> = BTreeMap::new();
        for q in &self.b.queries {
            *per_connection.entry(self.connection_of(&q.query)?).or_default() += 1;
        }
        match self.stages.source.clone() {
            Source::Compile => return self.compile(&per_connection),
            Source::Check => return self.check(&per_connection),
            Source::Replay(dir) => self.replay(&dir)?,
            Source::Retry(id) => match self.start_retry(&id)? {
                Some(dir) => {
                    self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
                    self.replay_from(&dir, "the saved results")?;
                }
                None => self.query(&per_connection)?,
            },
            Source::Query => self.query(&per_connection)?,
        }
        pool.close_all();
        self.results()?;
        let (file_outs, message_outs) = self.make_files()?;
        self.check_drift()?;
        self.deliver_all(file_outs, message_outs)?;
        self.record_schema();
        Ok(())
    }

    /// The run's values for the report's declared vars, and the parse pass's problems.
    fn check_inputs(&self) -> Result<(), Fail> {
        let errors = crate::variables::check_run_values(&self.report.var_schema, &self.vars, &self.opts.vars);
        if !errors.is_empty() {
            return Err(errors.join("; ").into());
        }
        if self.parsed.errors.is_empty() {
            return Ok(());
        }
        let msgs: Vec<String> = self
            .parsed
            .errors
            .iter()
            .map(|p| match p.line {
                Some(l) => format!("{}:{l}: {}", p.file.display(), p.message),
                None => format!("{}: {}", p.file.display(), p.message),
            })
            .collect();
        Err(Fail::new(Code::ParseFailed, msgs.join("\n    ")))
    }

    /// Deliver the file outputs, then render and deliver each message, so a message can link to
    /// what was delivered. A failed output doesn't stop the next one.
    fn deliver_all(&mut self, file_outs: Vec<usize>, message_outs: Vec<usize>) -> Result<(), Fail> {
        let mut failures = Vec::new();
        for oi in file_outs.into_iter().chain(message_outs) {
            let per_row = self.b.outputs[oi].for_each.is_some();
            if let Err(e) = self.deliver_output_stage(oi) {
                // Rows stop at a cancel; anything else is recorded, and the next output goes on.
                if per_row && (e.code == Code::RunCancelled || e.code == Code::RunTimedOut) {
                    return Err(e);
                }
                failures.push(e);
            }
        }
        if matches!(self.stages.deliver, Deliver::Keep(_)) {
            let dir = rel(self.project.root(), &self.run_dir);
            self.ui.step(
                Level::Info,
                "Preview",
                &format!("not delivered; output stays in {}", dir.display()),
                None,
            );
        }
        match failures.first() {
            None => Ok(()),
            Some(first) => {
                let messages: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
                Err(Fail::new(first.code.clone(), messages.join("; ")))
            }
        }
    }

    /// Output `oi` through the deliver stage: its rows, its message, then its destinations.
    fn deliver_output_stage(&mut self, oi: usize) -> Result<(), Fail> {
        if self.outs[oi].status == Some(OutputStatus::Skipped) {
            return Ok(());
        }
        if self.b.outputs[oi].for_each.is_some() {
            return self.run_rows(oi).inspect_err(|e| {
                if e.code != Code::RunCancelled && e.code != Code::RunTimedOut {
                    self.outs[oi].status = Some(OutputStatus::Failed);
                    self.outs[oi].error.get_or_insert_with(|| e.message.clone());
                }
            });
        }
        if self.b.outputs[oi].is_message() {
            match self.render_message(oi) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(e) => {
                    let e = e.or(Code::RenderFailed);
                    let e = Fail::new(e.code.clone(), format!("{}: {e}", self.b.outputs[oi].label(oi)));
                    self.outs[oi].status = Some(OutputStatus::Failed);
                    self.outs[oi].error = Some(e.message.clone());
                    return Err(e);
                }
            }
        }
        match self.stages.deliver {
            Deliver::Keep(note) => {
                self.outs[oi].delivery_note = Some(note.into());
                self.outs[oi].status = Some(OutputStatus::Kept);
                Ok(())
            }
            Deliver::Send | Deliver::HoldAll => self.deliver(oi).map_err(|e| e.or(Code::DeliveryFailed)),
        }
    }

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
        } else if !notes.is_empty() && self.stages.preview.is_none() {
            parts.push(format!("({})", notes.join("; ")));
        }
        parts.join(" ")
    }

    /// Every file the Binding's outputs wrote, and where each was first delivered.
    pub(super) fn files(&self) -> impl Iterator<Item = &(PathBuf, Option<String>)> {
        self.outs.iter().flat_map(|o| o.files.iter())
    }
}
