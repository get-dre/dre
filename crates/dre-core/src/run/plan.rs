//! Planning a run: which Bindings it runs (asking which Set when a person can answer), each
//! one's parse pass with the run's inputs (done once, here, and carried to execution), every
//! profile's entry for the run, and how many Bindings may run at once.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map as JsonMap, Value as Json};

use super::scheduler::Scheduler;
use super::session::ProfileConnections;
use super::{BindingOutcome, RunOptions, RunTargets, Status, UsedProfile};
use crate::codes::Code;
use crate::context::Context;
use crate::engine::{EventSink, RunEvent};
use crate::error::Error;
use crate::parse::ParsedBinding;
use crate::profiles::{Entry, Role};
use crate::project::{Binding, Project, Report};

/// What a run will do: the Bindings to run, the run's target with every profile's entry, and how
/// they're scheduled.
pub struct Plan<'a> {
    pub bindings: Vec<Planned<'a>>,
    pub targets: RunTargets,
    pub(super) scheduler: Scheduler,
}

/// One Binding a run will run, with its parse pass for the run's inputs.
pub struct Planned<'a> {
    pub report: &'a Report,
    pub binding: Binding,
    /// The Binding's vars with the schedule's layered on (`--var` stays separate, on top).
    pub vars: JsonMap<String, Json>,
    pub parsed: ParsedBinding,
}

impl Planned<'_> {
    /// `report` or `report [set]`, for the progress bar.
    pub fn label(&self) -> String {
        match &self.binding.set {
            Some(set) => format!("{} [{set}]", self.report.name),
            None => self.report.name.clone(),
        }
    }
}

/// Which Bindings `opts` selects, each parsed once with the run's inputs, and every profile's
/// entry for the run. Asking which Set to run happens here, through `opts.choose_set`. A
/// selection that fails for one report is that report's failed outcome, sent to `sink` and
/// returned in `refused`; a failure that stops the whole run is the error.
pub fn plan<'a>(
    ctx: &Context,
    project: &'a Project,
    opts: &RunOptions,
    sink: &mut dyn EventSink,
    refused: &mut Vec<BindingOutcome>,
) -> Result<Plan<'a>, Error> {
    if let Err(e) = crate::target::ensure(project.target_dir()) {
        return Err(Error::new(
            Code::InvalidSelector,
            format!(
                "can't write to the target path {} (from {}): {e}",
                project.target_dir().display(),
                project.target_source()
            ),
        ));
    }
    let selected = match &opts.schedule {
        Some(name) => {
            if project.schedule(name).is_none() {
                return Err(Error::new(Code::InvalidSelector, unknown_schedule(project, name)));
            }
            schedule_bindings(project, name, opts).map_err(|e| Error::new(Code::InvalidSelector, e))?
        }
        None => select(project, opts, sink, refused)?,
    };
    let bindings = parse(ctx, project, opts, selected);
    // Every profile the run uses needs an entry for it, before anything runs.
    let targets =
        run_targets(project, &bindings, opts).map_err(|e| Error::new(Code::MissingTargetEntry, e))?;
    let scheduler = schedule(ctx, project, &bindings, opts, sink);
    Ok(Plan {
        bindings,
        targets,
        scheduler,
    })
}

/// The selected reports' Bindings: their Sets resolved first (prompts happen here), so progress
/// has a total.
fn select<'a>(
    project: &'a Project,
    opts: &RunOptions,
    sink: &mut dyn EventSink,
    refused: &mut Vec<BindingOutcome>,
) -> Result<Vec<(&'a Report, Binding)>, Error> {
    let reports: Vec<&Report> = match &opts.selector {
        None => project.reports().iter().collect(),
        Some(s) => match project.select(s) {
            Ok(r) if r.is_empty() => {
                return Err(Error::new(
                    Code::InvalidSelector,
                    format!("selector `{s}` matches no report"),
                ));
            }
            Ok(r) => r,
            Err(e) => return Err(Error::new(Code::InvalidSelector, e.to_string())),
        },
    };
    let mut planned = Vec::new();
    for report in reports {
        match choose_bindings(project, report, opts) {
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
                sink.event(RunEvent::BindingEnd(outcome.clone()));
                refused.push(outcome);
            }
        }
    }
    Ok(planned)
}

/// The parse pass of each selected Binding with this run's inputs, once: its connections, its
/// sources and its destinations' profiles. A Binding whose parse fails reports that when it runs.
fn parse<'a>(
    ctx: &Context,
    project: &'a Project,
    opts: &RunOptions,
    selected: Vec<(&'a Report, Binding)>,
) -> Vec<Planned<'a>> {
    let schedule_vars = opts
        .schedule
        .as_ref()
        .and_then(|n| project.schedule(n))
        .map(|e| e.vars.clone())
        .unwrap_or_default();
    let inputs = inputs(ctx, project, opts, Utc::now());
    selected
        .into_iter()
        .map(|(report, binding)| {
            let mut vars = binding.vars.clone();
            vars.extend(schedule_vars.clone());
            let parsed = crate::parse::binding(project, report, &binding, &vars, &inputs);
            Planned {
                report,
                binding,
                vars,
                parsed,
            }
        })
        .collect()
}

/// The parse pass's inputs for a run started at `started_at`.
pub(super) fn inputs(
    ctx: &Context,
    project: &Project,
    opts: &RunOptions,
    started_at: DateTime<Utc>,
) -> crate::parse::Inputs {
    crate::parse::Inputs {
        target: project.target_name().to_string(),
        cli_vars: opts.vars.clone(),
        date: opts.date,
        scheduled_at: opts.scheduled_at,
        timezone: opts.timezone.clone(),
        schedule: opts.schedule.clone(),
        started_at: Some(started_at),
        process: ctx.clone(),
    }
}

/// How the planned Bindings run: one at a time, in order, unless a connection's entry sets
/// `threads:` above 1 or the run sets `--threads` (`DRE_THREADS`); then each connection runs at
/// most its `threads:` at once (1 on a file only one process may write, as its source plugin's
/// `describe` says), and the run at most `--threads`. Only a run that runs Bindings (`--preview`
/// included) runs them at once.
fn schedule(
    ctx: &Context,
    project: &Project,
    bindings: &[Planned<'_>],
    opts: &RunOptions,
    sink: &mut dyn EventSink,
) -> Scheduler {
    let cap = if opts.mode.concurrent() {
        opts.threads.unwrap_or(usize::MAX).max(1)
    } else {
        1
    };
    if cap == 1 {
        return Scheduler::new(1);
    }
    // Plugin log lines from `describe`, shown before the run starts.
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = {
        let lines = lines.clone();
        std::sync::Arc::new(move |plugin: &str, line: &str| {
            lines.lock().unwrap().push((plugin.to_string(), line.to_string()));
        })
    };
    let connections = ProfileConnections::new(ctx, project, log);
    let mut limits: Vec<(String, usize)> = Vec::new();
    for c in bindings.iter().flat_map(|p| p.parsed.connections()) {
        if !limits.iter().any(|(n, _)| n == c) {
            limits.push((c.to_string(), connections.threads(c)));
        }
    }
    for (plugin, line) in std::mem::take(&mut *lines.lock().unwrap()) {
        sink.event(RunEvent::PluginLog(plugin, line));
    }
    if cap == usize::MAX && limits.iter().all(|(_, n)| *n <= 1) {
        return Scheduler::new(1);
    }
    // No more workers than Bindings could ever run at once.
    let most = if bindings.iter().any(|p| p.parsed.connections().is_empty()) {
        bindings.len()
    } else {
        limits.iter().map(|(_, n)| *n).sum::<usize>().min(bindings.len())
    };
    let mut scheduler = Scheduler::new(cap.min(most));
    for (c, n) in &limits {
        scheduler = scheduler.limit(c, *n);
    }
    scheduler
}

/// The profiles the planned Bindings use (by their parse pass) and each one's entry. A profile
/// without an entry for the run is an error naming them all; profiles nothing planned uses
/// aren't checked.
pub(super) fn run_targets(
    project: &Project,
    planned: &[Planned<'_>],
    opts: &RunOptions,
) -> Result<RunTargets, String> {
    let profiles = project.profiles();
    let mut used: Vec<(Role, String)> = Vec::new();
    for p in planned {
        let names = p
            .parsed
            .connections()
            .into_iter()
            .map(|c| (Role::Connection, c.to_string()))
            .chain(
                p.parsed
                    .destinations
                    .values()
                    .flatten()
                    .filter(|_| opts.mode.renders_destinations())
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
    let all = project.scheduled_bindings(name);
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
            match (pick, &opts.choose_set) {
                (Some(p), _) => report.binding(&p).cloned().into_iter().collect(),
                (None, Some(choose)) => match choose.ask(&report.name, &names)? {
                    Some(p) => report.binding(&p).cloned().into_iter().collect(),
                    None => report.bindings.clone(),
                },
                (None, None) => {
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
