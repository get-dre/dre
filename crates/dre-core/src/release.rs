//! Held deliveries and `dre release`: a maker/checker step between rendering and sending.
//!
//! A destination with `hold: true` (or every destination, with `dre run --hold`) isn't delivered
//! by the run: the run renders everything, delivers the rest, and records each held delivery in
//! `held.json` in its run folder, with the files' SHA-256. `dre release <report> --run <id>`
//! delivers them later from that folder's files, byte for byte what was reviewed: it never
//! re-renders or re-queries, refuses a file that changed since, takes the Binding's lock, and
//! never sends a delivery twice. Each release is its own run folder (`release_of`), recording
//! who released what, and when.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as Json};

use crate::codes::Code;
use crate::engine::Events;
use crate::error::Error as Fail;
use crate::project::Project;
use crate::run::per_row::{RowDest, deliver_row};
use crate::run_results::Released;
use crate::runs::BindingRuns;

/// `held.json` in a run folder.
pub const HELD: &str = "held.json";
pub const SCHEMA_VERSION: &str = "dre/held/v1";
/// In a release's run folder: every entry released so far, this release's included.
pub const RELEASED: &str = "released.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Held {
    pub schema_version: String,
    pub run_id: String,
    pub entries: Vec<HeldEntry>,
}

/// One held delivery: the destination entry as rendered (unmasked: it's what will be sent) and
/// the files it sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldEntry {
    /// `<output>/<key>/<n>` for a per-row output, `<output>/<n>` otherwise.
    pub id: String,
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub options: JsonMap<String, Json>,
    /// Files attached to a message, with their SHA-256 at the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attach: Vec<HeldFile>,
    /// The destination's `rate:` at the run, which the release keeps unless `--rate` says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<crate::rate::Rate>,
    /// A message's title and text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<(String, String)>,
    pub files: Vec<HeldFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldFile {
    pub path: PathBuf,
    pub sha256: String,
}

/// A file and its SHA-256 now, for a held delivery.
pub fn held_file(path: &Path) -> HeldFile {
    HeldFile {
        path: path.to_path_buf(),
        sha256: crate::checksum::sha256_file(path).unwrap_or_default(),
    }
}

pub fn write_held(run_dir: &Path, run_id: &str, entries: &[HeldEntry]) -> std::io::Result<()> {
    let held = Held {
        schema_version: SCHEMA_VERSION.into(),
        run_id: run_id.to_string(),
        entries: entries.to_vec(),
    };
    crate::fs::write_atomic(
        &run_dir.join(HELD),
        serde_json::to_string_pretty(&held)?.as_bytes(),
    )
}

/// `released.json` in a release's folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleasedFile {
    release_of: String,
    released: Vec<String>,
}

/// What `dre release` was asked to do.
#[derive(Debug, Clone, Default)]
pub struct ReleaseOptions {
    pub report: String,
    /// The Binding's folder name: the Set's, or `default`.
    pub binding: String,
    pub run_id: String,
    pub only_keys: Option<Vec<String>>,
    pub exclude_keys: Option<Vec<String>>,
    /// Who releases (`--by`, default `$USER`).
    pub by: String,
    /// `--rate`: every delivery's rate for this release, over the recorded ones.
    pub rate: Option<crate::rate::Rate>,
    /// Ctrl-C or a termination: no delivery starts after it.
    pub cancel: crate::engine::CancelToken,
}

/// How a release went.
#[derive(Debug, Clone, Default)]
pub struct ReleaseOutcome {
    pub release_id: String,
    pub released: Vec<Released>,
    /// Entries skipped because an earlier release delivered them.
    pub already: usize,
}

impl ReleaseOutcome {
    pub fn failed(&self) -> bool {
        self.released
            .iter()
            .any(|r| r.status != "delivered" && r.status != "not_delivered")
    }
}

/// Deliver run `opts.run_id`'s held deliveries.
pub fn release(
    ctx: &crate::Context,
    project: &Project,
    opts: &ReleaseOptions,
    events: &Events,
) -> Result<ReleaseOutcome, Fail> {
    let store = crate::engine::RunStore::new(&project.target_dir, ctx.secrets());
    let runs = BindingRuns::new(&store.run_dir(&opts.report, &opts.binding));
    let run_dir = runs.run_dir(&opts.run_id);
    let held_path = run_dir.join(HELD);
    if !run_dir.is_dir() {
        return Err(Fail::new(
            Code::ReleaseFailed,
            format!(
                "{} [{}] has no run `{}` in the target path (it may have been pruned: keep it with `keep_runs` for the review window; `dre history` lists the runs)",
                opts.report, opts.binding, opts.run_id
            ),
        ));
    }
    let held: Held = match std::fs::read_to_string(&held_path) {
        Ok(t) => {
            serde_json::from_str(&t).map_err(|e| format!("{} is malformed: {e}", held_path.display()))?
        }
        Err(_) => {
            return Err(Fail::new(
                Code::ReleaseFailed,
                format!(
                    "run `{}` has no held deliveries (no {HELD} in its folder)",
                    opts.run_id
                ),
            ));
        }
    };
    // Entries an earlier release delivered: the newest release's cumulative list.
    let mut done: BTreeSet<String> = BTreeSet::new();
    // Each release keeps every entry released so far (written as each one goes out).
    for id in runs.list() {
        if let Ok(t) = std::fs::read_to_string(runs.run_dir(&id).join(RELEASED))
            && let Ok(r) = serde_json::from_str::<ReleasedFile>(&t)
            && r.release_of == opts.run_id
        {
            done.extend(r.released);
        }
    }
    let started = chrono::Utc::now();
    let release_id = crate::runs::new_run_id(started);
    let _guard = runs
        .lock(&release_id, started, &crate::runs::host_name(ctx.env()))
        .map_err(|e| {
            let why = match e {
                crate::runs::LockError::Held(h) => format!("{} holds its lock", h.describe()),
                crate::runs::LockError::Unreadable(p) => {
                    format!("its lock file {} is unreadable", p.display())
                }
                crate::runs::LockError::Io(e) => format!("its lock can't be taken: {e}"),
            };
            Fail::new(
                Code::RunInProgress,
                format!(
                    "{} [{}] is busy: {why}; release when it finishes",
                    opts.report, opts.binding
                ),
            )
        })?;
    let release_dir = runs.run_dir(&release_id);
    std::fs::create_dir_all(&release_dir).map_err(|e| e.to_string())?;
    let mut outcome = ReleaseOutcome {
        release_id: release_id.clone(),
        ..ReleaseOutcome::default()
    };
    let chosen = |e: &HeldEntry| match &e.key {
        Some(k) => {
            opts.only_keys.as_ref().is_none_or(|ks| ks.contains(k))
                && opts.exclude_keys.as_ref().is_none_or(|ks| !ks.contains(k))
        }
        None => opts.only_keys.is_none(),
    };
    let log = events.plugin_log();
    let mut plugins: HashMap<String, Box<dyn crate::plugin_host::Plugin>> = HashMap::new();
    let pacer = crate::rate::Pacer::default();
    let mut progress = RowsLeft::new(
        held.entries.iter().filter(|e| chosen(e) && !done.contains(&e.id)),
        opts.rate,
    );
    progress.start(events);
    for e in held.entries.iter().filter(|e| chosen(e)) {
        if done.contains(&e.id) {
            outcome.already += 1;
            continue;
        }
        // Addresses and paths in messages are masked, as for a per-row run.
        let mask = if project.mask_row_values {
            let mut values: JsonMap<String, Json> = e.options.clone();
            if let Some(p) = &e.path {
                values.insert("$path".into(), Json::from(p.clone()));
            }
            crate::for_each::RowMask::new(&values, &[])
        } else {
            crate::for_each::RowMask::default()
        };
        let mut r = Released {
            id: e.id.clone(),
            output: e.output.clone(),
            key: e.key.clone(),
            profile: e.profile.clone(),
            status: String::new(),
            location: None,
            error: None,
        };
        // Byte for byte what was reviewed: a changed or missing file is refused.
        let changed: Vec<String> = e
            .files
            .iter()
            .chain(&e.attach)
            .filter(|f| crate::checksum::sha256_file(&f.path).ok().as_deref() != Some(f.sha256.as_str()))
            .map(|f| f.path.display().to_string())
            .collect();
        if !changed.is_empty() {
            r.status = "refused".into();
            r.error = Some(format!(
                "refused: {} changed (or is gone) since the run rendered it; re-run the report to render it again",
                changed.join(", ")
            ));
            events.warn(&format!("  {}: {}", e.id, r.error.as_deref().unwrap_or_default()));
            outcome.released.push(r);
            progress.entry_done(e, true, events);
            continue;
        }
        let dest = RowDest {
            idx: 0,
            profile: e.profile.clone(),
            path: e.path.clone(),
            options: e.options.clone(),
            hold: false,
            rate: None,
            attach: e.attach.iter().map(|f| f.path.clone()).collect(),
            attach_missing: false,
            // A held zip is already one of the entry's files: sent as it is.
            zip: None,
            zip_file: None,
        };
        // Paced per destination profile, at the recorded rate or `--rate`.
        if let Some(rate) = opts.rate.or(e.rate)
            && !pacer.wait(&e.profile, &rate, &opts.cancel)
        {
            break;
        }
        if opts.cancel.is_cancelled() {
            break;
        }
        let files: Vec<PathBuf> = e.files.iter().map(|f| f.path.clone()).collect();
        match deliver_row(
            ctx,
            project,
            &log,
            &dest,
            &files,
            e.message.as_ref(),
            &mut plugins,
            &project.root,
        ) {
            Ok(Some(loc)) => {
                r.status = "delivered".into();
                r.location = Some(mask.apply(&loc));
                done.insert(e.id.clone());
                // Recorded at once: a release stopped part-way never sends this one again.
                write_released(&release_dir, &opts.run_id, &done)?;
            }
            Ok(None) => r.status = "not_delivered".into(),
            Err(err) => {
                let msg = mask.apply(&err.message);
                r.status = "failed".into();
                r.error = Some(msg.clone());
                events.warn(&format!("  {}: {msg}", e.id));
            }
        }
        progress.entry_done(e, r.status == "failed", events);
        outcome.released.push(r);
    }
    progress.finish(events);
    for (_, p) in plugins {
        p.close();
    }
    // The release's own record: who, when, what; and every entry released so far.
    write_released(&release_dir, &opts.run_id, &done)?;
    let mut record: Json = std::fs::read_to_string(run_dir.join("run_results.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let failed = outcome.failed();
    if let Json::Object(m) = &mut record {
        m.insert("run_id".into(), Json::from(release_id));
        m.insert(
            "status".into(),
            Json::from(if failed { "error" } else { "success" }),
        );
        m.insert(
            "started_at".into(),
            Json::from(started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
        m.insert(
            "duration_ms".into(),
            Json::from((chrono::Utc::now() - started).num_milliseconds().max(0)),
        );
        m.insert("release_of".into(), Json::from(opts.run_id.clone()));
        m.insert("released_by".into(), Json::from(opts.by.clone()));
        m.insert(
            "released".into(),
            serde_json::to_value(&outcome.released).unwrap_or_default(),
        );
        m.insert("error".into(), Json::Null);
        m.insert("error_code".into(), Json::Null);
        m.insert("error_kind".into(), Json::Null);
        if failed {
            m.insert(
                "error".into(),
                Json::from("one or more held deliveries failed or were refused"),
            );
            m.insert("error_code".into(), Json::from(Code::ReleaseFailed.to_string()));
        }
        m.remove("retry_of");
    }
    crate::fs::write_atomic(
        &release_dir.join("run_results.json"),
        serde_json::to_string_pretty(&record)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    Ok(outcome)
}

/// Every entry released so far, this release's included, in the release's folder.
fn write_released(release_dir: &Path, run_id: &str, done: &BTreeSet<String>) -> Result<(), Fail> {
    let file = ReleasedFile {
        release_of: run_id.to_string(),
        released: done.iter().cloned().collect(),
    };
    crate::fs::write_atomic(
        &release_dir.join(RELEASED),
        serde_json::to_string(&file).unwrap_or_default().as_bytes(),
    )
    .map_err(|e| Fail::from(e.to_string()))
}

/// A release's progress in rows, per output: a row is done once each of its held deliveries
/// this release sends has gone (or failed, or was refused).
struct RowsLeft {
    outputs: Vec<crate::run::RowProgress>,
    /// Per (output label, row): deliveries still to go, and whether one failed.
    rows: HashMap<(String, String), (usize, bool)>,
}

impl RowsLeft {
    fn label(e: &HeldEntry) -> String {
        match &e.output {
            Some(o) => format!("output `{o}`"),
            None => "output".to_string(),
        }
    }

    /// A row's key: an entry without one (not a per-row output) is a row of its own.
    fn row(e: &HeldEntry) -> (String, String) {
        (Self::label(e), e.key.clone().unwrap_or_else(|| e.id.clone()))
    }

    fn new<'a>(pending: impl Iterator<Item = &'a HeldEntry>, rate: Option<crate::rate::Rate>) -> RowsLeft {
        let mut p = RowsLeft {
            outputs: Vec::new(),
            rows: HashMap::new(),
        };
        for e in pending {
            let label = Self::label(e);
            let slot = p.rows.entry(Self::row(e)).or_insert((0, false));
            slot.0 += 1;
            let new_row = slot.0 == 1;
            match p.outputs.iter_mut().find(|o| o.output == label) {
                Some(o) => {
                    if new_row {
                        o.total += 1;
                    }
                    o.rate = o.rate.or(rate.or(e.rate));
                }
                None => p.outputs.push(crate::run::RowProgress {
                    output: label,
                    done: 0,
                    failed: 0,
                    total: 1,
                    rate: rate.or(e.rate),
                    finished: false,
                }),
            }
        }
        p
    }

    fn start(&self, events: &Events) {
        for o in &self.outputs {
            events.rows(o.clone());
        }
    }

    fn entry_done(&mut self, e: &HeldEntry, failed: bool, events: &Events) {
        let Some(slot) = self.rows.get_mut(&Self::row(e)) else {
            return;
        };
        slot.0 = slot.0.saturating_sub(1);
        slot.1 |= failed;
        if slot.0 > 0 {
            return;
        }
        let row_failed = slot.1;
        let label = Self::label(e);
        if let Some(o) = self.outputs.iter_mut().find(|o| o.output == label) {
            o.done += 1;
            if row_failed {
                o.failed += 1;
            }
            events.rows(o.clone());
        }
    }

    fn finish(&mut self, events: &Events) {
        for o in &mut self.outputs {
            o.finished = true;
            events.rows(o.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::RunEvent;

    fn entry(output: &str, key: &str, n: usize) -> HeldEntry {
        HeldEntry {
            id: format!("{output}/{key}/{n}"),
            output: Some(output.into()),
            key: Some(key.into()),
            profile: "mail".into(),
            path: None,
            options: JsonMap::new(),
            attach: Vec::new(),
            rate: Some(crate::rate::Rate::parse("50/second").unwrap()),
            message: None,
            files: Vec::new(),
        }
    }

    #[test]
    fn a_row_is_done_once_all_its_deliveries_are() {
        // Row `a` has two deliveries (email and a copy), row `b` one.
        let entries = [
            entry("email", "a", 0),
            entry("email", "a", 1),
            entry("email", "b", 0),
        ];
        let (tx, rx) = std::sync::mpsc::channel();
        let events = Events::new(tx);
        let mut p = RowsLeft::new(entries.iter(), None);
        p.start(&events);
        p.entry_done(&entries[0], false, &events);
        p.entry_done(&entries[1], true, &events);
        p.entry_done(&entries[2], false, &events);
        p.finish(&events);
        drop(events);
        let seen: Vec<(u64, u64, u64, bool)> = rx
            .try_iter()
            .map(|e| match e {
                RunEvent::Rows(r) => {
                    assert_eq!(r.output, "output `email`");
                    assert_eq!(r.rate.map(|r| r.shown()).as_deref(), Some("50/second"));
                    (r.done, r.failed, r.total, r.finished)
                }
                _ => panic!("only progress"),
            })
            .collect();
        // Started; `a` done (one delivery failed, so the row failed); `b` done; finished.
        assert_eq!(
            seen,
            vec![
                (0, 0, 2, false),
                (1, 1, 2, false),
                (2, 1, 2, false),
                (2, 1, 2, true)
            ]
        );
    }
}
