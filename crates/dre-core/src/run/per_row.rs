//! Per-row outputs (`for_each`) in a run. The report's queries ran once; here the output is made
//! and delivered once per driver row, each copy with only its row's slice of the results and the
//! driver row as `each.*`. Rows render and deliver on `concurrency` workers, each with its own
//! format and destination plugin processes, reused from row to row. One failed row is recorded
//! and the rest carry on (or, with `on_row_error: fail`, nothing is delivered unless every row
//! rendered). Every row's result is a line of `<output>.rows.jsonl`, written as it finishes.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::mpsc;

use sha2::Digest;

use super::*;
use crate::for_each::{Population, RowMask, Table as RowTable, collision, safe_name};
use crate::project::{OrphanRows, RowErrors};
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

/// A destination entry of one row, rendered with its `each` (or a held delivery's, for
/// `dre release`).
#[derive(Clone)]
pub(crate) struct RowDest {
    /// Its place in the output's destinations (for pacing).
    pub idx: usize,
    pub profile: String,
    pub path: Option<String>,
    pub options: JsonMap<String, Json>,
    pub hold: bool,
    pub rate: Option<crate::rate::Rate>,
    /// Files to attach to a message: the same row's, or an output's shared files.
    pub attach: Vec<PathBuf>,
    /// An attached per-row output's row failed or was skipped: the message is skipped.
    pub attach_missing: bool,
    /// `zip:`, its password rendered with `each`.
    pub zip: Option<crate::zip_pack::ZipSpec>,
    /// Where the row's zip for this entry is built; delivered instead of the row's files.
    pub zip_file: Option<PathBuf>,
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
struct Job {
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

/// What every worker shares.
struct Shared<'a> {
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
    fn event(&self, finished: bool) -> crate::run::RowProgress {
        let (done, failed) = *self.counts.lock().unwrap();
        crate::run::RowProgress {
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

impl BindingRun<'_> {
    /// Make and deliver per-row output `oi`: one copy per row of its driver.
    pub(super) fn run_rows(&mut self, oi: usize) -> Result<(), Fail> {
        let o = self.b.outputs[oi].clone();
        let fe = o.for_each.clone().expect("a per-row output");
        let label = o.label(oi);
        let failed = |m: String| Fail::new(Code::ForEachFailed, format!("{label}: {m}"));
        // The population: the driver and each query the output makes a section of.
        let driver_p = self
            .produced
            .iter()
            .position(|p| p.query == fe.query)
            .ok_or_else(|| failed(format!("its driver `{}` made no result", fe.query)))?;
        let data_ps: Vec<usize> = (0..self.produced.len())
            .filter(|&i| o.sections(&self.produced[i].query))
            .filter(|&i| !self.outs[oi].dropped.contains(&self.produced[i].query))
            .collect();
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
        let read = |p: &Produced| -> Result<RowTable, String> {
            let reader = FileReader::try_new(File::open(&p.spool).map_err(|e| e.to_string())?, None)
                .map_err(|e| e.to_string())?;
            let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            RowTable::new(&p.query, p.schema.clone(), &batches)
        };
        let driver = read(&self.produced[driver_p])?;
        let data: Vec<RowTable> = data_ps
            .iter()
            .map(|&i| read(&self.produced[i]))
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
        // Which rows: all, less those a retry finished, `--only-keys`, then `--limit-rows` (or a
        // preview's first three).
        let stem = self.output_stem(oi);
        let done = self
            .retry
            .as_ref()
            .and_then(|r| r.done.get(&stem))
            .cloned()
            .unwrap_or_default();
        if let Some(keys) = &self.opts.only_keys {
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
            .filter(|&i| {
                self.opts
                    .only_keys
                    .as_ref()
                    .is_none_or(|k| k.contains(&pop.keys[i]))
            })
            .collect();
        let preview = self.opts.preview.is_some();
        let limit = self
            .opts
            .limit_rows
            .or((preview && self.opts.only_keys.is_none()).then_some(3));
        let population = rows.len();
        if let Some(n) = limit {
            rows.truncate(n);
        }
        let partial = self.retry.is_some() || self.opts.only_keys.is_some() || self.opts.limit_rows.is_some();
        let mut summary = ForEachResult {
            query: fe.query.clone(),
            key: fe.key.clone(),
            rows: rows.len() as u64,
            orphan_rows: pop.orphan_rows,
            partial,
            ..ForEachResult::default()
        };
        let sidecar = self.run_dir.join(format!("{stem}.rows.jsonl"));
        summary.sidecar = record_path(self.project, &sidecar);
        self.outs[oi].for_each = Some(summary.clone());
        if rows.is_empty() {
            // An empty driver (`on_empty` has decided already), or nothing left to redo.
            let why = if pop.is_empty() {
                format!("the driver `{}` has no rows", fe.query)
            } else {
                "no row left to make".to_string()
            };
            self.ui
                .step(Level::Info, "No data", &format!("{label}: {why}"), None);
            std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
            std::fs::write(&sidecar, "").map_err(|e| e.to_string())?;
            self.outs[oi].status = Some(OutputStatus::Skipped);
            self.outs[oi].reason = Some("no_data".into());
            return Ok(());
        }

        // File names, by row: checked for collisions before anything renders.
        let folder = self.run_dir.join(safe_name(&self.output_stem(oi)));
        let ext = match &o.extension {
            Some(e) => e.clone(),
            None => extension(&o.format).to_string(),
        };
        let renderer = self.renderer(None)?;
        let cal = self.calendar;
        let mut eaches = Vec::with_capacity(rows.len());
        let mut names = Vec::with_capacity(rows.len());
        for &i in &rows {
            let (each_json, each_value) = each_of(&pop, i, cal);
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
                None => match ext.as_str() {
                    "" => safe_name(&pop.keys[i]),
                    e => format!("{}.{e}", safe_name(&pop.keys[i])),
                },
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

        // Each data query's result set description, as for a single output.
        let metas: Vec<ResultSetMeta> = data_ps
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
        let single =
            !(o.format == "xlsx" || o.template.is_some() || !is_single_table(&o.format)) && data_ps.len() > 1;
        let template_base = if crate::document::FORMATS.contains(&o.format.as_str()) {
            Some(self.document_payload(o.template.as_ref())?)
        } else if let Some(t) = &o.template {
            Some(self.template_payload(t)?)
        } else {
            None
        };
        let is_message = o.is_message();
        let droppable = !(is_message || crate::document::FORMATS.contains(&o.format.as_str()));

        // Attached outputs, for a message: a per-row output's row files by key, else shared files.
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

        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let rows_file =
            File::create(&sidecar).map_err(|e| format!("can't write {}: {e}", sidecar.display()))?;
        let concurrency = self
            .opts
            .row_concurrency
            .or(fe.concurrency)
            .unwrap_or(crate::project::DEFAULT_ROW_CONCURRENCY)
            .clamp(1, 64);
        let shared = Shared {
            project: self.project,
            pop: &pop,
            format: o.format.clone(),
            options: o.options.clone(),
            metas,
            is_message,
            deliver: !(self.opts.no_deliver || self.opts.preview.is_some()),
            hold_all: self.opts.hold,
            log: self.ui.plugin_log(),
            events: self.ui.clone(),
            cancel: self.opts.cancel.clone(),
            pacer: crate::rate::Pacer::default(),
            rows_file: Mutex::new(rows_file),
            progress: Progress {
                output: label.clone(),
                total: rows.len() as u64,
                // The pace sends go at, when they go now (held ones are paced by `dre release`).
                rate: self.opts.rate.or_else(|| {
                    o.destinations
                        .iter()
                        .filter(|d| !(d.hold || self.opts.hold))
                        .find_map(|d| d.rate)
                }),
                counts: Mutex::new((0, 0)),
            },
        };
        let two_pass = fe.on_row_error == RowErrors::Fail;
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

        // Prepare each row's job (rendering its Jinja here, on the run's thread), and let the
        // workers make and deliver them.
        let (res_tx, res_rx) = mpsc::channel::<(usize, Job, RowRecord)>();
        let mut records: Vec<(usize, Job, RowRecord)> = Vec::new();
        {
            let first = if two_pass { Phase::Render } else { Phase::All };
            let cancel = self.opts.cancel.clone();
            let mut prepared = rows.iter().enumerate().map(|(n, &i)| {
                self.row_job(
                    oi,
                    &pop,
                    i,
                    &names[n],
                    &eaches[n],
                    &folder,
                    single,
                    droppable,
                    template_base.as_ref(),
                    &attached,
                    &fe.key,
                    first,
                )
            });
            let (job_tx, job_rx) = mpsc::sync_channel::<Job>(concurrency * 2);
            let job_rx = Mutex::new(job_rx);
            let mut prep_error = None;
            std::thread::scope(|s| {
                for _ in 0..concurrency {
                    let res_tx = res_tx.clone();
                    let job_rx = &job_rx;
                    let shared = &shared;
                    s.spawn(move || worker(shared, job_rx, res_tx));
                }
                for job in prepared.by_ref() {
                    if cancel.is_cancelled() {
                        break;
                    }
                    match job {
                        Ok(j) => {
                            if job_tx.send(j).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            prep_error = Some(e);
                            break;
                        }
                    }
                }
                drop(job_tx);
            });
            records.extend(res_rx.try_iter());
            if let Some(e) = prep_error {
                return Err(e);
            }
        }
        if two_pass {
            let any_failed = records.iter().any(|(_, _, r)| r.status == "failed");
            if any_failed {
                // Deliver nothing: record every rendered row as not delivered.
                for (_, job, r) in records.iter_mut() {
                    if r.status == "rendered" {
                        r.status = "failed".into();
                        r.error = Some(FailedKey {
                            key: job.key.clone(),
                            code: Code::RowsFailed.to_string(),
                            message: "not delivered: another row failed and `on_row_error: fail` delivers all or nothing".into(),
                        });
                        write_record(&shared, r);
                        shared.progress.row_done(&shared.events, true);
                    }
                }
            } else {
                // Rows the first pass skipped keep their record; the rendered ones deliver now.
                let (rendered, kept): (Vec<_>, Vec<_>) =
                    records.drain(..).partition(|(_, _, r)| r.status == "rendered");
                records = kept;
                let second: Vec<Job> = rendered
                    .into_iter()
                    .map(|(_, mut job, _)| {
                        job.phase = Phase::Deliver;
                        job
                    })
                    .collect();
                let (job_tx, job_rx) = mpsc::sync_channel::<Job>(concurrency * 2);
                let job_rx = Mutex::new(job_rx);
                let cancel = self.opts.cancel.clone();
                std::thread::scope(|s| {
                    for _ in 0..concurrency {
                        let res_tx = res_tx.clone();
                        let job_rx = &job_rx;
                        let shared = &shared;
                        s.spawn(move || worker(shared, job_rx, res_tx));
                    }
                    for j in second {
                        if cancel.is_cancelled() || job_tx.send(j).is_err() {
                            break;
                        }
                    }
                    drop(job_tx);
                });
                records.extend(res_rx.try_iter());
            }
        }
        drop(res_tx);
        shared.events.rows(shared.progress.event(true));
        records.sort_by_key(|(i, _, _)| *i);

        // The summary, and what a message attaching this output finds.
        let mut s = self.outs[oi].for_each.take().unwrap_or_default();
        for (_, job, r) in &records {
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
        // Held deliveries, for `dre release`: the rendered entry, unmasked, and the files' checksums.
        for (_, job, r) in &records {
            for (d, rd) in job.dests.iter().zip(&r.deliveries) {
                if rd.status == "held" {
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
        if s.failed > 0 {
            let keys: Vec<&str> = s.failed_keys.iter().take(10).map(|k| k.key.as_str()).collect();
            let more = s.failed_keys.len().saturating_sub(10);
            self.ui.step(
                Level::Info,
                "Rows",
                &format!(
                    "{line}; failed keys: {}{}",
                    keys.join(", "),
                    if more > 0 {
                        format!(" (+{more} more)")
                    } else {
                        String::new()
                    }
                ),
                None,
            );
        } else {
            self.ui.step(Level::Info, "Rows", &line, None);
        }
        if preview {
            self.preview_rows(oi, &records, population);
        }
        if let Some(r) = &self.retry {
            // What's finished now, this retry included, for the next `--retry-failed`.
            let mut all = r.done.get(&stem).cloned().unwrap_or_default();
            all.extend(
                records
                    .iter()
                    .filter(|(_, _, r)| finished(r))
                    .map(|(_, j, _)| j.key.clone()),
            );
            let path = self.run_dir.join(format!("{stem}.done.json"));
            let mut keys: Vec<&String> = all.iter().collect();
            keys.sort();
            std::fs::write(&path, serde_json::to_string(&keys).unwrap_or_default())
                .map_err(|e| e.to_string())?;
        }
        let failed_n = s.failed;
        let s_held_only = s.held > 0 && s.succeeded == 0;
        let first_error = s.failed_keys.first().cloned();
        self.outs[oi].for_each = Some(s);
        self.not_cancelled()?;
        self.outs[oi].status = Some(if failed_n > 0 {
            OutputStatus::Failed
        } else if s_held_only {
            OutputStatus::Held
        } else if shared.deliver {
            OutputStatus::Delivered
        } else {
            OutputStatus::Kept
        });
        if !shared.deliver && failed_n == 0 {
            self.outs[oi].delivery_note = Some(if self.opts.no_deliver {
                "--no-deliver: output kept locally".into()
            } else {
                "preview: not delivered".into()
            });
        }
        if failed_n > 0 {
            let e = first_error.expect("a failed row has an error");
            let msg = format!(
                "{label}: {failed_n} row(s) failed (first: key `{}`: {}); see {}",
                e.key,
                e.message,
                record_path(self.project, &sidecar).display()
            );
            self.outs[oi].error = Some(msg.clone());
            return Err(Fail::new(Code::RowsFailed, msg));
        }
        Ok(())
    }

    /// `--preview` of a per-row output: each previewed row's files and rendered destinations
    /// (masked unless `--unmask`), and what a real run would send.
    fn preview_rows(&mut self, oi: usize, records: &[(usize, Job, RowRecord)], population: usize) {
        let label = self.b.outputs[oi].label(oi);
        let mut sends = 0usize;
        for (_, job, r) in records {
            let shown = |t: &str| {
                if self.opts.unmask {
                    t.to_string()
                } else {
                    job.mask.apply(t)
                }
            };
            let files: Vec<String> = job
                .files
                .iter()
                .map(|(f, _)| rel(&self.project.root, f).display().to_string())
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
        if let Some((_, j, _)) = records.first() {
            sends = j.dests.len();
        }
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
        let original = results["retry_of"].as_str().unwrap_or(id).to_string();
        if original != id && read(&original).is_none() {
            return Err(Fail::new(
                Code::RetryFailed,
                format!(
                    "--retry-failed {id}: it redid run `{original}`, which is no longer in the target path"
                ),
            ));
        }
        let mut done: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        let original_dir = self.runs.run_dir(&original);
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
                if read(&run).is_some_and(|r| r["retry_of"].as_str() == Some(original.as_str()))
                    && let Ok(text) =
                        std::fs::read_to_string(self.runs.run_dir(&run).join(format!("{stem}.done.json")))
                    && let Ok(keys) = serde_json::from_str::<Vec<String>>(&text)
                {
                    set.extend(keys);
                }
            }
        }
        self.retry = Some(Retry {
            original: original.clone(),
            done,
        });
        let snapshot = original_dir.join(SNAPSHOT_DIR);
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

    /// Save the run's query results in its folder, for `--retry-failed`.
    pub(super) fn save_snapshot(&mut self) -> Result<(), Fail> {
        let dir = self.run_dir.join(SNAPSHOT_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut index = crate::saved::Index {
            schema_version: crate::saved::SCHEMA_VERSION.into(),
            run_id: self.run_id.clone(),
            results: Vec::new(),
        };
        for r in &self.produced {
            let file = format!("{}.arrows", safe_name(&r.query));
            crate::saved::spool_to_stream(&r.spool, &dir.join(&file))?;
            index.results.push(crate::saved::SavedResult {
                query: r.query.clone(),
                name: r.name.clone(),
                connection: r.connection.clone(),
                rows: r.rows,
                sql_sha256: std::fs::read_to_string(self.compiled_dir.join(format!("{}.sha256", r.query)))
                    .ok()
                    .map(|h| h.trim().to_string()),
                file,
                // The run folder keeps the real values: a retry delivers what they feed.
                masked_columns: Vec::new(),
            });
        }
        crate::saved::write_index(&dir, &index).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The name a per-row output's folder and sidecar take: its `name:`, else the report's.
    fn output_stem(&self, oi: usize) -> String {
        self.b.outputs[oi]
            .name
            .clone()
            .unwrap_or_else(|| self.report.name.clone())
    }

    /// Prepare row `i`'s job: its files, its slices' `on_empty`, its destinations and message
    /// rendered with `each`.
    #[allow(clippy::too_many_arguments)]
    fn row_job(
        &mut self,
        oi: usize,
        pop: &Population,
        i: usize,
        name: &str,
        each: &(JsonMap<String, Json>, minijinja::Value),
        folder: &Path,
        single: bool,
        droppable: bool,
        template_base: Option<&Json>,
        attached: &[Attached],
        key_cols: &[String],
        phase: Phase,
    ) -> Result<Job, Fail> {
        let o = &self.b.outputs[oi];
        let key = pop.keys[i].clone();
        let mask = if o
            .for_each
            .as_ref()
            .and_then(|f| f.mask_row_values)
            .unwrap_or(self.project.mask_row_values)
        {
            RowMask::new(&each.0, key_cols)
        } else {
            RowMask::default()
        };
        // This row's slices and their `on_empty`.
        let slice_rows: Vec<u64> = (0..pop.data.len()).map(|q| pop.slice_len(i, q)).collect();
        let entries: Vec<crate::no_data::Query<'_>> = pop
            .data
            .iter()
            .zip(&slice_rows)
            .map(|((t, _), rows)| crate::no_data::Query {
                name: &t.name,
                rows: *rows,
                on_empty: self
                    .b
                    .queries
                    .iter()
                    .find(|q| q.query == t.name)
                    .and_then(|q| q.on_empty),
            })
            .collect();
        let decision = if entries.is_empty() {
            crate::no_data::Decision::default()
        } else {
            crate::no_data::decide(o.on_empty, &entries, droppable)
        };
        let mut job = Job {
            i,
            key: key.clone(),
            mask: mask.clone(),
            files: Vec::new(),
            template: None,
            options: None,
            message: None,
            dests: Vec::new(),
            pre: None,
            phase,
        };
        if let Some(why) = decision.fail {
            job.pre = Some(RowRecord {
                key: key.clone(),
                status: "failed".into(),
                reason: Some("no_data".into()),
                files: Vec::new(),
                deliveries: Vec::new(),
                error: Some(FailedKey {
                    key,
                    code: Code::NoData.to_string(),
                    message: why,
                }),
            });
            return Ok(job);
        }
        if decision.skip.is_some() {
            job.pre = Some(RowRecord {
                key,
                status: "skipped".into(),
                reason: Some("no_data".into()),
                files: Vec::new(),
                deliveries: Vec::new(),
                error: None,
            });
            return Ok(job);
        }
        let kept: Vec<usize> = (0..pop.data.len())
            .filter(|&q| !decision.drop.contains(&pop.data[q].0.name))
            .collect();
        let path = folder.join(name);
        job.files = if single {
            let (stem, ext) = split_ext(name);
            kept.iter()
                .map(|&q| {
                    (
                        folder.join(format!("{stem}_{}{ext}", pop.data[q].0.name)),
                        vec![q],
                    )
                })
                .collect()
        } else {
            vec![(path.clone(), kept.clone())]
        };
        if let Some(Json::Object(base)) = template_base {
            let mut t = base.clone();
            if crate::document::FORMATS.contains(&o.format.as_str()) {
                t.insert("each".into(), Json::Object(each.0.clone()));
            }
            job.template = Some(Json::Object(t));
        }
        // Destinations and a message, rendered with `each` (and a message's row results).
        let renderer = self.renderer(None)?;
        let mut ctx_map: BTreeMap<String, minijinja::Value> = BTreeMap::new();
        ctx_map.insert("each".into(), each.1.clone());
        if o.is_message() {
            let slices = pop.slices(i)?;
            let results: Vec<(String, Arc<QueryResult>)> = kept
                .iter()
                .map(|&q| {
                    let t = &pop.data[q].0;
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
            let base = self.template_context(oi, &results);
            for k in ["results", "outputs"] {
                if let Ok(v) = base.get_attr(k) {
                    ctx_map.insert(k.into(), v);
                }
            }
            let ctx = minijinja::Value::from(ctx_map.clone());
            let opt = |k: &str| o.options.get(k).and_then(Json::as_str).map(str::to_string);
            let text = match (opt("text"), opt("file")) {
                (Some(t), _) => renderer.render_with(&self.report.file, &t, ctx.clone(), true),
                (None, Some(f)) => {
                    let path = crate::project::find_template(&self.project.root, &f)
                        .ok_or_else(|| format!("message file `{f}` doesn't exist"))?;
                    let src =
                        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    renderer.render_with(&rel(&self.project.root, &path), &src, ctx.clone(), true)
                }
                (None, None) => Ok(crate::message::default_text(&results, self.locale())),
            }
            .map_err(|e| Fail::new(Code::RenderFailed, mask.apply(&e.message)))?;
            let title = match opt("title") {
                Some(t) => renderer
                    .render_with(&self.report.file, &t, ctx.clone(), false)
                    .map_err(|e| Fail::new(Code::RenderFailed, mask.apply(&e.message)))?,
                None => format!("{}: {}", self.report.name, self.date),
            };
            let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
            job.message = Some((title, text.trim().to_string()));
            let (stem, _) = split_ext(name);
            job.files = vec![(folder.join(format!("{stem}.md")), Vec::new())];
        }
        let ctx = minijinja::Value::from(ctx_map);
        if !o.is_message() && o.options.values().any(has_template) {
            let rendered = render_json_with(
                &renderer,
                &self.report.file,
                &Json::Object(o.options.clone()),
                &ctx,
            )
            .map_err(|e| {
                Fail::new(
                    Code::RenderFailed,
                    format!("format option: {}", mask.apply(&e.message)),
                )
            })?;
            if let Json::Object(m) = rendered {
                job.options = Some(m);
            }
        }
        let connections = self.pool.as_ref().expect("pool").connections.clone();
        for (idx, d) in o.destinations.iter().enumerate() {
            let profile = renderer
                .render_with(&self.report.file, &d.profile, ctx.clone(), false)
                .map_err(|e| {
                    Fail::new(
                        Code::RenderFailed,
                        format!("destination profile: {}", mask.apply(&e.message)),
                    )
                })?;
            renderer.set_destination(connections.profile(&profile, Some("destination")).ok());
            let rendered = (|| -> Result<RowDest, Fail> {
                let path = match &d.path {
                    Some(p) => Some(
                        renderer
                            .render_with(&self.report.file, p, ctx.clone(), false)
                            .map_err(|e| format!("output path: {}", mask.apply(&e.message)))?,
                    ),
                    None => None,
                };
                let mut options = JsonMap::new();
                for (k, v) in &d.options {
                    let v = render_json_with(&renderer, &self.report.file, v, &ctx).map_err(|e| {
                        format!("destination `{profile}` option `{k}`: {}", mask.apply(&e.message))
                    })?;
                    options.insert(k.clone(), v);
                }
                let mut attach = Vec::new();
                let mut attach_missing = false;
                for name in &d.attach {
                    if let Some((_, per_row, shared)) = attached.iter().find(|(n, _, _)| n == name) {
                        match per_row {
                            Some(rows) => match rows.get(&key) {
                                Some(files) => attach.extend(files.iter().cloned()),
                                None => attach_missing = true,
                            },
                            None => attach.extend(shared.iter().cloned()),
                        }
                    }
                }
                let zip = match &d.zip {
                    Some(z) => Some(crate::zip_pack::ZipSpec {
                        password: match &z.password {
                            Some(pw) => Some(
                                renderer
                                    .render_with(&self.report.file, pw, ctx.clone(), false)
                                    .map_err(|e| format!("`zip.password`: {}", mask.apply(&e.message)))?,
                            ),
                            None => None,
                        },
                        encryption: z.encryption,
                    }),
                    None => None,
                };
                let zip_file = match &zip {
                    Some(_) => {
                        let stem = job
                            .files
                            .first()
                            .and_then(|(f, _)| f.file_stem())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| key.clone());
                        let name = crate::zip_pack::zip_name(
                            path.as_deref()
                                .or(options.get("attachment_name").and_then(Json::as_str)),
                            &stem,
                        )
                        .map_err(|e| format!("destination `{profile}`: {e}"))?;
                        // Entries zipping the same files the same way share one zip.
                        let shared = job.dests.iter().find(|x| {
                            x.zip == zip
                                && x.attach == attach
                                && x.zip_file.as_ref().is_some_and(|f| f.ends_with(&name))
                        });
                        Some(match shared.and_then(|x| x.zip_file.clone()) {
                            Some(f) => f,
                            None => folder.join("zips").join(format!("{}-{idx}", stem)).join(&name),
                        })
                    }
                    None => None,
                };
                Ok(RowDest {
                    idx,
                    profile: profile.clone(),
                    path,
                    options,
                    hold: d.hold,
                    rate: self.opts.rate.or(d.rate),
                    attach,
                    attach_missing,
                    zip,
                    zip_file,
                })
            })();
            renderer.set_destination(None);
            job.dests.push(rendered?);
        }
        if job.dests.iter().any(|d| d.attach_missing) {
            job.pre = Some(RowRecord {
                key: job.key.clone(),
                status: "skipped".into(),
                reason: Some("attachment_failed".into()),
                files: Vec::new(),
                deliveries: Vec::new(),
                error: None,
            });
        }
        Ok(job)
    }
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

/// A worker: takes jobs until there are none (or the run is cancelled), with its own plugin
/// processes, and sends each row's record back.
fn worker(
    shared: &Shared<'_>,
    jobs: &Mutex<mpsc::Receiver<Job>>,
    results: mpsc::Sender<(usize, Job, RowRecord)>,
) {
    let mut format: Option<PluginProcess> = None;
    let mut dests: HashMap<String, PluginProcess> = HashMap::new();
    loop {
        if shared.cancel.is_cancelled() {
            break;
        }
        let job = match jobs.lock().unwrap().recv() {
            Ok(j) => j,
            Err(_) => break,
        };
        // A row the run stopped part-way isn't recorded: it never finished, so a retry makes it.
        let Some(record) = run_job(shared, &job, &mut format, &mut dests) else {
            break;
        };
        if record.status != "rendered" {
            write_record(shared, &record);
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
        let _ = results.send((job.i, job, record));
    }
    if let Some(p) = format {
        let _ = p.close();
    }
    for (_, p) in dests {
        let _ = p.close();
    }
}

fn row_failed(
    job: &Job,
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

fn run_job(
    shared: &Shared<'_>,
    job: &Job,
    format: &mut Option<PluginProcess>,
    dests: &mut HashMap<String, PluginProcess>,
) -> Option<RowRecord> {
    if let Some(pre) = &job.pre {
        return Some(pre.clone());
    }
    let root = &shared.project.root;
    if job.phase != Phase::Deliver
        && let Err(e) = make_files(shared, job, format)
    {
        // A plugin that failed may be broken: the next row starts a fresh one.
        if let Some(p) = format.take() {
            let _ = p.close();
        }
        return Some(row_failed(
            job,
            &e.code.to_string(),
            e.message,
            Vec::new(),
            Vec::new(),
        ));
    }
    if job.phase != Phase::Deliver
        && let Err(e) = make_zips(job)
    {
        return Some(row_failed(
            job,
            &Code::DeliveryFailed.to_string(),
            job.mask.apply(&e),
            Vec::new(),
            Vec::new(),
        ));
    }
    let files: Vec<RowFile> = job
        .files
        .iter()
        .map(|(f, _)| RowFile {
            path: record_path_in(shared.project, f),
            sha256: sha256_file(f).unwrap_or_default(),
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
    let mut deliveries = Vec::new();
    let mut first_error: Option<Fail> = None;
    let mut held = false;
    for d in &job.dests {
        let target = d
            .path
            .clone()
            .or_else(|| address(&d.options))
            .map(|t| job.mask.apply(&t));
        if d.hold || shared.hold_all {
            held = true;
            deliveries.push(RowDelivery {
                profile: d.profile.clone(),
                target,
                status: "held".into(),
                location: None,
                error: None,
            });
            continue;
        }
        if !shared.deliver {
            deliveries.push(RowDelivery {
                profile: d.profile.clone(),
                target,
                status: "kept".into(),
                location: None,
                error: None,
            });
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
        let r = deliver_row(
            shared.project,
            &shared.log,
            d,
            &local,
            job.message.as_ref(),
            dests,
            root,
        );
        let (status, location, error) = match r {
            Ok(Some(loc)) => ("delivered", Some(job.mask.apply(&loc)), None),
            Ok(None) => ("not_delivered", None, None),
            Err(e) => {
                let msg = job.mask.apply(&e.message);
                first_error.get_or_insert(Fail::new(e.code.clone(), msg.clone()));
                ("failed", None, Some(msg))
            }
        };
        deliveries.push(RowDelivery {
            profile: d.profile.clone(),
            target,
            status: status.into(),
            location,
            error,
        });
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

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(p)?;
    Ok(sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn record_path_in(project: &Project, p: &Path) -> PathBuf {
    record_path(project, p)
}

/// Write a row's files through the format plugin (or a message's `.md`).
fn make_files(shared: &Shared<'_>, job: &Job, format: &mut Option<PluginProcess>) -> Result<(), Fail> {
    if shared.is_message {
        let (title, text) = job.message.clone().unwrap_or_default();
        let (path, _) = &job.files[0];
        std::fs::write(path, crate::message::file_body(&title, &text)).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let slices = shared.pop.slices(job.i)?;
    if format.is_none() {
        let plugin = find_plugin(shared.project, PluginKind::Format, &shared.format)
            .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?;
        *format = Some(
            plugin
                .start(shared.log.clone(), Some(&shared.project.root))
                .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?,
        );
    }
    let p = format.as_mut().expect("started");
    let fmt_err =
        |e: HostError| Fail::from_plugin(&e, format!("{} format: {e}", shared.format)).or(Code::FormatFailed);
    for (path, qs) in &job.files {
        let metas: Vec<ResultSetMeta> = qs.iter().map(|&q| shared.metas[q].clone()).collect();
        p.write_begin(
            &path.to_string_lossy(),
            &shared.format,
            job.options.clone().unwrap_or_else(|| shared.options.clone()),
            metas,
            job.template.clone(),
        )
        .map_err(fmt_err)?;
        for &q in qs {
            let b = &slices[q];
            p.write_result_set(&b.schema(), vec![b.clone()])
                .map_err(fmt_err)?;
        }
        let (_files, warnings) = p.write_finish_with_warnings().map_err(fmt_err)?;
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
fn make_zips(job: &Job) -> Result<(), String> {
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

/// Deliver a row's files (or its message) to one destination. `Ok(None)`: the entry delivers
/// nowhere (`deliver: false`).
pub(crate) fn deliver_row(
    project: &Project,
    log: &LogSink,
    d: &RowDest,
    files: &[PathBuf],
    message: Option<&(String, String)>,
    plugins: &mut HashMap<String, PluginProcess>,
    root: &Path,
) -> Result<Option<String>, Fail> {
    let profiles = &project.profiles;
    let out = match profiles.entry(Role::Destination, &d.profile) {
        Entry::Use(o) => o,
        Entry::Nowhere => return Ok(None),
        Entry::Missing => {
            return Err(Fail::new(
                Code::MissingTargetEntry,
                profiles.missing_entry(Role::Destination, &d.profile),
            ));
        }
        Entry::Unknown => {
            return Err(format!("destination profile `{}` isn't in profiles.yml", d.profile).into());
        }
    };
    let kind = out.kind.clone();
    let connection = render_connection(out)?;
    let targets: Vec<DeliveryFile> = files
        .iter()
        .map(|f| {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let r = d.path.as_deref().map(|r| {
                if files.len() == 1 {
                    r.to_string()
                } else {
                    match r.rfind(['/', '\\']) {
                        Some(i) => format!("{}{name}", &r[..=i]),
                        None => name.clone(),
                    }
                }
            });
            DeliveryFile {
                local_path: f.to_string_lossy().to_string(),
                remote_path: r,
            }
        })
        .collect();
    if kind == LOCAL_TYPE {
        let errors = crate::options::local_option_errors(&d.options);
        if !errors.is_empty() {
            return Err(format!("destination `{}`: {}", d.profile, errors.join("; ")).into());
        }
        let (rules, _) = dre_protocol::delivery::Rules::from_settings(
            dre_protocol::delivery::Rules {
                retries: 0,
                ..Default::default()
            },
            &JsonMap::new(),
            &d.options,
            &[],
        )?;
        let mut locations = Vec::new();
        for f in &targets {
            let r = f
                .remote_path
                .as_ref()
                .ok_or("the local destination needs `output.destination.path`")?;
            let dst = root.join(r).to_string_lossy().to_string();
            let delivered = dre_protocol::delivery::deliver(
                &mut dre_protocol::delivery::LocalStore,
                Path::new(&f.local_path),
                &dst,
                &rules,
            )
            .map_err(|e| Fail::new(Code::DeliveryFailed, format!("delivery to {dst} failed: {e}")))?;
            locations.push(delivered.path);
        }
        return Ok(Some(locations.join(", ")));
    }
    let failed = |e: HostError| Fail::from_plugin(&e, format!("delivery through `{kind}` failed: {e}"));
    if !plugins.contains_key(&kind) {
        let plugin = find_plugin(project, PluginKind::Destination, &kind)
            .map_err(|e| Fail::new(Code::DeliveryFailed, e.to_string()))?;
        let p = plugin
            .start(log.clone(), Some(&project.root))
            .map_err(|e| Fail::new(Code::DeliveryFailed, e.to_string()))?;
        plugins.insert(kind.clone(), p);
    }
    let p = plugins.get_mut(&kind).expect("started");
    let result = (|| {
        if let Some((title, text)) = message
            && p.has(CAP_MESSAGE)
        {
            let m = dre_protocol::msg::Message {
                title: title.clone(),
                text: text.clone(),
                html: None,
                path: targets[0].local_path.clone(),
            };
            let attach: Vec<DeliveryFile> = d
                .attach
                .iter()
                .map(|f| DeliveryFile {
                    local_path: f.to_string_lossy().to_string(),
                    remote_path: None,
                })
                .collect();
            return p
                .deliver_message(&m, &attach, connection.clone(), d.options.clone())
                .map_err(failed);
        }
        if message.is_none() && p.has(CAP_MESSAGE_ONLY) {
            return Err(Fail::from(format!("`{kind}` only takes messages")));
        }
        let mut locations = Vec::new();
        let batches: Vec<Vec<usize>> = if targets.len() > 1 && p.has(CAP_MULTI_FILE) {
            vec![(0..targets.len()).collect()]
        } else {
            (0..targets.len()).map(|i| vec![i]).collect()
        };
        for batch in batches {
            let files: Vec<DeliveryFile> = batch.iter().map(|&i| targets[i].clone()).collect();
            locations.push(
                p.deliver_files(&files, connection.clone(), d.options.clone())
                    .map_err(failed)?,
            );
        }
        Ok(locations.join(", "))
    })();
    if result.is_err() {
        // A destination plugin that failed may be broken: the next row starts a fresh one.
        if let Some(p) = plugins.remove(&kind) {
            let _ = p.close();
        }
    }
    result.map(Some)
}
