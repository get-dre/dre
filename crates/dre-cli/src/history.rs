//! `dre history`: a report's runs (and the current one) in `target/run/`; `dre unlock`: clear a
//! Binding's lock left by a run that's gone.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use dre_core::runs::BindingRuns;
use serde_json::{Value, json};

use crate::exit;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HistoryOutput {
    Text,
    Json,
}

#[derive(Args)]
pub struct HistoryArgs {
    /// The report.
    report: String,
    /// Only this Binding: a Set's name, or `default` for a report without Sets.
    #[arg(long)]
    binding: Option<String>,
    /// Only the current run (the latest that finished) of each Binding.
    #[arg(long)]
    latest: bool,
    /// Print only the runs' folders, one per line (with --latest: where the latest files are).
    #[arg(long)]
    path: bool,
    /// `text` for people, `json` for scripts.
    #[arg(long, value_enum, default_value = "text")]
    output: HistoryOutput,
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// The target path (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then
    /// target/).
    #[arg(long, value_name = "PATH")]
    target_path: Option<String>,
}

#[derive(Args)]
pub struct UnlockArgs {
    /// The report.
    report: String,
    /// The Binding: a Set's name, or `default` (the default) for a report without Sets.
    #[arg(long, default_value = "default")]
    binding: String,
    /// Don't ask for confirmation (for scripts).
    #[arg(long)]
    yes: bool,
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// The target path (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then
    /// target/).
    #[arg(long, value_name = "PATH")]
    target_path: Option<String>,
}

/// `<target path>/run/<report>`.
fn report_dir(project_dir: &Path, target_path: Option<&str>, report: &str) -> Result<PathBuf, String> {
    let from_file = dre_core::target::project_value(project_dir);
    let t = dre_core::target::resolve(project_dir, target_path, from_file.as_deref())?;
    Ok(t.dir.join("run").join(report))
}

fn shown(project_dir: &Path, p: &Path) -> String {
    let p = p.strip_prefix(project_dir).unwrap_or(p);
    dre_core::slash(p).display().to_string()
}

pub fn history(a: HistoryArgs) -> ExitCode {
    let dir = match report_dir(&a.project_dir, a.target_path.as_deref(), &a.report) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::not_started();
        }
    };
    let mut bindings: Vec<String> = match &a.binding {
        Some(b) => vec![b.clone()],
        None => std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
    };
    bindings.sort();
    let mut rows: Vec<Value> = Vec::new();
    for b in &bindings {
        let runs = BindingRuns::new(&dir.join(b));
        let current = runs.current();
        let mut ids = runs.list();
        ids.reverse();
        if a.latest {
            ids.retain(|id| Some(id) == current.as_ref());
        }
        for id in ids {
            let folder = runs.run_dir(&id);
            let results: Value = std::fs::read_to_string(folder.join("run_results.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            rows.push(json!({
                "report": a.report,
                "binding": b,
                "run_id": id,
                "current": Some(&id) == current.as_ref(),
                // No run_results.json: the run never finished (it crashed or is still going).
                "status": results.get("status").cloned().unwrap_or(json!("unfinished")),
                "started_at": results.get("started_at").cloned().unwrap_or(Value::Null),
                "scheduled_at": results.get("scheduled_at").cloned().unwrap_or(Value::Null),
                "duration_ms": results.get("duration_ms").cloned().unwrap_or(Value::Null),
                "error": results.get("error").cloned().unwrap_or(Value::Null),
                "path": shown(&a.project_dir, &folder),
            }));
        }
        if let Some(h) = runs.holder() {
            eprintln!("note: {} {b} is locked by {}", a.report, h.describe());
        }
    }
    if rows.is_empty() {
        eprintln!(
            "error: no runs of `{}`{} in {}",
            a.report,
            a.binding
                .as_ref()
                .map(|b| format!(" (Binding `{b}`)"))
                .unwrap_or_default(),
            shown(&a.project_dir, &dir)
        );
        return exit::failed();
    }
    if a.path {
        for r in &rows {
            println!("{}", r["path"].as_str().unwrap_or_default());
        }
    } else if a.output == HistoryOutput::Json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap());
    } else {
        for r in &rows {
            let mark = if r["current"] == true { "*" } else { " " };
            println!(
                "{mark} {:<10} {:<24} {:<12} {}",
                r["binding"].as_str().unwrap_or_default(),
                r["run_id"].as_str().unwrap_or_default(),
                r["status"].as_str().unwrap_or_default(),
                r["path"].as_str().unwrap_or_default()
            );
        }
        println!("(* the current run)");
    }
    exit::ok()
}

pub fn unlock(a: UnlockArgs) -> ExitCode {
    let dir = match report_dir(&a.project_dir, a.target_path.as_deref(), &a.report) {
        Ok(d) => d.join(&a.binding),
        Err(e) => {
            eprintln!("error: {e}");
            return exit::not_started();
        }
    };
    let runs = BindingRuns::new(&dir);
    let lock = dir.join(dre_core::runs::LOCK);
    if !lock.exists() {
        eprintln!("{} {} isn't locked", a.report, a.binding);
        return exit::ok();
    }
    match runs.holder() {
        Some(h) => eprintln!("{} {} is locked by {}", a.report, a.binding, h.describe()),
        None => eprintln!(
            "{} {} has an unreadable lock at {}",
            a.report,
            a.binding,
            lock.display()
        ),
    }
    if !a.yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("error: not removing the lock without confirmation; pass --yes");
            return exit::not_started();
        }
        eprint!("Remove it? Only do this if that run is no longer going on. [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            eprintln!("Left it in place.");
            return exit::failed();
        }
    }
    match runs.unlock() {
        Ok(_) => {
            eprintln!("Removed the lock.");
            exit::ok()
        }
        Err(e) => {
            eprintln!("error: can't remove {}: {e}", lock.display());
            exit::failed()
        }
    }
}
