//! How project commands start, and `dre run` from its command line to its summary: install the
//! project's packages and plugins, load and check its selection, resolve each setting
//! (flag, else environment variable, else `dre_project.yml`) and run. The CLI parses flags, calls
//! [`run`] and prints what comes back; everything it shows on the way goes through a [`Console`].

use std::path::PathBuf;
use std::time::Duration;

use crate::context::Context;
use crate::engine::CancelReason;
use crate::engine::EventSink;
use crate::project::{LoadOptions, Project};
use crate::run::{RunOptions, RunSummary};
use crate::{Diagnostic, Diagnostics, Env, Severity};

/// Where a command shows what happens before and around a run, besides the run's own events,
/// which it consumes as their [`EventSink`].
pub trait Console: EventSink {
    /// A package or plugin was installed: `what` names it and its version.
    fn installed(&self, what: &str);
    /// Something stopped the command.
    fn error(&self, message: &str);
    /// A problem found in the project.
    fn diagnostic(&self, d: &Diagnostic);
    /// A detail of the command: a resolved setting (`Setting`), the profiles used (`Profiles`).
    fn note(&self, verb: &str, text: &str);
    /// Start the run's log files, so they record what follows (installing plugins included).
    fn start_logs(&self);
    /// What the run was asked to do, for its log.
    fn params(&self, params: &serde_json::Value);
    /// How long the run may take, counted from when the command started.
    fn timeout(&self, timeout: Option<Duration>);
}

/// The project a command works on, as the command line gave it.
#[derive(Debug, Clone)]
pub struct ProjectRequest {
    pub dir: PathBuf,
    pub load: LoadOptions,
    /// Install the declared packages and plugins that are missing (no `--no-auto-install`).
    pub install: bool,
}

/// Install the macro packages the project at `root` declares but lacks. False after saying why.
pub fn install_packages(root: &std::path::Path, console: &dyn Console) -> bool {
    match crate::packages::sync(root, true, |m| console.installed(m)) {
        Ok(()) => true,
        Err(errors) => {
            for e in errors {
                console.error(&e);
            }
            false
        }
    }
}

/// The start of every project command but `validate` and `deps`: [`install_packages`], then
/// [`load_for_run`].
pub fn load_project(
    ctx: &Context,
    req: &ProjectRequest,
    console: &dyn Console,
) -> Option<(Project, Option<String>)> {
    if req.install && !install_packages(&req.dir, console) {
        return None;
    }
    load_for_run(ctx, req, console)
}

/// Load a whole project for commands such as `compile`, writing its manifest; `None` (after
/// saying why) when it cannot proceed. Also returns the SHA-256 of the manifest written.
pub fn load_for_run(
    ctx: &Context,
    req: &ProjectRequest,
    console: &dyn Console,
) -> Option<(Project, Option<String>)> {
    load_command(ctx, req, console, true)
}

/// Loading for a run leaves the whole-project manifest to `compile` and `validate`.
fn load_command(
    ctx: &Context,
    req: &ProjectRequest,
    console: &dyn Console,
    manifest: bool,
) -> Option<(Project, Option<String>)> {
    let (project, diags) = crate::project::load_with(&req.dir, &req.load, ctx);
    if let Some(p) = &project {
        ctx.secrets().set_enabled(p.mask_secrets());
    }
    let checksum = if manifest {
        let errors = project
            .as_ref()
            .map(|p| crate::manifest::report_errors(p, &diags))
            .unwrap_or_default();
        match write_manifest(ctx, project.as_ref(), &errors, req) {
            Ok(checksum) => checksum,
            Err(error) => {
                console.error(&error);
                return None;
            }
        }
    } else {
        None
    };
    for d in diags.sorted() {
        // Unmanaged reports warn again when they run.
        if d.severity == Severity::Warning && d.code == "unmanaged-report" {
            continue;
        }
        console.diagnostic(d);
    }
    if diags.has_errors() {
        console.error(&has_errors(&diags));
        return None;
    }
    project.map(|p| (p, checksum))
}

/// Write the whole project's manifest into its target folder, or, when the project didn't load,
/// remove a stale one. The error names the target path and where it was set.
pub fn write_manifest(
    ctx: &Context,
    project: Option<&Project>,
    errors: &crate::manifest::ReportErrors,
    req: &ProjectRequest,
) -> Result<Option<String>, String> {
    let Some(p) = project else {
        let root = &req.dir;
        let from_file = crate::target::project_value(root);
        if let Ok(t) = crate::target::resolve(
            root,
            req.load.target_path.as_deref(),
            from_file.as_deref(),
            ctx.env(),
        ) {
            crate::manifest::remove(&t.dir);
        }
        return Ok(None);
    };
    crate::manifest::write(p, errors)
        .map(Some)
        .map_err(|e| format!("{e} (target path from {})", p.target_source()))
}

/// Which plugins a command needs before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedPlugins {
    /// Every declared plugin, installed if missing (unless installing is off: then an error).
    All,
    /// Install what's missing when installing is on; otherwise don't check.
    IfInstalling,
}

/// Install or check the project's plugins, as `need` says. False after saying why.
pub fn ready_plugins(
    ctx: &Context,
    project: &Project,
    install: bool,
    need: NeedPlugins,
    console: &dyn Console,
) -> bool {
    if need == NeedPlugins::IfInstalling && !install {
        return true;
    }
    sync_plugins(ctx, project, install, false, console)
}

/// Install whatever plugins the project declares but lacks (with `install`). False (after
/// saying why) if any is missing and couldn't be installed. With `resolve` (`dre deps`), plugins
/// not pinned in `dre.lock` are resolved against the registry and pinned.
pub fn sync_plugins(
    ctx: &Context,
    project: &Project,
    install: bool,
    resolve: bool,
    console: &dyn Console,
) -> bool {
    let installed = |m: &str| console.installed(m.trim_start_matches("installed "));
    match crate::manager::sync(ctx, project, install, resolve, installed) {
        Ok(_) => true,
        Err(errors) => {
            for e in errors {
                console.error(&e);
            }
            false
        }
    }
}

/// What [`check_project`] checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    /// The project's plugins are declared (`dre compile`).
    PluginUses,
    /// That, then every connection's settings (`dre run`).
    PluginUsesAndOptions,
}

/// Check the project as `checks` says. False after showing the diagnostics, when there are
/// errors.
pub fn check_project(
    ctx: &Context,
    project: &Project,
    install: bool,
    checks: Checks,
    console: &dyn Console,
) -> bool {
    let mut diags = Diagnostics::default();
    check_plugin_uses(ctx, project, install, &mut diags);
    if checks == Checks::PluginUsesAndOptions && !diags.has_errors() {
        crate::options::check(ctx, project, false, &mut diags);
    }
    for d in diags.sorted() {
        console.diagnostic(d);
    }
    if diags.has_errors() {
        console.error(&has_errors(&diags));
        return false;
    }
    true
}

/// Check that the declared plugin packages provide every plugin the project uses, naming the
/// package to add from DRE's registry (unless installing is off, which also means offline).
pub fn check_plugin_uses(ctx: &Context, project: &Project, install: bool, diags: &mut Diagnostics) {
    crate::plugins::check_uses(project, ctx.env(), diags);
    if install {
        crate::manager::explain_undeclared(ctx, diags);
    }
}

/// A count of 1 or more from `flag_name`, else the environment variable `var`; any fallback
/// after that is the caller's.
pub fn count_setting(
    env: &Env,
    flag: Option<&str>,
    flag_name: &'static str,
    var: &'static str,
) -> Result<Option<usize>, String> {
    match crate::settings::flag_or_env(env, flag, flag_name, var) {
        None => Ok(None),
        Some((v, from)) => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => Ok(Some(n)),
            _ => Err(format!("{from} must be a whole number of 1 or more, got `{v}`")),
        },
    }
}

/// What stops a command when the project has errors.
fn has_errors(diags: &Diagnostics) -> String {
    format!(
        "the project has {} error(s); fix them before running (see `dre validate`)",
        diags.error_count()
    )
}

/// `dre run` as the command line gave it.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub project: ProjectRequest,
    /// `--timeout`, `--keep-runs`, `--row-concurrency` and `--threads`, as typed: each falls back
    /// to its environment variable, and `--timeout` and `--keep-runs` to `dre_project.yml`.
    pub timeout: Option<String>,
    pub keep_runs: Option<String>,
    pub row_concurrency: Option<String>,
    pub threads: Option<String>,
    /// Everything else. Its counts, `date`, `scheduled_at` and `manifest_checksum` are set
    /// here.
    pub options: RunOptions,
}

/// How a `dre run` went.
#[derive(Debug)]
pub enum Started {
    /// It never started; the console was told why.
    NotStarted,
    /// It was cancelled before anything ran (the console was told).
    Cancelled(CancelReason),
    /// It ran, with this timeout.
    Ran {
        summary: RunSummary,
        timeout: Option<Duration>,
    },
}

/// `dre run`: get the project ready, resolve the run's settings and run it.
pub fn run(ctx: &Context, mut req: RunRequest, console: &mut dyn Console) -> Started {
    req.project.load.select = req.options.selector.clone();
    req.project.load.schedule = req.options.schedule.clone();
    if req.project.install && !install_packages(&req.project.dir, console) {
        return Started::NotStarted;
    }
    let Some((mut project, manifest_checksum)) = load_command(ctx, &req.project, console, false) else {
        return Started::NotStarted;
    };
    if let Err(error) = project.retain_needed_plugins(ctx, req.project.install) {
        console.error(&error);
        return Started::NotStarted;
    }
    let mut opts = req.options;
    opts.project_errors = project.project_errors().to_vec();
    let count = opts.project_errors.len();
    if count > 0 {
        let reports = if count == 1 { "report has" } else { "reports have" };
        console.diagnostic(&Diagnostic {
            severity: Severity::Warning,
            code: crate::codes::Code::OtherReportErrors,
            message: format!("{count} other {reports} errors; run dre validate"),
            file: None,
            line: None,
            plugin: None,
        });
    }
    if let Some(path) = &opts.output_json {
        let value = path.to_string_lossy();
        if let Err(error) = crate::target::resolve(project.root(), Some(&value), None, ctx.env()) {
            console.error(&format!("--output-json: {error}"));
            return Started::NotStarted;
        }
    }
    // Run logs start before the plugins are installed, so the log files record installing them.
    console.start_logs();
    for s in project.settings().lines() {
        console.note("Setting", &s);
    }
    if !ready_plugins(ctx, &project, req.project.install, NeedPlugins::All, console) {
        return Started::NotStarted;
    }
    let env = ctx.env();
    // `--timeout`, else `DRE_RUN_TIMEOUT`, else `flags: run_timeout`.
    let run_timeout = match crate::settings::flag_or_env(
        env,
        req.timeout.as_deref(),
        "--timeout",
        crate::settings::RUN_TIMEOUT,
    ) {
        Some((v, from)) => match dre_protocol::delivery::parse_duration(&serde_json::Value::String(v)) {
            Ok(d) if !d.is_zero() => Some(d),
            _ => {
                console.error(&format!(
                    "{from} must be a duration such as `2h` or `90m`, or seconds"
                ));
                return Started::NotStarted;
            }
        },
        None => project.run_timeout(),
    };
    // Each flag, else its environment variable; `keep_runs` falls back to `flags: keep_runs`.
    let count = |flag: Option<&str>, name, var| {
        count_setting(env, flag, name, var)
            .map_err(|e| console.error(&e))
            .ok()
    };
    let Some(keep_runs) = count(
        req.keep_runs.as_deref(),
        "--keep-runs",
        crate::settings::KEEP_RUNS,
    ) else {
        return Started::NotStarted;
    };
    let Some(row_concurrency) = count(
        req.row_concurrency.as_deref(),
        "--row-concurrency",
        crate::settings::ROW_CONCURRENCY,
    ) else {
        return Started::NotStarted;
    };
    let Some(threads) = count(req.threads.as_deref(), "--threads", crate::settings::THREADS) else {
        return Started::NotStarted;
    };
    let mut checked_project = project.clone();
    checked_project.retain_loaded_selection();
    if !check_project(
        ctx,
        &checked_project,
        req.project.install,
        Checks::PluginUsesAndOptions,
        console,
    ) {
        return Started::NotStarted;
    }
    opts.keep_runs = keep_runs;
    opts.rows.concurrency = row_concurrency;
    opts.threads = threads;
    opts.date = crate::settings::run_date(env);
    opts.scheduled_at = crate::settings::run_at(env).ok().flatten();
    opts.manifest_checksum = manifest_checksum;
    if let Some(name) = &opts.schedule
        && project.schedule(name).is_none()
    {
        console.error(&crate::run::unknown_schedule(&project, name));
        return Started::NotStarted;
    }
    // Each Binding records its own date, in its own timezone; this line only logs the request.
    let date = opts
        .date
        .or(opts.scheduled_at.map(|t| t.date_naive()))
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let mut params = serde_json::to_value(opts.params(date)).unwrap_or_default();
    params["target"] = serde_json::json!(project.target_name());
    params["profiles"] = serde_json::json!(project.profiles().path);
    params["target_path"] = serde_json::json!(project.target_dir());
    console.params(&params);
    console.note("Profiles", &profiles_line(project.profiles()));
    console.timeout(run_timeout);
    if let Some(reason) = opts.cancel.reason() {
        console.error("the run was cancelled before it started");
        return Started::Cancelled(reason);
    }
    let summary = crate::run::run(ctx, &project, &opts, &mut ConsoleSink(console));
    Started::Ran {
        summary,
        timeout: run_timeout,
    }
}

/// A console as the run's event sink.
struct ConsoleSink<'c>(&'c mut dyn Console);

impl EventSink for ConsoleSink<'_> {
    fn event(&mut self, event: crate::engine::RunEvent) {
        self.0.event(event);
    }
}

/// `profiles.yml` and where it was found, for the `Profiles` note.
pub fn profiles_line(p: &crate::profiles::Profiles) -> String {
    let missing = if p.exists() { "" } else { ", not found" };
    format!("{} (from {}{missing})", p.path.display(), p.found_by)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_one_or_more() {
        let max = usize::MAX.to_string();
        let env = Env::default();
        for flag in ["--keep-runs", "--row-concurrency", "--threads"] {
            let parse = |v: &str| count_setting(&env, Some(v), flag, "DRE_TEST_COUNT_UNSET");
            assert_eq!(parse("1"), Ok(Some(1)));
            assert_eq!(parse(&max), Ok(Some(usize::MAX)));
            assert_eq!(parse(" 4 "), Ok(Some(4)));
            for bad in ["0", "-1", "abc"] {
                assert_eq!(
                    parse(bad),
                    Err(format!("{flag} must be a whole number of 1 or more, got `{bad}`"))
                );
            }
        }
    }

    #[test]
    fn a_count_falls_back_to_its_environment_variable() {
        let env = Env::from_vars([("DRE_THREADS", "3")]);
        assert_eq!(count_setting(&env, None, "--threads", "DRE_THREADS"), Ok(Some(3)));
        assert_eq!(
            count_setting(&env, Some("2"), "--threads", "DRE_THREADS"),
            Ok(Some(2))
        );
    }
}
