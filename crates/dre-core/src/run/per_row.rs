//! Per-row outputs (`for_each`) in a run. The report's queries ran once; here the output is made
//! and delivered once per driver row, each copy with only its row's slice of the results and the
//! driver row as `each.*`. Each row's job is prepared (its Jinja rendered) on the run's thread,
//! then made and delivered by the run's [scheduler](super::scheduler) on `concurrency` workers,
//! each with its own format and destination plugin processes, reused from row to row. One failed
//! row is recorded and the rest carry on (or, with `on_row_error: fail`, nothing is delivered
//! unless every row rendered). Every row's result is a line of `<output>.rows.jsonl`, written as
//! it finishes.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arrow::array::RecordBatch;
use arrow::ipc::reader::FileReader;
use dre_protocol::host::{HostError, LogSink};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Map as JsonMap, Value as Json};

use super::mode::Deliver;
use super::scheduler::{Job, Scheduler};
use super::{BindingRun, Fail, OutputStatus, Produced, RowProgress, record_path, rel, thousands};
use crate::codes::Code;
use crate::context::Context;
use crate::dates::Calendar;
use crate::delivery::{Deliverer, Request};
use crate::engine::{CancelToken, Events, Level};
use crate::for_each::{Population, RowMask, Table as RowTable, collision, safe_name};
use crate::formats::{self, Files, Format};
use crate::message::QueryResult;
use crate::plugin_host::{Plugin, WriteRequest, find_plugin};
use crate::project::{ForEach, OrphanRows, Output, PluginKind, Project, RowErrors};
use crate::render::{RenderError, Renderer};
use crate::run_results::{FailedKey, ForEachResult, RowDelivery, RowFile, RowRecord};

/// `--retry-failed`: the run being redone and, per per-row output (by its stem), the keys
/// already done there or in an earlier retry (succeeded, skipped or held).
pub(super) struct Retry {
    pub original: String,
    pub done: HashMap<String, std::collections::HashSet<String>>,
}

/// A row that's finished: never made again by `--retry-failed`. A message skipped because its
/// attachment failed isn't: the retry may make the attachment.
fn finished(r: &RowRecord) -> bool {
    matches!(r.status.as_str(), "succeeded" | "skipped" | "held")
        && r.reason.as_deref() != Some("attachment_failed")
}

/// A destination entry of one row, rendered with its `each`.
#[derive(Clone)]
struct RowDest {
    /// Its place in the output's destinations (for pacing).
    idx: usize,
    profile: String,
    path: Option<String>,
    options: JsonMap<String, Json>,
    hold: bool,
    rate: Option<crate::rate::Rate>,
    /// Files to attach to a message: the same row's, or an output's shared files.
    attach: Vec<PathBuf>,
    /// An attached per-row output's row failed or was skipped: the message is skipped.
    attach_missing: bool,
    /// `zip:`, its password rendered with `each`.
    zip: Option<crate::zip_pack::ZipSpec>,
    /// Where the row's zip for this entry is built; delivered instead of the row's files.
    zip_file: Option<PathBuf>,
}

/// A run folder's saved query results, for `--retry-failed`.
pub(super) const SNAPSHOT_DIR: &str = "results";

/// An output a message attaches: its name, a per-row output's files by key, or its shared files.
type Attached = (String, Option<HashMap<String, Vec<PathBuf>>>, Vec<PathBuf>);

/// What a worker does with a row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Render, then deliver.
    All,
    /// Render only (`on_row_error: fail`, first pass).
    Render,
    /// Deliver what the first pass rendered.
    Deliver,
}

/// One row's work, prepared on the run's thread.
#[derive(Clone)]
struct RowJob {
    i: usize,
    key: String,
    mask: RowMask,
    /// Each file to make: its path and the data queries (indexes) in it.
    files: Vec<(PathBuf, Vec<usize>)>,
    /// A document's template context, with `each`.
    template: Option<Json>,
    /// The format options rendered with `each`, when any holds Jinja (a pdf `password:`).
    options: Option<JsonMap<String, Json>>,
    /// A message's title and text.
    message: Option<(String, String)>,
    dests: Vec<RowDest>,
    /// Decided before any work: a skipped or failed row.
    pre: Option<RowRecord>,
    phase: Phase,
}

/// A finished row: its job and its record.
type Done = (RowJob, RowRecord);

/// What every row of one per-row output shares while its jobs are prepared.
struct RowPlan<'p> {
    oi: usize,
    pop: &'p Population,
    /// The output's folder in the run folder.
    folder: PathBuf,
    /// One file per section (a single-table format with several data queries).
    splits: bool,
    /// Empty sections may be left out (the format's template doesn't decide them).
    droppable: bool,
    /// A document's or a template's payload, before `each`.
    template_base: Option<Json>,
    attached: Vec<Attached>,
    key_cols: Vec<String>,
}

/// One driver row to prepare: its index in the population, its file name and its `each`.
struct Row<'r> {
    i: usize,
    name: &'r str,
    each: &'r (JsonMap<String, Json>, minijinja::Value),
}

/// What every worker shares.
struct Shared<'a> {
    ctx: Context,
    project: &'a Project,
    pop: &'a Population,
    format: String,
    options: JsonMap<String, Json>,
    /// Each data query's result set description, by index.
    metas: Vec<ResultSetMeta>,
    is_message: bool,
    /// Deliver at all (not with `--no-deliver` or a preview).
    deliver: bool,
    /// `--hold`: hold every destination.
    hold_all: bool,
    log: LogSink,
    events: Events,
    cancel: CancelToken,
    pacer: crate::rate::Pacer,
    rows_file: Mutex<File>,
    progress: Progress,
}

/// Rows done so far, sent to the UI as each one finishes.
struct Progress {
    output: String,
    total: u64,
    rate: Option<crate::rate::Rate>,
    /// Done and failed.
    counts: Mutex<(u64, u64)>,
}

impl Progress {
    fn event(&self, finished: bool) -> RowProgress {
        let (done, failed) = *self.counts.lock().unwrap();
        RowProgress {
            output: self.output.clone(),
            done,
            failed,
            total: self.total,
            rate: self.rate,
            finished,
        }
    }

    fn row_done(&self, events: &Events, failed: bool) {
        {
            let mut c = self.counts.lock().unwrap();
            c.0 += 1;
            if failed {
                c.1 += 1;
            }
        }
        events.rows(self.event(false));
    }
}

/// A worker's own plugin processes, reused from row to row and closed when it's done.
struct Worker<'a> {
    format: Option<Box<dyn Plugin>>,
    deliverer: Deliverer<'a>,
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.format.take() {
            p.close();
        }
    }
}

impl BindingRun<'_> {
    /// Make and deliver per-row output `oi`: one copy per row of its driver.
    pub(super) fn run_rows(&mut self, oi: usize) -> Result<(), Fail> {
        let o = self.b.outputs[oi].clone();
        let fe = o.for_each.clone().expect("a per-row output");
        let label = o.label(oi);
        let pop = self.population(oi, &o, &fe)?;
        let (rows, population) = self.pick_rows(oi, &pop, &fe, &label);
        let stem = self.output_stem(oi);
        let sidecar = self.run_dir.join(format!("{stem}.rows.jsonl"));
        self.outs[oi].for_each = Some(ForEachResult {
            query: fe.query.clone(),
            key: fe.key.clone(),
            rows: rows.len() as u64,
            orphan_rows: pop.orphan_rows,
            partial: self.retry.is_some()
                || self.opts.rows.only_keys.is_some()
                || self.opts.rows.limit.is_some(),
            sidecar: record_path(self.project, &sidecar),
            ..ForEachResult::default()
        });
        if rows.is_empty() {
            return self.no_rows(oi, &pop, &fe, &label, &sidecar);
        }
        let (names, eaches) = self.row_names(&o, &pop, &rows, &label)?;
        let plan = self.row_plan(oi, &o, &pop, &fe)?;
        std::fs::create_dir_all(&plan.folder).map_err(|e| e.to_string())?;
        let rows_file =
            File::create(&sidecar).map_err(|e| format!("can't write {}: {e}", sidecar.display()))?;
        let concurrency = self
            .opts
            .rows
            .concurrency
            .or(fe.concurrency)
            .unwrap_or(crate::project::DEFAULT_ROW_CONCURRENCY)
            .clamp(1, 64);
        let shared = self.row_shared(&o, &pop, &plan, &label, rows.len(), rows_file);
        let started = Instant::now();
        self.ui.step(
            Level::Info,
            "Rows",
            &format!(
                "{label}: {} row(s) of `{}`, {concurrency} at a time",
                rows.len(),
                fe.query
            ),
            None,
        );
        shared.events.rows(shared.progress.event(false));
        let scheduler = Scheduler::new(concurrency).queue(concurrency * 2);
        let rows: Vec<Row<'_>> = rows
            .iter()
            .zip(&names)
            .zip(&eaches)
            .map(|((&i, name), each)| Row { i, name, each })
            .collect();
        let records = self.make_rows(
            &plan,
            &shared,
            &scheduler,
            &rows,
            fe.on_row_error == RowErrors::Fail,
        )?;
        shared.events.rows(shared.progress.event(true));
        self.finish_rows(oi, &records, &label, started);
        if self.stages.preview.is_some() {
            self.preview_rows(oi, &records, population);
        }
        if self.retry.is_some() {
            self.record_retried(&stem, &records)?;
        }
        self.rows_outcome(oi, shared.deliver, &label, &sidecar)
    }

    /// The population: the driver and each query the output makes a section of, split by key.
    fn population(&mut self, oi: usize, o: &Output, fe: &ForEach) -> Result<Population, Fail> {
        let label = o.label(oi);
        let failed = |m: String| Fail::new(Code::ForEachFailed, format!("{label}: {m}"));
        let driver_p = self
            .produced
            .iter()
            .position(|p| p.query == fe.query)
            .ok_or_else(|| failed(format!("its driver `{}` made no result", fe.query)))?;
        let data_ps = self.sections(oi, o);
        let total: u64 = std::iter::once(driver_p)
            .chain(data_ps.iter().copied())
            .map(|i| self.produced[i].rows)
            .sum();
        if total > fe.max_population_rows {
            return Err(failed(format!(
                "its queries return {} rows, more than `for_each.max_population_rows` ({}); the whole population is held in memory to split it, so filter or aggregate in SQL, or raise `max_population_rows`",
                thousands(total),
                thousands(fe.max_population_rows)
            )));
        }
        let driver = read_table(&self.produced[driver_p])?;
        let data: Vec<RowTable> = data_ps
            .iter()
            .map(|&i| read_table(&self.produced[i]))
            .collect::<Result<_, _>>()?;
        let pop = Population::build(driver, data, &fe.key).map_err(failed)?;
        if pop.orphan_rows > 0 {
            let msg = format!(
                "{} row(s) of its queries have a key that isn't in the driver `{}`, so they're in no copy (keys: {}{})",
                thousands(pop.orphan_rows),
                fe.query,
                pop.orphan_samples.join(", "),
                if pop.orphan_keys as usize > pop.orphan_samples.len() {
                    ", ..."
                } else {
                    ""
                }
            );
            match fe.on_orphan_rows {
                OrphanRows::Warn => self.ui.warn(&format!("  {label}: {msg}")),
                OrphanRows::Fail => return Err(failed(format!("{msg} (`on_orphan_rows: fail`)"))),
            }
        }
        Ok(pop)
    }

    /// The results output `oi` makes a section of, by index, less those `on_empty` dropped.
    fn sections(&self, oi: usize, o: &Output) -> Vec<usize> {
        (0..self.produced.len())
            .filter(|&i| o.sections(&self.produced[i].query))
            .filter(|&i| !self.outs[oi].dropped.contains(&self.produced[i].query))
            .collect()
    }

    /// Which rows: all, less those a retry finished, `--only-keys`, then `--limit-rows` (or a
    /// preview's first three). Also how many there were before the limit.
    fn pick_rows(&mut self, oi: usize, pop: &Population, fe: &ForEach, label: &str) -> (Vec<usize>, usize) {
        let stem = self.output_stem(oi);
        let done = self
            .retry
            .as_ref()
            .and_then(|r| r.done.get(&stem))
            .cloned()
            .unwrap_or_default();
        let only_keys = self.opts.rows.only_keys.as_ref();
        if let Some(keys) = only_keys {
            let missing: Vec<&str> = keys
                .iter()
                .filter(|k| !pop.keys.contains(k))
                .map(String::as_str)
                .collect();
            if !missing.is_empty() {
                self.ui.warn(&format!(
                    "  {label}: --only-keys names keys that aren't in the driver `{}`: {}",
                    fe.query,
                    missing.join(", ")
                ));
            }
        }
        let mut rows: Vec<usize> = (0..pop.len())
            .filter(|&i| !done.contains(&pop.keys[i]))
            .filter(|&i| only_keys.is_none_or(|k| k.contains(&pop.keys[i])))
            .collect();
        let population = rows.len();
        if let Some(n) = self.stages.sample_rows {
            rows.truncate(n);
        }
        (rows, population)
    }

    /// No row to make: an empty driver (`on_empty` has decided already), or nothing left to
    /// redo.
    fn no_rows(
        &mut self,
        oi: usize,
        pop: &Population,
        fe: &ForEach,
        label: &str,
        sidecar: &Path,
    ) -> Result<(), Fail> {
        let why = if pop.is_empty() {
            format!("the driver `{}` has no rows", fe.query)
        } else {
            "no row left to make".to_string()
        };
        self.ui
            .step(Level::Info, "No data", &format!("{label}: {why}"), None);
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        std::fs::write(sidecar, "").map_err(|e| e.to_string())?;
        self.outs[oi].status = Some(OutputStatus::Skipped);
        self.outs[oi].reason = Some("no_data".into());
        Ok(())
    }

    /// Each row's file name and `each`, checked for collisions before anything renders.
    #[allow(clippy::type_complexity)]
    fn row_names(
        &mut self,
        o: &Output,
        pop: &Population,
        rows: &[usize],
        label: &str,
    ) -> Result<(Vec<String>, Vec<(JsonMap<String, Json>, minijinja::Value)>), Fail> {
        let failed = |m: String| Fail::new(Code::ForEachFailed, format!("{label}: {m}"));
        let files = Files::of(o);
        let renderer = self.renderer(None)?;
        let mut eaches = Vec::with_capacity(rows.len());
        let mut names = Vec::with_capacity(rows.len());
        for &i in rows {
            let (each_json, each_value) = each_of(pop, i, self.calendar);
            let name = match &o.file_name {
                Some(src) => {
                    let ctx = minijinja::context! { each => each_value.clone() };
                    let n = renderer
                        .render_with(&self.report.file, src, ctx, false)
                        .map_err(|e| failed(format!("`file_name`: {}", e.message)))?
                        .trim()
                        .to_string();
                    if n.is_empty() || n.contains(['/', '\\']) || n == "." || n == ".." {
                        return Err(failed(format!(
                            "`file_name` rendered `{n}` for key `{}`; it must be a file name, without folders",
                            pop.keys[i]
                        )));
                    }
                    n
                }
                None => files.name(&safe_name(&pop.keys[i])),
            };
            names.push(name);
            eaches.push((each_json, each_value));
        }
        if let Some((file, a, b)) = collision(
            rows.iter()
                .map(|&i| pop.keys[i].as_str())
                .zip(names.iter().map(String::as_str)),
        ) {
            return Err(failed(format!(
                "the rows with keys `{a}` and `{b}` would both write `{file}`; make `file_name` (or the key) unique per row"
            )));
        }
        Ok((names, eaches))
    }

    /// What every row of output `oi` shares: its folder, its sections, its template, and the
    /// outputs a message attaches (a per-row output's row files by key, else shared files).
    fn row_plan<'p>(
        &mut self,
        oi: usize,
        o: &Output,
        pop: &'p Population,
        fe: &ForEach,
    ) -> Result<RowPlan<'p>, Fail> {
        let files = Files::of(o);
        let template_base = if files.format.is_document() {
            Some(self.document_payload(o.template.as_ref())?)
        } else if let Some(t) = &o.template {
            Some(self.template_payload(t)?)
        } else {
            None
        };
        let attached: Vec<Attached> = o
            .destinations
            .iter()
            .flat_map(|d| d.attach.iter())
            .filter_map(|name| {
                self.b
                    .outputs
                    .iter()
                    .position(|x| x.name.as_deref() == Some(name))
            })
            .map(|j| {
                let name = self.b.outputs[j].name.clone().unwrap_or_default();
                match &self.b.outputs[j].for_each {
                    Some(_) => (name, Some(self.outs[j].row_files.clone()), Vec::new()),
                    None => (
                        name,
                        None,
                        self.outs[j].files.iter().map(|(f, _)| f.clone()).collect(),
                    ),
                }
            })
            .collect();
        Ok(RowPlan {
            oi,
            pop,
            folder: self.run_dir.join(safe_name(&self.output_stem(oi))),
            splits: files.splits(self.sections(oi, o).len()),
            droppable: !files.format.template_decides_sections(),
            template_base,
            attached,
            key_cols: fe.key.clone(),
        })
    }

    /// What the row workers share for output `o`.
    fn row_shared<'s>(
        &self,
        o: &Output,
        pop: &'s Population,
        plan: &RowPlan<'_>,
        label: &str,
        total: usize,
        rows_file: File,
    ) -> Shared<'s>
    where
        Self: 's,
    {
        let hold_all = self.stages.deliver == Deliver::HoldAll;
        Shared {
            ctx: self.ctx.clone(),
            project: self.project,
            pop,
            format: o.format.clone(),
            options: o.options.clone(),
            metas: self
                .sections(plan.oi, o)
                .into_iter()
                .map(|i| self.produced[i].meta())
                .collect(),
            is_message: o.is_message(),
            deliver: !matches!(self.stages.deliver, Deliver::Keep(_)),
            hold_all,
            log: self.ui.plugin_log(),
            events: self.ui.clone(),
            cancel: self.opts.cancel.clone(),
            pacer: crate::rate::Pacer::default(),
            rows_file: Mutex::new(rows_file),
            progress: Progress {
                output: label.to_string(),
                total: total as u64,
                // The pace sends go at, when they go now (held ones are paced by `dre release`).
                rate: self.opts.rate.or_else(|| {
                    o.destinations
                        .iter()
                        .filter(|d| !(d.hold || hold_all))
                        .find_map(|d| d.rate)
                }),
                counts: Mutex::new((0, 0)),
            },
        }
    }

    /// Prepare each row's job (rendering its Jinja here, on the run's thread) and let the
    /// scheduler's workers make and deliver them. With `two_pass` (`on_row_error: fail`), every
    /// row renders first, and they deliver only if none failed.
    fn make_rows(
        &mut self,
        plan: &RowPlan<'_>,
        shared: &Shared<'_>,
        scheduler: &Scheduler,
        rows: &[Row<'_>],
        two_pass: bool,
    ) -> Result<Vec<Done>, Fail> {
        let first = if two_pass { Phase::Render } else { Phase::All };
        let jobs = rows
            .iter()
            .map(|row| self.prepare_row(plan, row, first).map(Job::free));
        let ran = run_jobs(scheduler, shared, jobs);
        let mut records: Vec<Done> = ran.done.into_iter().map(|(_, d)| d).collect();
        if let Some(e) = ran.stopped {
            return Err(e);
        }
        if !two_pass {
            return Ok(records);
        }
        if records.iter().any(|(_, r)| r.status == "failed") {
            // Deliver nothing: record every rendered row as not delivered.
            for (job, r) in records.iter_mut().filter(|(_, r)| r.status == "rendered") {
                r.status = "failed".into();
                r.error = Some(FailedKey {
                    key: job.key.clone(),
                    code: Code::RowsFailed.to_string(),
                    message:
                        "not delivered: another row failed and `on_row_error: fail` delivers all or nothing"
                            .into(),
                });
                write_record(shared, r);
                shared.progress.row_done(&shared.events, true);
            }
            return Ok(records);
        }
        // Rows the first pass skipped keep their record; the rendered ones deliver now.
        let (rendered, mut kept): (Vec<Done>, Vec<Done>) =
            records.into_iter().partition(|(_, r)| r.status == "rendered");
        let second = rendered.into_iter().map(|(mut job, _)| {
            job.phase = Phase::Deliver;
            Ok::<_, Fail>(Job::free(job))
        });
        kept.extend(
            run_jobs(scheduler, shared, second)
                .done
                .into_iter()
                .map(|(_, d)| d),
        );
        kept.sort_by_key(|(job, _)| job.i);
        Ok(kept)
    }

    /// The output's summary from its rows, what a message attaching it finds, its held
    /// deliveries, and its `Rows` line.
    fn finish_rows(&mut self, oi: usize, records: &[Done], label: &str, started: Instant) {
        let mut s = self.outs[oi].for_each.take().unwrap_or_default();
        for (job, r) in records {
            match r.status.as_str() {
                "succeeded" => s.succeeded += 1,
                "held" => s.held += 1,
                "skipped" => s.skipped += 1,
                _ => {
                    s.failed += 1;
                    if let Some(e) = &r.error {
                        s.failed_keys.push(e.clone());
                    }
                }
            }
            if matches!(r.status.as_str(), "succeeded" | "held") {
                self.outs[oi].row_files.insert(
                    job.key.clone(),
                    job.files.iter().map(|(f, _)| f.clone()).collect(),
                );
            }
        }
        s.not_started = s.rows - records.len() as u64;
        self.hold_rows(oi, records);
        self.ui
            .step(Level::Info, "Rows", &rows_line(label, &s, started), None);
        self.outs[oi].for_each = Some(s);
    }

    /// Held deliveries, for `dre release`: the rendered entry, unmasked, and the files'
    /// checksums.
    fn hold_rows(&mut self, oi: usize, records: &[Done]) {
        for (job, r) in records {
            for (d, rd) in job.dests.iter().zip(&r.deliveries) {
                if rd.status != "held" {
                    continue;
                }
                self.held.push(crate::release::HeldEntry {
                    id: format!("{}/{}/{}", self.output_stem(oi), job.key, d.idx),
                    output: self.b.outputs[oi].name.clone(),
                    key: Some(job.key.clone()),
                    profile: d.profile.clone(),
                    path: d.path.clone(),
                    options: d.options.clone(),
                    attach: match &d.zip_file {
                        Some(_) => Vec::new(),
                        None => d.attach.iter().map(|f| crate::release::held_file(f)).collect(),
                    },
                    rate: d.rate,
                    message: job.message.clone(),
                    files: match &d.zip_file {
                        Some(z) => vec![crate::release::held_file(z)],
                        None => job
                            .files
                            .iter()
                            .zip(&r.files)
                            .map(|((f, _), rf)| crate::release::HeldFile {
                                path: f.clone(),
                                sha256: rf.sha256.clone(),
                            })
                            .collect(),
                    },
                });
            }
        }
    }

    /// What's finished now, this retry included, for the next `--retry-failed`.
    fn record_retried(&mut self, stem: &str, records: &[Done]) -> Result<(), Fail> {
        let Some(r) = &self.retry else {
            return Ok(());
        };
        let mut all = r.done.get(stem).cloned().unwrap_or_default();
        all.extend(
            records
                .iter()
                .filter(|(_, r)| finished(r))
                .map(|(j, _)| j.key.clone()),
        );
        let path = self.run_dir.join(format!("{stem}.done.json"));
        let mut keys: Vec<&String> = all.iter().collect();
        keys.sort();
        std::fs::write(&path, serde_json::to_string(&keys).unwrap_or_default()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Output `oi`'s status from its rows; an error when any row failed.
    fn rows_outcome(&mut self, oi: usize, deliver: bool, label: &str, sidecar: &Path) -> Result<(), Fail> {
        let s = self.outs[oi].for_each.clone().unwrap_or_default();
        self.not_cancelled()?;
        let failed_n = s.failed;
        self.outs[oi].status = Some(if failed_n > 0 {
            OutputStatus::Failed
        } else if s.held > 0 && s.succeeded == 0 {
            OutputStatus::Held
        } else if deliver {
            OutputStatus::Delivered
        } else {
            OutputStatus::Kept
        });
        if let (Deliver::Keep(note), 0) = (self.stages.deliver, failed_n) {
            self.outs[oi].delivery_note = Some(note.into());
        }
        if failed_n == 0 {
            return Ok(());
        }
        let e = s.failed_keys.first().cloned().expect("a failed row has an error");
        let msg = format!(
            "{label}: {failed_n} row(s) failed (first: key `{}`: {}); see {}",
            e.key,
            e.message,
            record_path(self.project, sidecar).display()
        );
        self.outs[oi].error = Some(msg.clone());
        Err(Fail::new(Code::RowsFailed, msg))
    }

    /// `--preview` of a per-row output: each previewed row's files and rendered destinations
    /// (masked unless `--unmask`), and what a real run would send.
    fn preview_rows(&mut self, oi: usize, records: &[Done], population: usize) {
        let label = self.b.outputs[oi].label(oi);
        for (job, r) in records {
            let shown = |t: &str| {
                if self.opts.rows.unmask {
                    t.to_string()
                } else {
                    job.mask.apply(t)
                }
            };
            let files: Vec<String> = job
                .files
                .iter()
                .map(|(f, _)| rel(self.project.root(), f).display().to_string())
                .collect();
            let dests: Vec<String> = job
                .dests
                .iter()
                .map(|d| {
                    let target = d.path.clone().or_else(|| address(&d.options)).unwrap_or_default();
                    format!("{} → {}", d.profile, shown(&target))
                })
                .collect();
            self.ui.step(
                Level::Info,
                "Preview",
                &format!(
                    "{label}: row `{}` ({}): {}{}",
                    job.key,
                    r.status,
                    files.join(", "),
                    if dests.is_empty() {
                        String::new()
                    } else {
                        format!("; {}", dests.join("; "))
                    }
                ),
                None,
            );
        }
        let sends = records.first().map_or(0, |(j, _)| j.dests.len());
        self.ui.step(
            Level::Info,
            "Preview",
            &format!(
                "{label}: would make {} row(s){}; nothing was sent",
                thousands(population as u64),
                if sends > 0 {
                    format!(" and {} deliveries", thousands((population * sends) as u64))
                } else {
                    String::new()
                }
            ),
            None,
        );
    }

    /// `--retry-failed <id>`: find the run (following a retry to the run it redid), its saved
    /// results, and the rows already finished. `Ok(None)`: no saved results (`keep_results:
    /// false`), so the queries run again.
    pub(super) fn start_retry(&mut self, id: &str) -> Result<Option<PathBuf>, Fail> {
        let read = |run: &str| -> Option<Json> {
            let text = std::fs::read_to_string(self.runs.run_dir(run).join("run_results.json")).ok()?;
            serde_json::from_str(&text).ok()
        };
        let Some(results) = read(id) else {
            return Err(Fail::new(
                Code::RetryFailed,
                format!(
                    "--retry-failed {id}: {} has no run `{id}` in the target path (it may have been pruned: `keep_runs` keeps the newest runs; see `dre history`)",
                    self.label()
                ),
            ));
        };
        let original = results
            .get("retry_of")
            .and_then(Json::as_str)
            .unwrap_or(id)
            .to_string();
        if original != id && read(&original).is_none() {
            return Err(Fail::new(
                Code::RetryFailed,
                format!(
                    "--retry-failed {id}: it redid run `{original}`, which is no longer in the target path"
                ),
            ));
        }
        let done = self.rows_done(&original);
        self.retry = Some(Retry {
            original: original.clone(),
            done,
        });
        let snapshot = self.runs.run_dir(&original).join(SNAPSHOT_DIR);
        if snapshot.join(crate::saved::INDEX).is_file() {
            self.ui.step(
                Level::Info,
                "Retry",
                &format!(
                    "redoing the failed and never-started rows of run {original}, from its saved results"
                ),
                None,
            );
            return Ok(Some(snapshot));
        }
        if self
            .b
            .outputs
            .iter()
            .filter_map(|o| o.for_each.as_ref())
            .all(|f| !f.keep_results)
        {
            self.ui.warn(&format!(
                "  run {original} kept no results (`for_each.keep_results: false`), so the queries run again: the retried rows may show newer data than the rest"
            ));
            return Ok(None);
        }
        Err(Fail::new(
            Code::RetryFailed,
            format!(
                "--retry-failed {id}: run `{original}` has no saved results ({SNAPSHOT_DIR}/ in its folder is gone)"
            ),
        ))
    }

    /// The rows of each per-row output (by its stem) that run `original` and its earlier
    /// retries finished.
    fn rows_done(&self, original: &str) -> HashMap<String, std::collections::HashSet<String>> {
        let read = |run: &str| -> Option<Json> {
            let text = std::fs::read_to_string(self.runs.run_dir(run).join("run_results.json")).ok()?;
            serde_json::from_str(&text).ok()
        };
        let mut done: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        let original_dir = self.runs.run_dir(original);
        for (oi, o) in self.b.outputs.iter().enumerate() {
            if o.for_each.is_none() {
                continue;
            }
            let stem = self.output_stem(oi);
            let set = done.entry(stem.clone()).or_default();
            if let Ok(text) = std::fs::read_to_string(original_dir.join(format!("{stem}.rows.jsonl"))) {
                for line in text.lines() {
                    if let Ok(r) = serde_json::from_str::<RowRecord>(line)
                        && finished(&r)
                    {
                        set.insert(r.key);
                    }
                }
            }
            // Earlier retries of the same run.
            for run in self.runs.list() {
                if read(&run).is_some_and(|r| r.get("retry_of").and_then(Json::as_str) == Some(original))
                    && let Ok(text) =
                        std::fs::read_to_string(self.runs.run_dir(&run).join(format!("{stem}.done.json")))
                    && let Ok(keys) = serde_json::from_str::<Vec<String>>(&text)
                {
                    set.extend(keys);
                }
            }
        }
        done
    }

    /// The name a per-row output's folder and sidecar take: its `name:`, else the report's.
    fn output_stem(&self, oi: usize) -> String {
        self.b.outputs[oi]
            .name
            .clone()
            .unwrap_or_else(|| self.report.name.clone())
    }

    /// Prepare one row's job: its files, its slices' `on_empty`, its destinations and message
    /// rendered with `each`.
    fn prepare_row(&mut self, plan: &RowPlan<'_>, row: &Row<'_>, phase: Phase) -> Result<RowJob, Fail> {
        let o = &self.b.outputs[plan.oi];
        let key = plan.pop.keys[row.i].clone();
        let mask = if o
            .for_each
            .as_ref()
            .and_then(|f| f.mask_row_values)
            .unwrap_or(self.project.mask_row_values())
        {
            RowMask::new(&row.each.0, &plan.key_cols)
        } else {
            RowMask::default()
        };
        let mut job = RowJob {
            i: row.i,
            key,
            mask,
            files: Vec::new(),
            template: None,
            options: None,
            message: None,
            dests: Vec::new(),
            pre: None,
            phase,
        };
        let decision = self.row_decision(plan, row.i);
        if let Some(why) = decision.fail {
            job.pre = Some(pre_record(
                &job.key,
                "failed",
                "no_data",
                Some((Code::NoData, why)),
            ));
            return Ok(job);
        }
        if decision.skip.is_some() {
            job.pre = Some(pre_record(&job.key, "skipped", "no_data", None));
            return Ok(job);
        }
        let kept: Vec<usize> = (0..plan.pop.data.len())
            .filter(|&q| !decision.drop.contains(&plan.pop.data[q].0.name))
            .collect();
        job.files = row_files(plan, row.name, &kept)?;
        if let Some(Json::Object(base)) = &plan.template_base {
            let mut t = base.clone();
            if Format::new(&o.format).is_document() {
                t.insert("each".into(), Json::Object(row.each.0.clone()));
            }
            job.template = Some(Json::Object(t));
        }
        let ctx = self.row_context(plan, row, &kept, &mut job)?;
        self.row_options(plan.oi, &ctx, &mut job)?;
        for idx in 0..o.destinations.len() {
            let d = self.row_destination(plan, idx, &ctx, &job)?;
            job.dests.push(d);
        }
        if job.dests.iter().any(|d| d.attach_missing) {
            job.pre = Some(pre_record(&job.key, "skipped", "attachment_failed", None));
        }
        Ok(job)
    }

    /// Row `i`'s slices and their `on_empty`.
    fn row_decision(&self, plan: &RowPlan<'_>, i: usize) -> crate::no_data::Decision {
        let o = &self.b.outputs[plan.oi];
        let entries: Vec<crate::no_data::Query<'_>> = plan
            .pop
            .data
            .iter()
            .enumerate()
            .map(|(q, (t, _))| crate::no_data::Query {
                name: &t.name,
                rows: plan.pop.slice_len(i, q),
                on_empty: self
                    .b
                    .queries
                    .iter()
                    .find(|x| x.query == t.name)
                    .and_then(|x| x.on_empty),
            })
            .collect();
        if entries.is_empty() {
            crate::no_data::Decision::default()
        } else {
            crate::no_data::decide(o.on_empty, &entries, plan.droppable)
        }
    }

    /// What the row's Jinja sees: `each`, and for a message its row's `results` and `outputs`;
    /// a message's title and text are rendered here, into `job`.
    fn row_context(
        &mut self,
        plan: &RowPlan<'_>,
        row: &Row<'_>,
        kept: &[usize],
        job: &mut RowJob,
    ) -> Result<minijinja::Value, Fail> {
        let o = &self.b.outputs[plan.oi];
        let mut ctx_map: BTreeMap<String, minijinja::Value> = BTreeMap::new();
        ctx_map.insert("each".into(), row.each.1.clone());
        if !o.is_message() {
            return Ok(minijinja::Value::from(ctx_map));
        }
        let slices = plan.pop.slices(row.i)?;
        let results: Vec<(String, Arc<QueryResult>)> = kept
            .iter()
            .map(|&q| {
                let t = &plan.pop.data[q].0;
                let cols = t
                    .batch
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.name().clone())
                    .collect();
                (
                    t.name.clone(),
                    Arc::new(crate::document::query_result(cols, &slices[q..=q], self.calendar)),
                )
            })
            .collect();
        let base = self.template_context(plan.oi, &results);
        for k in ["results", "outputs"] {
            if let Ok(v) = base.get_attr(k) {
                ctx_map.insert(k.into(), v);
            }
        }
        let message = self.row_message(
            plan.oi,
            &results,
            &minijinja::Value::from(ctx_map.clone()),
            &job.mask,
        )?;
        job.message = Some(message);
        let (stem, _) = formats::split_ext(row.name);
        let md = Format::new(formats::MESSAGE).extension();
        job.files = vec![(plan.folder.join(format!("{stem}.{md}")), Vec::new())];
        Ok(minijinja::Value::from(ctx_map))
    }

    /// A per-row message's title and text, rendered with the row's context.
    fn row_message(
        &mut self,
        oi: usize,
        results: &[(String, Arc<QueryResult>)],
        ctx: &minijinja::Value,
        mask: &RowMask,
    ) -> Result<(String, String), Fail> {
        let renderer = self.renderer(None)?;
        let o = &self.b.outputs[oi];
        let opt = |k: &str| o.options.get(k).and_then(Json::as_str).map(str::to_string);
        let text = match (opt("text"), opt("file")) {
            (Some(t), _) => renderer.render_with(&self.report.file, &t, ctx.clone(), true),
            (None, Some(f)) => {
                let path = crate::project::find_template(self.project.root(), &f)
                    .ok_or_else(|| format!("message file `{f}` doesn't exist"))?;
                let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                renderer.render_with(&rel(self.project.root(), &path), &src, ctx.clone(), true)
            }
            (None, None) => Ok(crate::message::default_text(results, self.locale())),
        }
        .map_err(|e| Fail::new(Code::RenderFailed, mask.apply(&e.message)))?;
        let title = match opt("title") {
            Some(t) => renderer
                .render_with(&self.report.file, &t, ctx.clone(), false)
                .map_err(|e| Fail::new(Code::RenderFailed, mask.apply(&e.message)))?,
            None => format!("{}: {}", self.report.name, self.date),
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        Ok((title, text.trim().to_string()))
    }

    /// A file output's format options rendered with the row's context, when any holds Jinja.
    fn row_options(&mut self, oi: usize, ctx: &minijinja::Value, job: &mut RowJob) -> Result<(), Fail> {
        let o = &self.b.outputs[oi];
        if o.is_message() || !o.options.values().any(has_template) {
            return Ok(());
        }
        let renderer = self.renderer(None)?;
        let rendered = render_json_with(
            &renderer,
            &self.report.file,
            &Json::Object(o.options.clone()),
            ctx,
        )
        .map_err(|e| {
            Fail::new(
                Code::RenderFailed,
                format!("format option: {}", job.mask.apply(&e.message)),
            )
        })?;
        if let Json::Object(m) = rendered {
            job.options = Some(m);
        }
        Ok(())
    }

    /// Destination entry `idx` of the row's output, rendered with the row's context.
    fn row_destination(
        &mut self,
        plan: &RowPlan<'_>,
        idx: usize,
        ctx: &minijinja::Value,
        job: &RowJob,
    ) -> Result<RowDest, Fail> {
        let d = &self.b.outputs[plan.oi].destinations[idx];
        let renderer = self.renderer(None)?;
        let mask = &job.mask;
        let file = &self.report.file;
        let profile = renderer
            .render_with(file, &d.profile, ctx.clone(), false)
            .map_err(|e| {
                Fail::new(
                    Code::RenderFailed,
                    format!("destination profile: {}", mask.apply(&e.message)),
                )
            })?;
        let connections = self.pool.as_ref().expect("pool").connections.clone();
        renderer.set_destination(
            crate::render::Connections::profile(&*connections, &profile, Some("destination")).ok(),
        );
        let rendered = (|| -> Result<RowDest, Fail> {
            let path = match &d.path {
                Some(p) => Some(
                    renderer
                        .render_with(file, p, ctx.clone(), false)
                        .map_err(|e| format!("output path: {}", mask.apply(&e.message)))?,
                ),
                None => None,
            };
            let mut options = JsonMap::new();
            for (k, v) in &d.options {
                let v = render_json_with(&renderer, file, v, ctx).map_err(|e| {
                    format!("destination `{profile}` option `{k}`: {}", mask.apply(&e.message))
                })?;
                options.insert(k.clone(), v);
            }
            let (attach, attach_missing) = row_attachments(&plan.attached, &d.attach, &job.key);
            let zip = match &d.zip {
                Some(z) => Some(crate::zip_pack::ZipSpec {
                    password: match &z.password {
                        Some(pw) => Some(
                            renderer
                                .render_with(file, pw, ctx.clone(), false)
                                .map_err(|e| format!("`zip.password`: {}", mask.apply(&e.message)))?,
                        ),
                        None => None,
                    },
                    encryption: z.encryption,
                }),
                None => None,
            };
            let mut dest = RowDest {
                idx,
                profile: profile.clone(),
                path,
                options,
                hold: d.hold,
                rate: self.opts.rate.or(d.rate),
                attach,
                attach_missing,
                zip,
                zip_file: None,
            };
            dest.zip_file = zip_file(plan, job, &dest)?;
            Ok(dest)
        })();
        renderer.set_destination(None);
        rendered
    }
}

/// A row decided before any work: skipped or failed, with why.
fn pre_record(key: &str, status: &str, reason: &str, error: Option<(Code, String)>) -> RowRecord {
    RowRecord {
        key: key.to_string(),
        status: status.into(),
        reason: Some(reason.into()),
        files: Vec::new(),
        deliveries: Vec::new(),
        error: error.map(|(code, message)| FailedKey {
            key: key.to_string(),
            code: code.to_string(),
            message,
        }),
    }
}

/// The row's files: one per kept section when the format splits them, else one.
fn row_files(plan: &RowPlan<'_>, name: &str, kept: &[usize]) -> Result<Vec<(PathBuf, Vec<usize>)>, Fail> {
    if !plan.splits {
        return Ok(vec![(plan.folder.join(name), kept.to_vec())]);
    }
    let tabs: Vec<&str> = kept.iter().map(|&q| plan.pop.data[q].0.name.as_str()).collect();
    Ok(Files::split_names(name, &tabs)?
        .into_iter()
        .zip(kept)
        .map(|(n, &q)| (plan.folder.join(n), vec![q]))
        .collect())
}

/// The files a row's message attaches: the same row's of a per-row output (missing when that
/// row didn't make them), or another output's shared files.
fn row_attachments(attached: &[Attached], names: &[String], key: &str) -> (Vec<PathBuf>, bool) {
    let mut attach = Vec::new();
    let mut missing = false;
    for name in names {
        if let Some((_, per_row, shared)) = attached.iter().find(|(n, _, _)| n == name) {
            match per_row {
                Some(rows) => match rows.get(key) {
                    Some(files) => attach.extend(files.iter().cloned()),
                    None => missing = true,
                },
                None => attach.extend(shared.iter().cloned()),
            }
        }
    }
    (attach, missing)
}

/// Where a zipping entry's zip goes: entries zipping the same files the same way share one.
fn zip_file(plan: &RowPlan<'_>, job: &RowJob, d: &RowDest) -> Result<Option<PathBuf>, Fail> {
    if d.zip.is_none() {
        return Ok(None);
    }
    let stem = job
        .files
        .first()
        .and_then(|(f, _)| f.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| job.key.clone());
    let name = crate::zip_pack::zip_name(
        d.path
            .as_deref()
            .or(d.options.get("attachment_name").and_then(Json::as_str)),
        &stem,
    )
    .map_err(|e| format!("destination `{}`: {e}", d.profile))?;
    let shared = job.dests.iter().find(|x| {
        x.zip == d.zip && x.attach == d.attach && x.zip_file.as_ref().is_some_and(|f| f.ends_with(&name))
    });
    Ok(Some(match shared.and_then(|x| x.zip_file.clone()) {
        Some(f) => f,
        None => plan
            .folder
            .join("zips")
            .join(format!("{}-{}", stem, d.idx))
            .join(&name),
    }))
}

/// A per-row output's `Rows` line: how its rows went.
fn rows_line(label: &str, s: &ForEachResult, started: Instant) -> String {
    let line = format!(
        "{label}: {} succeeded, {} skipped, {} failed{}{} in {:.1}s",
        s.succeeded,
        s.skipped,
        s.failed,
        if s.held > 0 {
            format!(", {} held", s.held)
        } else {
            String::new()
        },
        if s.not_started > 0 {
            format!(", {} not started", s.not_started)
        } else {
            String::new()
        },
        started.elapsed().as_secs_f64()
    );
    if s.failed == 0 {
        return line;
    }
    let keys: Vec<&str> = s.failed_keys.iter().take(10).map(|k| k.key.as_str()).collect();
    let more = s.failed_keys.len().saturating_sub(10);
    format!(
        "{line}; failed keys: {}{}",
        keys.join(", "),
        if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        }
    )
}

/// A spooled result, read whole: the population is split in memory.
fn read_table(p: &Produced) -> Result<RowTable, String> {
    let reader = FileReader::try_new(File::open(&p.spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    RowTable::new(&p.query, &p.schema, &batches)
}

/// Row `i` of the driver: as JSON (for a document's context) and as a template value (dates
/// stay DRE dates).
fn each_of(pop: &Population, i: usize, cal: Calendar) -> (JsonMap<String, Json>, minijinja::Value) {
    let row = pop.driver_row(i);
    let names: Vec<String> = row.schema().fields().iter().map(|f| f.name().clone()).collect();
    let typed = crate::values::typed_rows(&row, cal).pop().unwrap_or_default();
    let json: JsonMap<String, Json> = names
        .iter()
        .zip(&typed)
        .map(|(n, v)| (n.clone(), serde_json::to_value(v).unwrap_or(Json::Null)))
        .collect();
    let value: BTreeMap<String, minijinja::Value> = names.into_iter().zip(typed).collect();
    (json, minijinja::Value::from(value))
}

/// Whether a JSON value holds Jinja at any depth.
pub(crate) fn has_template(v: &Json) -> bool {
    match v {
        Json::String(s) => crate::preflight::is_templated(s),
        Json::Array(a) => a.iter().any(has_template),
        Json::Object(o) => o.values().any(has_template),
        _ => false,
    }
}

fn render_json_with(
    renderer: &Renderer,
    file: &Path,
    v: &Json,
    ctx: &minijinja::Value,
) -> Result<Json, RenderError> {
    Ok(match v {
        Json::String(s) if crate::preflight::is_templated(s) => {
            Json::String(renderer.render_with(file, s, ctx.clone(), false)?)
        }
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| render_json_with(renderer, file, v, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), render_json_with(renderer, file, v, ctx)?)))
                .collect::<Result<_, RenderError>>()?,
        ),
        other => other.clone(),
    })
}

fn write_record(shared: &Shared<'_>, r: &RowRecord) {
    if let Ok(line) = serde_json::to_string(r) {
        let mut f = shared.rows_file.lock().unwrap();
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

/// Run row jobs on `scheduler`'s workers, each with its own plugin processes; a row the run
/// stopped part-way isn't recorded: it never finished, so a retry makes it.
fn run_jobs<E>(
    scheduler: &Scheduler,
    shared: &Shared<'_>,
    jobs: impl IntoIterator<Item = Result<Job<RowJob>, E>>,
) -> super::scheduler::Ran<Done, E> {
    scheduler.run(
        jobs,
        &shared.cancel,
        || Worker {
            format: None,
            deliverer: Deliverer::new(&shared.ctx, shared.project, shared.log.clone()),
        },
        |w, _, job| {
            let record = run_job(shared, &job, w)?;
            report_row(shared, &job, &record);
            Some((job, record))
        },
    )
}

/// Record a finished row in the sidecar and the progress, and say how it went.
fn report_row(shared: &Shared<'_>, job: &RowJob, record: &RowRecord) {
    if record.status != "rendered" {
        write_record(shared, record);
        shared
            .progress
            .row_done(&shared.events, record.status == "failed");
    }
    if record.status == "failed" {
        let msg = record.error.as_ref().map(|e| e.message.as_str()).unwrap_or("");
        shared.events.warn(&format!("  row `{}` failed: {msg}", job.key));
    } else {
        shared.events.step(
            Level::Debug,
            "Row",
            &format!("`{}`: {}", job.key, record.status),
            None,
        );
    }
}

fn row_failed(
    job: &RowJob,
    code: &str,
    message: String,
    files: Vec<RowFile>,
    deliveries: Vec<RowDelivery>,
) -> RowRecord {
    RowRecord {
        key: job.key.clone(),
        status: "failed".into(),
        reason: None,
        files,
        deliveries,
        error: Some(FailedKey {
            key: job.key.clone(),
            code: code.into(),
            message: job.mask.apply(&message),
        }),
    }
}

/// Make the row's files and zips (unless the first pass did), then deliver them; `None` when
/// the run stopped part-way.
fn run_job(shared: &Shared<'_>, job: &RowJob, w: &mut Worker<'_>) -> Option<RowRecord> {
    if let Some(pre) = &job.pre {
        return Some(pre.clone());
    }
    if job.phase != Phase::Deliver {
        if let Err(e) = make_files(shared, job, &mut w.format) {
            // A plugin that failed may be broken: the next row starts a fresh one.
            if let Some(p) = w.format.take() {
                p.close();
            }
            return Some(row_failed(
                job,
                &e.code.to_string(),
                e.message,
                Vec::new(),
                Vec::new(),
            ));
        }
        if let Err(e) = make_zips(job) {
            return Some(row_failed(
                job,
                &Code::DeliveryFailed.to_string(),
                job.mask.apply(&e),
                Vec::new(),
                Vec::new(),
            ));
        }
    }
    let files: Vec<RowFile> = job
        .files
        .iter()
        .map(|(f, _)| RowFile {
            path: record_path(shared.project, f),
            sha256: crate::checksum::sha256_file(f).unwrap_or_default(),
        })
        .collect();
    if job.phase == Phase::Render {
        return Some(RowRecord {
            key: job.key.clone(),
            status: "rendered".into(),
            reason: None,
            files,
            deliveries: Vec::new(),
            error: None,
        });
    }
    deliver_row(shared, job, &mut w.deliverer, files)
}

/// Deliver a made row to each of its destinations (or hold, or keep it); `None` when the run
/// stopped part-way.
fn deliver_row(
    shared: &Shared<'_>,
    job: &RowJob,
    deliverer: &mut Deliverer<'_>,
    files: Vec<RowFile>,
) -> Option<RowRecord> {
    let mut deliveries = Vec::new();
    let mut first_error: Option<Fail> = None;
    let mut held = false;
    for d in &job.dests {
        let target = d
            .path
            .clone()
            .or_else(|| address(&d.options))
            .map(|t| job.mask.apply(&t));
        let record = |status: &str| RowDelivery {
            profile: d.profile.clone(),
            target: target.clone(),
            status: status.into(),
            location: None,
            error: None,
        };
        if d.hold || shared.hold_all {
            held = true;
            deliveries.push(record("held"));
            continue;
        }
        if !shared.deliver {
            deliveries.push(record("kept"));
            continue;
        }
        if shared.cancel.is_cancelled() {
            return None;
        }
        if let Some(rate) = &d.rate
            && !shared.pacer.wait(&d.idx.to_string(), rate, &shared.cancel)
        {
            return None;
        }
        let local: Vec<PathBuf> = match &d.zip_file {
            Some(z) => vec![z.clone()],
            None => job.files.iter().map(|(f, _)| f.clone()).collect(),
        };
        let request = Request::new(&d.profile, d.path.as_deref(), &d.options, &local)
            .message(job.message.as_ref(), &d.attach)
            .format(&shared.format);
        let mut r = record("delivered");
        match deliverer.deliver(request) {
            Ok(Some(sent)) => r.location = Some(job.mask.apply(&sent.location)),
            Ok(None) => r.status = "not_delivered".into(),
            Err(e) => {
                let msg = job.mask.apply(&e.message);
                first_error.get_or_insert(Fail::new(e.code.clone(), msg.clone()));
                r.status = "failed".into();
                r.error = Some(msg);
            }
        }
        deliveries.push(r);
    }
    Some(match first_error {
        Some(e) => row_failed(job, &e.code.to_string(), e.message, files, deliveries),
        None => RowRecord {
            key: job.key.clone(),
            status: if held { "held" } else { "succeeded" }.into(),
            reason: None,
            files,
            deliveries,
            error: None,
        },
    })
}

/// The address a pathless destination sends to, for the record: `to`, `channel`, `webhook`...
fn address(options: &JsonMap<String, Json>) -> Option<String> {
    ["to", "channel", "recipient", "space"]
        .iter()
        .find_map(|k| options.get(*k))
        .map(|v| match v {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        })
}

/// Write a row's files through the format plugin (or a message's `.md`).
fn make_files(shared: &Shared<'_>, job: &RowJob, format: &mut Option<Box<dyn Plugin>>) -> Result<(), Fail> {
    if shared.is_message {
        let (title, text) = job.message.clone().unwrap_or_default();
        let (path, _) = job.files.first().ok_or("the message has no file to write")?;
        std::fs::write(path, crate::message::file_body(&title, &text)).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let slices = shared.pop.slices(job.i)?;
    if format.is_none() {
        let plugin = find_plugin(&shared.ctx, shared.project, PluginKind::Format, &shared.format)
            .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?;
        *format = Some(
            shared
                .ctx
                .host()
                .start(&plugin, shared.log.clone(), Some(&shared.project.root()))
                .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?,
        );
    }
    let p = format.as_deref_mut().expect("started");
    let fmt_err =
        |e: HostError| Fail::from_plugin(&e, format!("{} format: {e}", shared.format)).or(Code::FormatFailed);
    for (path, qs) in &job.files {
        let metas: Vec<ResultSetMeta> = qs.iter().map(|&q| shared.metas[q].clone()).collect();
        p.write(WriteRequest {
            path: path.to_string_lossy().into_owned(),
            format: shared.format.clone(),
            options: job.options.clone().unwrap_or_else(|| shared.options.clone()),
            result_sets: metas,
            template: job.template.clone(),
        })
        .map_err(fmt_err)?;
        for &q in qs {
            let b = &slices[q];
            crate::plugin_host::write_result_set(p, &b.schema(), vec![b.clone()]).map_err(fmt_err)?;
        }
        let warnings = p.finish().map_err(fmt_err)?.warnings;
        for w in warnings {
            shared.events.warn(&format!(
                "  row `{}`: {} format: {}",
                job.key,
                shared.format,
                job.mask.apply(&w)
            ));
        }
    }
    Ok(())
}

/// Build the row's zips: its files and the entry's attached files, once per zip.
fn make_zips(job: &RowJob) -> Result<(), String> {
    let mut built: Vec<&PathBuf> = Vec::new();
    for d in &job.dests {
        let (Some(z), Some(to)) = (&d.zip, &d.zip_file) else {
            continue;
        };
        if built.contains(&to) {
            continue;
        }
        let mut files: Vec<PathBuf> = job.files.iter().map(|(f, _)| f.clone()).collect();
        files.extend(d.attach.iter().cloned());
        crate::zip_pack::build(to, &files, z.password.as_deref(), z.encryption)
            .map_err(|e| format!("destination `{}`: {e}", d.profile))?;
        built.push(to);
    }
    Ok(())
}
