#[cfg(test)]
mod cli_reference;
mod exit;
mod extensions;
mod history;
mod init;
mod ls;
mod output;
mod plugins;
mod progress;
mod schedule;
mod signals;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use dre_core::codes::Code;
use dre_core::project::{self, LoadOptions};
use dre_core::settings;

#[derive(Parser)]
#[command(name = "dre", version = dre_core::version(), about = "DRE, the Declarative Reporting Engine: SQL in, formatted files out")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Show every step (same as `--log-level debug`).
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Only show errors and the final summary.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// How much to show.
    #[arg(long, global = true, value_enum)]
    log_level: Option<output::Verbosity>,
    /// `text` for people, `json` (one object per line) for CI and tooling.
    #[arg(long, global = true, value_enum, default_value = "text")]
    log_format: output::LogFormat,
    /// Colour output: auto (default; off when NO_COLOR is set or output isn't a terminal),
    /// always or never.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: output::ColorChoice,
}

impl Cli {
    fn printer(&self) -> output::Printer {
        let v = self.log_level.unwrap_or(if self.verbose {
            output::Verbosity::Debug
        } else if self.quiet {
            output::Verbosity::Quiet
        } else {
            output::Verbosity::Info
        });
        output::Printer::new(v, self.log_format, self.color)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Check the project (config, references, templates, schedules) and compile its SQL.
    Validate(ValidateArgs),
    /// Run reports: render, execute, format into the target folder, and deliver.
    Run(Box<RunArgs>),
    /// Render reports' SQL into <target path>/compiled/ without running it, and list the files.
    Compile(CompileArgs),
    /// Run one read-only query on a configured connection and write typed JSON rows.
    Query(QueryArgs),
    /// Read live table/column definitions and comments through a configured connection.
    Catalog(CatalogArgs),
    /// Hand arguments to the companion that provides `dre mcp`, offering to install it.
    Mcp(extensions::McpArgs),
    /// Install or update companion binaries, separately from source/format/destination plugins.
    #[command(subcommand)]
    Extension(extensions::ExtensionCommand),
    /// Remove the target folder (compiled SQL, run outputs, schema snapshots, the manifest).
    Clean(CleanArgs),
    /// List the reports and Bindings a selection or schedule covers, without running anything.
    Ls(ls::LsArgs),
    /// Work out when schedules fire, for people and orchestrators.
    #[command(subcommand)]
    Schedule(schedule::ScheduleCommand),
    /// Set up a connection (installing its plugin) and optionally a starter project, interactively.
    Init(InitArgs),
    /// Create a starter project in a new directory.
    New(NewArgs),
    /// Install the project's declared plugins (pinned by dre.lock) without running anything.
    Deps(DepsArgs),
    /// Manage plugins (sources, formats, destinations).
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Commands about DRE itself rather than a project.
    #[command(subcommand)]
    System(system::SystemCommand),
    /// Explain an error code (`dre explain unknown-key`): what it means and how to fix it.
    Explain(ExplainArgs),
    /// A report's runs in the target path, newest first, and which is current (the latest
    /// finished); `--latest --path` prints where the latest files are.
    History(history::HistoryArgs),
    /// Remove a Binding's lock left by a run that's no longer going on (it shows the holder and
    /// asks first).
    Unlock(history::UnlockArgs),
    /// Deliver a run's held deliveries (`hold: true`, `dre run --hold`) from its run folder,
    /// byte for byte what was rendered; never re-renders or re-queries, and never sends twice.
    Release(ReleaseArgs),
}

#[derive(Args)]
struct ReleaseArgs {
    /// The report.
    report: String,
    /// The run whose held deliveries to deliver (its id, as `dre history` lists it).
    #[arg(long = "run", value_name = "RUN_ID")]
    run: String,
    /// The Binding: the Set's name, or `default` for a report without Sets.
    #[arg(long, default_value = "default")]
    binding: String,
    /// Release only these rows of a per-row output (keys, comma-separated).
    #[arg(long, value_name = "KEYS", value_delimiter = ',')]
    only_keys: Option<Vec<String>>,
    /// Release every held delivery but these rows' (keys, comma-separated).
    #[arg(long, value_name = "KEYS", value_delimiter = ',')]
    exclude_keys: Option<Vec<String>>,
    /// Who releases, recorded in the release's run_results.json (default: $USER).
    #[arg(long, value_name = "NAME")]
    by: Option<String>,
    /// Pace the deliveries at this rate, over each one's recorded `rate:`: a count and a unit,
    /// like `100/minute`, `5/second` or `20000/hour`, spaced evenly.
    #[arg(long, value_name = "N/UNIT", value_parser = parse_rate)]
    rate: Option<dre_core::rate::Rate>,
    #[command(flatten)]
    project: ProjectArgs,
}

#[derive(Args)]
struct ExplainArgs {
    /// The code, as in `error[unknown-key]`.
    code: String,
}

#[derive(Args)]
struct QueryArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// The configured source connection.
    #[arg(long)]
    profile: String,
    /// File containing one read-only SQL query (relative to the project directory).
    #[arg(long)]
    sql_file: PathBuf,
    #[command(flatten)]
    output: OutputFile,
    /// Maximum result rows. The result records whether more rows were available.
    #[arg(long, default_value_t = 1000)]
    max_rows: u64,
}

#[derive(Args)]
struct CatalogArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// The source connection to inspect.
    #[arg(long)]
    profile: String,
    /// JSON object with `tables` and `schemas` arrays of relation names.
    #[arg(long)]
    tables_file: PathBuf,
    #[command(flatten)]
    output: OutputFile,
}

/// Where `dre query` and `dre catalog` write their JSON.
#[derive(Args)]
struct OutputFile {
    /// The JSON file to write, relative to the project directory.
    #[arg(long)]
    output_file: PathBuf,
}

#[derive(Args)]
struct RunArgs {
    /// What to run: report names, `tag:<tag>`, folder names or dotted folder paths
    /// (`dre run a b` runs both). Runs every report when omitted.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Run one Set (declared or ad hoc), or `all` of a report's Sets.
    #[arg(long)]
    set: Option<String>,
    /// Run the Bindings a schedules.yml entry targets, with its vars. Pass the scheduled
    /// instant through DRE_RUN_AT (or the date through DRE_RUN_DATE) so reruns render the same.
    /// With a selector and/or --set, run just those of its Bindings.
    #[arg(long, value_name = "NAME")]
    schedule: Option<String>,
    /// Use this connection instead of the inherited one (report, Set, folder `+profile`,
    /// `default_profile`), e.g. for an ad hoc Set. A query's own `profile:` or a source's wins.
    #[arg(long)]
    profile: Option<String>,
    /// Override the output file name for this run (the first output, with several).
    #[arg(long)]
    output_name: Option<String>,
    /// Override the full output (delivery) path for this run (the first output, with several).
    #[arg(long)]
    output_path: Option<String>,
    /// Render SQL into target/compiled/ and stop; no report query is executed.
    #[arg(long)]
    dry_run: bool,
    /// Keep normal outputs locally without delivering them to any destination.
    #[arg(long)]
    no_deliver: bool,
    /// Write typed JSON rows per query and isolate this run's outputs/history in DIR.
    #[arg(long, value_name = "DIR", conflicts_with = "dry_run")]
    output_json: Option<PathBuf>,
    /// Re-render the outputs from results saved with `--output-json DIR`, without running any
    /// query or opening a connection; implies --no-deliver. For designing a template.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["dry_run", "output_json", "preview"])]
    from_results: Option<PathBuf>,
    /// Execute with a row limit (default 100); output stays in target/ and is never delivered.
    #[arg(long, value_name = "ROWS", num_args = 0..=1, default_missing_value = "100")]
    preview: Option<u64>,
    /// Deliver even if the output schema changed since the last successful run, and accept
    /// the new schema. Snapshots live in the target path, so a fresh CI runner has no history
    /// unless `--target-path` (or DRE_TARGET_PATH) points at a folder that persists.
    #[arg(long)]
    accept_schema_change: bool,
    /// Stop the run if it takes longer than this: a duration such as `2h` or `90m`, or seconds
    /// (default: $DRE_RUN_TIMEOUT, then `flags: run_timeout` in dre_project.yml; off without
    /// any). Its Bindings are then recorded as `timed_out`, and `dre` exits 124.
    #[arg(long, value_name = "DURATION")]
    timeout: Option<String>,
    /// How many runs of each report and Set to keep in the target path, the current one included
    /// (default: $DRE_KEEP_RUNS, then `flags: keep_runs` in dre_project.yml, then 1).
    #[arg(long, value_name = "N")]
    keep_runs: Option<String>,
    /// How many Bindings may run at once in this run (default: $DRE_THREADS; without it, each
    /// connection entry's `threads:`, default 1). `--threads 1` runs them one at a time.
    #[arg(long, value_name = "N")]
    threads: Option<String>,
    /// How many rows of a per-row (`for_each`) output render and deliver at once, over
    /// `for_each.concurrency` (default: $DRE_ROW_CONCURRENCY, then `for_each.concurrency`, then 4).
    #[arg(long, value_name = "N")]
    row_concurrency: Option<String>,
    /// Hold every delivery: render and record them, and deliver them later with `dre release`.
    #[arg(long, conflicts_with_all = ["dry_run", "preview", "no_deliver"])]
    hold: bool,
    /// Redo a run's failed and never-started rows of its per-row outputs, from the query results
    /// it saved, with no query; other outputs are skipped. The new run doesn't become current.
    #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["dry_run", "from_results", "output_json"])]
    retry_failed: Option<String>,
    /// Make only these rows of each per-row output (keys, comma-separated); recorded as partial.
    #[arg(long, value_name = "KEYS", value_delimiter = ',')]
    only_keys: Option<Vec<String>>,
    /// Make at most this many rows of each per-row output; recorded as partial.
    #[arg(long, value_name = "N")]
    limit_rows: Option<usize>,
    /// With --preview: show per-row values (addresses, paths) as they are, not masked.
    #[arg(long, requires = "preview")]
    unmask: bool,
    /// Pace every destination at this rate for this run, over its `rate:`: a count and a unit,
    /// like `100/minute`, `5/second` or `20000/hour`, spaced evenly.
    #[arg(long, value_name = "N/UNIT", value_parser = parse_rate)]
    rate: Option<dre_core::rate::Rate>,
}

#[derive(Args)]
struct CleanArgs {
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Don't delete the folder: only remove the runs beyond `keep_runs` (and unfinished ones)
    /// from every report and Set, keeping each current run.
    #[arg(long)]
    prune: bool,
    /// With --prune, keep this many runs of each (default: $DRE_KEEP_RUNS, then `flags:
    /// keep_runs`, then 1).
    #[arg(long, value_name = "N", requires = "prune")]
    keep_runs: Option<String>,
    /// The folder to clean (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml,
    /// then target/). Only a folder DRE created is deleted.
    #[arg(long, value_name = "PATH")]
    target_path: Option<String>,
}

#[derive(Subcommand)]
enum PluginCommand {
    /// List installed plugins, with each one's version and protocol version.
    List,
    /// Install a plugin from the registry: `name`, `kind/name`, optionally `@<version req>`.
    Install(PluginArgs),
    /// Install the newest version allowed by the constraint (ignoring dre.lock's pin) and re-pin it.
    Update(PluginArgs),
    /// Remove installed versions of a plugin (`name@version` removes just one).
    Remove(PluginArgs),
}

#[derive(Args)]
struct PluginArgs {
    /// `duckdb`, `source/duckdb`, `xlsx@^1`, ...
    plugin: String,
    /// Project whose dre.lock to update (default: the current directory, if it's a project).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
}

#[derive(Args)]
struct InitArgs {
    /// Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then ~/.dre).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
}

#[derive(Args)]
struct NewArgs {
    /// Directory to create (must be missing or empty).
    dir: PathBuf,
    /// The connection profile the project uses by default.
    #[arg(long, default_value = "warehouse")]
    profile: String,
    /// The plugin of that connection (its `type`), which the project declares. `--source` is the
    /// 0.1 name.
    #[arg(long = "type", alias = "source", default_value = "duckdb")]
    plugin: String,
}

#[derive(Args)]
struct DepsArgs {
    /// Install the project's declared companions without asking, after reviewing their licences.
    #[arg(long)]
    accept_proprietary: bool,
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
}

#[derive(Args)]
struct ProjectArgs {
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then the project directory
    /// if it has a profiles.yml, then ~/.dre).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
    /// Fail instead of installing declared plugins that are missing.
    #[arg(long)]
    no_auto_install: bool,
    /// The run's target (environment), `target.name` in templates: every profile uses its entry
    /// for it (default: $DRE_TARGET; without either, each profile uses its own `target:`, else
    /// `dev`).
    #[arg(long)]
    target: Option<String>,
    /// Where DRE writes its generated files (default: $DRE_TARGET_PATH, then `target_path` in
    /// dre_project.yml, then target/). A local or mounted path, absolute or relative to the
    /// project root. Unrelated to `--target`.
    #[arg(long, value_name = "PATH")]
    target_path: Option<String>,
    /// Set a variable for `var()`, overriding every other level: `--var name=value`. The value is
    /// YAML 1.2 (`false`, `5`, `[a, b]` are typed); quote it to keep text: `--var x='"false"'`.
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = parse_var)]
    vars: Vec<(String, String)>,
    /// The run's timezone (IANA name, e.g. Australia/Sydney), above every `timezone:` setting
    /// (default: $DRE_TIMEZONE).
    #[arg(long)]
    timezone: Option<String>,
}

impl ProjectArgs {
    /// `--timezone`, else `DRE_TIMEZONE`.
    fn timezone(&self) -> Option<String> {
        settings::flag_or_env(self.timezone.as_deref(), "--timezone", settings::TIMEZONE).map(|(v, _)| v)
    }

    fn load_options(&self) -> LoadOptions {
        LoadOptions {
            profiles_dir: self.profiles_dir.clone(),
            target: self.target.clone(),
            vars: self.vars.iter().cloned().collect(),
            target_path: self.target_path.clone(),
            date: run_date(),
            scheduled_at: run_at().ok().flatten(),
            timezone: self.timezone(),
            settings: settings::run_settings(self.timezone.as_deref()),
        }
    }
}

pub(crate) fn parse_var(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err(format!("expected NAME=VALUE, got `{s}`")),
    }
}

#[derive(Args)]
struct ValidateArgs {
    /// Which reports to compile and check (same selectors as `dre run`; default: all). With a
    /// selector, validate also shows where each selected Binding's output would go.
    /// Reports whose templates query the database (`run_query()`, `columns()`) connect, and
    /// may sign in, to compile.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Emit machine-readable JSON instead of text.
    #[arg(long)]
    json: bool,
    /// After the offline checks, connect to each Binding's source and check every rendered
    /// statement without executing it (EXPLAIN or the dialect's equivalent).
    #[arg(long)]
    live: bool,
    /// Compile (and with --live, check) one Set instead of every Set.
    #[arg(long)]
    set: Option<String>,
    /// Treat warnings as errors: exit 1 when there are any.
    #[arg(long)]
    strict: bool,
    /// Check the connection settings of every entry in profiles.yml, not only the entries the
    /// selected reports would use with these flags.
    #[arg(long)]
    all_targets: bool,
}

#[derive(Args)]
struct CompileArgs {
    /// What to compile: report names, `tag:<tag>`, folder names or dotted folder paths.
    /// Compiles every report when omitted.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Compile one Set (declared or ad hoc), or `all` of a report's Sets.
    #[arg(long)]
    set: Option<String>,
}

fn main() -> ExitCode {
    dre_protocol::host::set_core_version(dre_core::version());
    let cli = Cli::parse();
    let printer = cli.printer();
    let project_args = match &cli.command {
        Command::Validate(a) => Some(&a.project),
        Command::Run(a) => Some(&a.project),
        Command::Compile(a) => Some(&a.project),
        _ => None,
    };
    if let Some(t) = project_args.and_then(ProjectArgs::timezone)
        && let Err(e) = dre_core::dates::parse_tz(&t)
    {
        let from = if project_args.is_some_and(|p| p.timezone.is_some()) {
            "--timezone"
        } else {
            "DRE_TIMEZONE"
        };
        printer.error(&format!("{from}: {e}"));
        return exit::not_started();
    }
    if project_args.is_some()
        && let Err(e) = run_at()
    {
        printer.error(&e);
        return exit::not_started();
    }
    match cli.command {
        Command::Explain(a) => explain(&a.code),
        Command::History(a) => history::history(a),
        Command::Unlock(a) => history::unlock(a),
        Command::Release(a) => release(a, printer),
        Command::Validate(a) => validate(a, &printer),
        Command::Run(a) => run(*a, printer),
        Command::Compile(a) => compile(a, printer),
        Command::Query(a) => query(a, printer),
        Command::Catalog(a) => catalog(a, printer),
        Command::Mcp(a) => extensions::handoff("mcp", a, &printer),
        Command::Extension(a) => extensions::manage(a, &printer),
        Command::Clean(a) => clean(a),
        Command::Ls(a) => ls::ls(a),
        Command::Schedule(schedule::ScheduleCommand::Ls(a)) => schedule::ls(a),
        Command::System(system::SystemCommand::Update(a)) => system::update(a, &printer),
        Command::Deps(a) => deps(a, &printer),
        Command::Init(a) => init::init(a.profiles_dir, &printer),
        Command::New(a) => init::new(a.dir, a.profile, a.plugin, &printer),
        Command::Plugin(PluginCommand::List) => plugin_list(),
        Command::Plugin(PluginCommand::Install(a)) => {
            plugins::install(a.plugin, a.project_dir, false, &printer)
        }
        Command::Plugin(PluginCommand::Update(a)) => {
            plugins::install(a.plugin, a.project_dir, true, &printer)
        }
        Command::Plugin(PluginCommand::Remove(a)) => plugins::remove(a.plugin, a.project_dir, &printer),
    }
}

/// `dre explain <code>`: the registry's explanation of a code.
fn explain(code: &str) -> ExitCode {
    let code = code
        .trim()
        .trim_start_matches("error[")
        .trim_start_matches("warning[")
        .trim_end_matches(']');
    if let Some(c) = dre_core::codes::Code::parse(code) {
        println!(
            "{code} ({})\n\n{}\n\n{}\n\n{}#{code}",
            c.kind(),
            c.summary(),
            c.explanation(),
            dre_core::codes::REFERENCE_URL
        );
        return exit::ok();
    }
    if let Some((plugin, _)) = code.split_once('/') {
        println!(
            "`{code}` is a code of the `{plugin}` plugin; see its page in the plugins reference: https://getdre.com/docs/plugins/"
        );
        return exit::ok();
    }
    let near: Vec<&str> = dre_core::codes::Code::ALL
        .iter()
        .map(|c| c.as_str())
        .filter(|s| s.contains(code) || code.contains(s))
        .collect();
    let hint = if near.is_empty() {
        format!("; every code is listed at {}", dre_core::codes::REFERENCE_URL)
    } else {
        format!(
            "; did you mean {}?",
            near.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    eprintln!("error: no code `{code}`{hint}");
    exit::not_started()
}

fn validate(a: ValidateArgs, printer: &output::Printer) -> ExitCode {
    // A failed or skipped install isn't fatal here: the load reports a missing package with
    // the rest.
    install_packages(&a.project, printer);
    let selector = selection(&a.select, &a.selector);
    let (project, mut diags) = project::load(&a.project.project_dir, &a.project.load_options());
    // validate checks every report, so templates that don't render are its errors too.
    for d in project.iter().flat_map(|p| p.parse_errors.values().flatten()) {
        diags.push(d.clone());
    }
    let report_errors = project
        .as_ref()
        .map(|p| dre_core::manifest::report_errors(p, &diags))
        .unwrap_or_default();
    if let Some(p) = &project {
        dre_core::secrets::set_enabled(p.mask_secrets);
    }
    if let Err(e) = write_manifest(project.as_ref(), &report_errors, &a.project) {
        diags.error(Code::TargetPathUnwritable, None, None, e);
    }
    if let Some(p) = &project {
        // Plugins aren't installed when the project already has errors, so a missing one is then
        // only a warning; the option checks run either way, so every problem shows in one pass.
        let offline = a.project.no_auto_install || diags.has_errors();
        plugins::check_for_validate(p, !a.project.no_auto_install, &mut diags, printer);
        check_plugin_uses(p, &a.project, &mut diags);
        dre_core::options::check(p, offline, &mut diags);
    }
    // Only a project that checks out gets compiled.
    let (plans, targets) = match &project {
        Some(p) if !diags.has_errors() => {
            compile_for_validate(p, selector.as_deref(), &a.set, &a.project, &mut diags)
        }
        _ => (Vec::new(), None),
    };
    if let Some(w) = targets.as_ref().and_then(dre_core::run::RunTargets::mismatch) {
        diags.warning(Code::TargetMismatch, None, None, w);
    }
    // Each profile entry's settings, checked by its plugin without connecting: the entries the
    // run would use, or with `--all-targets` every entry.
    if let Some(p) = &project {
        let entries = if a.all_targets {
            dre_core::options::all_entries(p)
        } else {
            targets
                .iter()
                .flat_map(|t| &t.profiles)
                .filter(|u| u.deliver)
                .map(|u| {
                    let role = if u.role == "connection" {
                        dre_core::profiles::Role::Connection
                    } else {
                        dre_core::profiles::Role::Destination
                    };
                    (role, u.profile.clone(), u.target.clone())
                })
                .collect()
        };
        if a.all_targets || !diags.has_errors() {
            dre_core::options::check_connections(p, &entries, &mut diags);
        }
    }
    // `--strict`: warnings count as errors.
    let ok = !diags.has_errors() && !(a.strict && diags.warning_count() > 0);
    if a.json {
        let out = serde_json::json!({
            "ok": ok,
            "profiles": project.as_ref().map(|p| serde_json::json!({
                "path": p.profiles.path,
                "exists": p.profiles.exists(),
                "found_by": p.profiles.found_by,
            })),
            "target": project.as_ref().map(|p| serde_json::json!({
                "name": p.target_name,
                "from": p.target_from.to_string(),
                "profiles": targets.as_ref().map(|t| t.profiles.clone()).unwrap_or_default(),
            })),
            "settings": project.as_ref().map(|p| &p.settings),
            "errors": diags.error_count(),
            "warnings": diags.warning_count(),
            "diagnostics": diags.sorted(),
            "compiled": plans,
            "project": project.as_ref().map(|p| dre_core::manifest::build(p, &report_errors)),
        });
        println!(
            "{}",
            dre_core::secrets::to_json_pretty(&out).expect("a JSON value serializes")
        );
    } else {
        for d in diags.sorted() {
            printer.diag(d);
        }
        if selector.is_some() {
            for p in &plans {
                printer.plan(p);
            }
        } else if !plans.is_empty()
            && let Some(p) = &project
        {
            println!(
                "Compiled {} Binding{} into {}/",
                plans.len(),
                plural(plans.len()),
                shown(p, &p.target_dir.join("compiled")).display()
            );
        }
        if let Some(p) = &project {
            printer.line(output::Tone::Note, "Profiles", &profiles_line(&p.profiles));
            let line = targets.as_ref().map_or_else(|| target_line(p), |t| t.line());
            printer.line(output::Tone::Note, "Target", &line);
            for s in p.settings.lines() {
                printer.detail(output::Tone::Note, "Setting", &s);
            }
        }
        let (e, w) = (diags.error_count(), diags.warning_count());
        let verdict = if ok { "passed" } else { "failed" };
        println!(
            "Validation {verdict}: {e} error{}, {w} warning{}",
            plural(e),
            plural(w)
        );
    }
    if project.is_none() {
        // No project to check: missing or unreadable dre_project.yml.
        return exit::not_started();
    }
    if !ok {
        return exit::failed();
    }
    if a.live
        && let Some(project) = project
    {
        return validate_live(&project, selector, a.set, &a.project, printer.clone());
    }
    exit::ok()
}

/// Compiles quietly, collecting what each Binding would do.
#[derive(Default)]
struct Collect {
    plans: Vec<dre_core::run::BindingPlan>,
    targets: Option<dre_core::run::RunTargets>,
}

impl dre_core::run::Ui for Collect {
    fn step(&mut self, _: dre_core::run::Level, _: &str, _: &str, _: Option<std::time::Duration>) {}
    fn warn(&mut self, _: &str) {}
    fn targets(&mut self, t: &dre_core::run::RunTargets) {
        self.targets = Some(t.clone());
    }
    fn compiled(&mut self, plan: &dre_core::run::BindingPlan) {
        self.plans.push(plan.clone());
    }
    fn choose_set(&mut self, _: &str, _: &[String]) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn plugin_log(&self) -> dre_protocol::host::LogSink {
        std::sync::Arc::new(|_, _| {})
    }
}

/// `validate` compiles the selection (every Set of every report by default); a Binding that
/// doesn't render is an error.
fn compile_for_validate(
    project: &dre_core::project::Project,
    selector: Option<&str>,
    set: &Option<String>,
    p: &ProjectArgs,
    diags: &mut dre_core::Diagnostics,
) -> (Vec<dre_core::run::BindingPlan>, Option<dre_core::run::RunTargets>) {
    let opts = dre_core::run::RunOptions {
        selector: selector.map(str::to_string),
        set: Some(set.clone().unwrap_or_else(|| "all".into())),
        target: p.target.clone(),
        vars: p.vars.iter().cloned().collect(),
        date: run_date(),
        scheduled_at: run_at().ok().flatten(),
        timezone: p.timezone(),
        dry_run: true,
        ..Default::default()
    };
    let mut ui = Collect::default();
    let summary = dre_core::run::run(project, &opts, &mut ui);
    if let Some(e) = summary.error {
        let code = if summary.missing_entry {
            Code::MissingTargetEntry
        } else {
            Code::InvalidSelector
        };
        diags.error(code, None, None, e);
    }
    for o in summary
        .outcomes
        .iter()
        .filter(|o| o.status == dre_core::run::Status::Error)
    {
        let set = o.set.as_ref().map(|s| format!(", Set `{s}`")).unwrap_or_default();
        let err = o.error.clone().unwrap_or_default();
        // A template that queries what an earlier query makes (a temp table) can't render
        // without running that query; validate runs nothing, so that's for `dre run` to check.
        if err.contains("run_query() failed:") || (err.contains("`columns('") && err.contains("')` failed:"))
        {
            diags.warning(
                Code::CompileNeedsRun,
                None,
                None,
                format!(
                    "report `{}`{set} queries the database while rendering and can only be checked by `dre run`: {err}",
                    o.report
                ),
            );
            continue;
        }
        diags.error(
            Code::CompileFailed,
            None,
            None,
            format!(
                "report `{}`{set} doesn't compile: {}",
                o.report,
                o.error.clone().unwrap_or_default()
            ),
        );
    }
    (ui.plans, ui.targets)
}

fn catalog(a: CatalogArgs, printer: output::Printer) -> ExitCode {
    json_command(&a.project, a.output, &printer, |project| {
        let request =
            std::fs::read_to_string(project.root.join(&a.tables_file)).map_err(|e| e.to_string())?;
        let request =
            serde_json::from_str(&request).map_err(|e| format!("invalid catalogue request: {e}"))?;
        dre_core::catalog::build(project, &a.profile, &request)
    })
}

fn query(a: QueryArgs, printer: output::Printer) -> ExitCode {
    json_command(&a.project, a.output, &printer, |project| {
        let sql = std::fs::read_to_string(project.root.join(&a.sql_file)).map_err(|e| e.to_string())?;
        dre_core::query::execute(project, &a.profile, &sql, a.max_rows)
    })
}

/// `dre query` and `dre catalog`: load the project, build one JSON document, write it.
fn json_command(
    args: &ProjectArgs,
    output: OutputFile,
    printer: &output::Printer,
    build: impl FnOnce(&dre_core::project::Project) -> Result<serde_json::Value, String>,
) -> ExitCode {
    let Some((project, _)) = load_project(args, printer) else {
        return exit::not_started();
    };
    let file = output.output_file;
    let result = build(&project).and_then(|value| {
        let path = project.root.join(&file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        dre_core::fs::write_atomic(
            &path,
            serde_json::to_vec(&value)
                .expect("a JSON value serializes")
                .as_slice(),
        )
        .map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            printer.error(&error);
            exit::not_started()
        }
    }
}

/// `dre compile`: render the selection into target/compiled/ and list the files.
fn compile(a: CompileArgs, mut printer: output::Printer) -> ExitCode {
    let Some((project, manifest_checksum)) = load_project(&a.project, &printer) else {
        return exit::not_started();
    };
    if !ready_plugins(&project, &a.project, NeedPlugins::IfInstalling, &printer)
        || !check_project(&project, &a.project, &printer, Checks::PluginUses)
    {
        return exit::not_started();
    }
    let opts = dre_core::run::RunOptions {
        selector: selection(&a.select, &a.selector),
        set: a.set,
        target: a.project.target.clone(),
        vars: a.project.vars.iter().cloned().collect(),
        date: run_date(),
        scheduled_at: run_at().ok().flatten(),
        timezone: a.project.timezone(),
        dry_run: true,
        interactive: {
            use std::io::IsTerminal;
            std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
        },
        manifest_checksum,
        ..Default::default()
    };
    let summary = dre_core::run::run(&project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::from(summary.exit_code());
    }
    printer.finish("compile");
    ExitCode::from(summary.exit_code())
}

fn validate_live(
    project: &dre_core::project::Project,
    selector: Option<String>,
    set: Option<String>,
    p: &ProjectArgs,
    mut printer: output::Printer,
) -> ExitCode {
    let opts = dre_core::run::RunOptions {
        selector,
        set,
        target: p.target.clone(),
        vars: p.vars.iter().cloned().collect(),
        date: run_date(),
        scheduled_at: run_at().ok().flatten(),
        timezone: p.timezone(),
        live_check: true,
        ..Default::default()
    };
    let summary = dre_core::run::run(project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::from(summary.exit_code());
    }
    printer.finish("validate --live");
    ExitCode::from(summary.exit_code())
}

/// `-s` values (joined: space means union) or the positional selector.
fn selection(select: &[String], positional: &[String]) -> Option<String> {
    let all = if select.is_empty() { positional } else { select };
    (!all.is_empty()).then(|| all.join(" "))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The project's plugin packages when run inside a project, otherwise the shared cache.
fn plugin_list() -> ExitCode {
    let here = std::path::Path::new(".");
    let in_project = here.join(dre_core::project::PROJECT_FILE).is_file();
    let dir = dre_core::plugins::plugins_dir(in_project.then_some(here));
    let found = dre_core::plugins::discover(&dir);
    if found.is_empty() {
        println!("No plugin packages installed in {}", dir.display());
        return exit::ok();
    }
    let quiet: dre_protocol::host::LogSink = std::sync::Arc::new(|_, _| {});
    let mut rows = vec![[
        "PACKAGE".to_string(),
        "PROVIDES".into(),
        "VERSION".into(),
        "PROTOCOL".into(),
        "PATH".into(),
    ]];
    for p in found {
        let first = p.provides.first();
        let (version, protocol) =
            match dre_protocol::host::PluginProcess::start_for(&p.path, first, quiet.clone(), None) {
                Ok(proc_) => {
                    let info = proc_.info().clone();
                    let _ = proc_.close();
                    (info.version, format!("v{}", info.protocol_version))
                }
                Err(e) => (
                    p.version.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
                    format!("error: {e}"),
                ),
            };
        let provides: Vec<String> = p.provides.iter().map(|i| i.to_string()).collect();
        rows.push([
            p.name,
            provides.join(", "),
            version,
            protocol,
            p.path.display().to_string(),
        ]);
    }
    let widths: Vec<usize> = (0..5)
        .map(|i| rows.iter().map(|r| r[i].len()).max().unwrap_or(0))
        .collect();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
    exit::ok()
}

/// Load and validate the project; print problems. `None` when it can't run.
/// Which profiles.yml a project uses, and why.
fn profiles_line(p: &dre_core::profiles::Profiles) -> String {
    let missing = if p.exists() { "" } else { ", not found" };
    format!("{} (from {}{missing})", p.path.display(), p.found_by)
}

/// The run's target and where it came from, for when the profiles' entries aren't known.
fn target_line(p: &dre_core::project::Project) -> String {
    format!("{} ({})", p.target_name, p.target_from)
}

/// What a project command needs of the project's plugins before it starts.
#[derive(Clone, Copy)]
enum NeedPlugins {
    /// All of them: installed, or with installing off, already there (`dre run`, `dre release`).
    All,
    /// Installed unless installing is off; then a missing one is reported by the Binding that
    /// needs it (`dre compile`, whose templates may not query).
    IfInstalling,
}

/// Install the project's macro packages, unless installing is off. False after printing why.
fn install_packages(args: &ProjectArgs, printer: &output::Printer) -> bool {
    args.no_auto_install || plugins::sync_packages(&args.project_dir, printer)
}

/// The start of every project command but `validate` and `deps`: [`install_packages`], then
/// [`load_for_run`].
fn load_project(
    args: &ProjectArgs,
    printer: &output::Printer,
) -> Option<(dre_core::project::Project, Option<String>)> {
    if !install_packages(args, printer) {
        return None;
    }
    load_for_run(args, printer)
}

/// Install or check the project's plugins, as `need` says. False after printing why.
fn ready_plugins(
    project: &dre_core::project::Project,
    args: &ProjectArgs,
    need: NeedPlugins,
    printer: &output::Printer,
) -> bool {
    let install = !args.no_auto_install;
    match need {
        NeedPlugins::All => plugins::ensure(project, install, false, printer),
        NeedPlugins::IfInstalling => !install || plugins::ensure(project, true, false, printer),
    }
}

/// What [`check_project`] checks.
#[derive(Clone, Copy, PartialEq)]
enum Checks {
    /// The project's plugins are declared (`dre compile`).
    PluginUses,
    /// That, then every connection's settings (`dre run`).
    PluginUsesAndOptions,
}

/// Check the project as `checks` says. False after printing the diagnostics, when there are
/// errors.
fn check_project(
    project: &dre_core::project::Project,
    args: &ProjectArgs,
    printer: &output::Printer,
    checks: Checks,
) -> bool {
    let mut diags = dre_core::Diagnostics::default();
    check_plugin_uses(project, args, &mut diags);
    if checks == Checks::PluginUsesAndOptions && !diags.has_errors() {
        dre_core::options::check(project, false, &mut diags);
    }
    report_diags(&diags, printer)
}

/// Load the project for `compile` and `run`, writing its manifest; `None` (after printing
/// why) when it can't run. Also returns the SHA-256 of the manifest written.
fn load_for_run(
    args: &ProjectArgs,
    printer: &output::Printer,
) -> Option<(dre_core::project::Project, Option<String>)> {
    let (project, diags) = project::load(&args.project_dir, &args.load_options());
    if let Some(p) = &project {
        dre_core::secrets::set_enabled(p.mask_secrets);
    }
    let errors = project
        .as_ref()
        .map(|p| dre_core::manifest::report_errors(p, &diags))
        .unwrap_or_default();
    let checksum = match write_manifest(project.as_ref(), &errors, args) {
        Ok(c) => c,
        Err(e) => {
            printer.error(&e);
            return None;
        }
    };
    for d in diags.sorted() {
        // Unmanaged reports warn again when they run.
        if d.severity == dre_core::Severity::Warning && d.code == "unmanaged-report" {
            continue;
        }
        printer.diag(d);
    }
    if diags.has_errors() {
        printer.error(&format!(
            "the project has {} error(s); fix them before running (see `dre validate`)",
            diags.error_count()
        ));
        return None;
    }
    project.map(|p| (p, checksum))
}

/// Write the whole project's manifest into its target folder, or, when the project didn't load,
/// remove a stale one. The error names the target path and where it was set.
fn write_manifest(
    project: Option<&dre_core::project::Project>,
    errors: &dre_core::manifest::ReportErrors,
    args: &ProjectArgs,
) -> Result<Option<String>, String> {
    let Some(p) = project else {
        let root = &args.project_dir;
        let from_file = dre_core::target::project_value(root);
        if let Ok(t) = dre_core::target::resolve(root, args.target_path.as_deref(), from_file.as_deref()) {
            dre_core::manifest::remove(&t.dir);
        }
        return Ok(None);
    };
    dre_core::manifest::write(p, errors)
        .map(Some)
        .map_err(|e| format!("{e} (target path from {})", p.target_source))
}

/// A path under the project as people read it: relative to the project when it's inside it.
fn shown(project: &dre_core::project::Project, p: &std::path::Path) -> PathBuf {
    dre_core::slash(p.strip_prefix(&project.root).unwrap_or(p))
}

/// Check that the declared plugin packages provide every plugin the project uses, naming the
/// package to add from DRE's registry (unless installing is off, which also means offline).
fn check_plugin_uses(
    project: &dre_core::project::Project,
    p: &ProjectArgs,
    diags: &mut dre_core::Diagnostics,
) {
    dre_core::plugins::check_uses(project, diags);
    if !p.no_auto_install {
        dre_core::manager::explain_undeclared(diags);
    }
}

/// Print `diags`; false (after saying so) when there are errors.
fn report_diags(diags: &dre_core::Diagnostics, printer: &output::Printer) -> bool {
    for d in diags.sorted() {
        printer.diag(d);
    }
    if diags.has_errors() {
        printer.error(&format!(
            "the project has {} error(s); fix them before running (see `dre validate`)",
            diags.error_count()
        ));
        return false;
    }
    true
}

pub(crate) fn run_date() -> Option<chrono::NaiveDate> {
    settings::env(settings::RUN_DATE).and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
}

/// `DRE_RUN_AT`: the instant a scheduled run was scheduled for (RFC 3339). Checked before any
/// project command starts, so callers can treat an error as unset.
pub(crate) fn run_at() -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
    match settings::env(settings::RUN_AT) {
        Some(v) => chrono::DateTime::parse_from_rfc3339(&v)
            .map(|t| Some(t.with_timezone(&chrono::Utc)))
            .map_err(|_| {
                format!("DRE_RUN_AT: `{v}` isn't an RFC 3339 date-time (e.g. 2026-09-01T06:00:00Z)")
            }),
        None => Ok(None),
    }
}

/// `--rate N/unit`.
fn parse_rate(s: &str) -> Result<dre_core::rate::Rate, String> {
    dre_core::rate::Rate::parse(s)
}

/// `dre release`: deliver a run's held deliveries.
fn release(a: ReleaseArgs, printer: output::Printer) -> ExitCode {
    let Some((project, _)) = load_project(&a.project, &printer) else {
        return exit::not_started();
    };
    if !ready_plugins(&project, &a.project, NeedPlugins::All, &printer) {
        return exit::not_started();
    }
    let by =
        a.by.or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("USERNAME").ok())
            .unwrap_or_else(|| "unknown".into());
    let opts = dre_core::release::ReleaseOptions {
        report: a.report.clone(),
        binding: a.binding.clone(),
        run_id: a.run.clone(),
        only_keys: a.only_keys,
        exclude_keys: a.exclude_keys,
        by: by.clone(),
        rate: a.rate,
        cancel: {
            let cancel = dre_core::engine::CancelToken::new();
            signals::watch(cancel.clone());
            cancel
        },
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // The release runs on its own thread; its warnings and progress show as they happen.
    let result = std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let events = dre_core::engine::Events::new(tx);
            dre_core::release::release(&project, &opts, &events)
        });
        let mut ui = printer.clone();
        for e in rx {
            match e {
                dre_core::engine::RunEvent::Warn(m) => printer.line(output::Tone::Warn, "Warning", m.trim()),
                dre_core::engine::RunEvent::Rows(p) => dre_core::run::Ui::rows(&mut ui, &p),
                _ => {}
            }
        }
        worker.join().expect("the release thread doesn't panic")
    });
    match result {
        Err(e) => {
            printer.error(&e.message);
            exit::failed()
        }
        Ok(o) => {
            let n = |s: &str| o.released.iter().filter(|r| r.status == s).count();
            for r in o.released.iter().filter(|r| r.status == "delivered") {
                printer.detail(
                    output::Tone::Good,
                    "Released",
                    &format!("{} → {}", r.id, r.location.as_deref().unwrap_or("")),
                );
            }
            let summary = format!(
                "release {} of run {} by {by}: {} delivered, {} failed, {} refused{}",
                o.release_id,
                a.run,
                n("delivered"),
                n("failed"),
                n("refused"),
                if o.already > 0 {
                    format!(", {} already released", o.already)
                } else {
                    String::new()
                }
            );
            if o.failed() {
                printer.line(output::Tone::Bad, "Failed", &summary);
                exit::failed()
            } else {
                printer.line(output::Tone::Good, "Released", &summary);
                exit::ok()
            }
        }
    }
}

fn run(a: RunArgs, mut printer: output::Printer) -> ExitCode {
    use std::io::IsTerminal;
    // Signals from the start: one that arrives while the project loads still cancels the run.
    let cancel = dre_core::engine::CancelToken::new();
    signals::watch(cancel.clone());
    let Some((project, manifest_checksum)) = load_project(&a.project, &printer) else {
        return exit::not_started();
    };
    if let Some(path) = &a.output_json {
        let value = path.to_string_lossy();
        if let Err(error) = dre_core::target::resolve(&project.root, Some(&value), None) {
            printer.error(&format!("--output-json: {error}"));
            return exit::not_started();
        }
    }
    // Run logs start before the plugins are installed, so the log files record installing them.
    printer.log_runs();
    for s in project.settings.lines() {
        printer.detail(output::Tone::Note, "Setting", &s);
    }
    if !ready_plugins(&project, &a.project, NeedPlugins::All, &printer) {
        return exit::not_started();
    }
    // `--timeout`, else `DRE_RUN_TIMEOUT`, else `flags: run_timeout`.
    let run_timeout = match dre_core::settings::flag_or_env(
        a.timeout.as_deref(),
        "--timeout",
        dre_core::settings::RUN_TIMEOUT,
    ) {
        Some((v, from)) => match dre_protocol::delivery::parse_duration(&serde_json::Value::String(v)) {
            Ok(d) if !d.is_zero() => Some(d),
            _ => {
                printer.error(&format!(
                    "{from} must be a duration such as `2h` or `90m`, or seconds"
                ));
                return exit::not_started();
            }
        },
        None => project.run_timeout,
    };
    // Each flag, else its environment variable; `keep_runs` falls back to `flags: keep_runs`.
    let count = |flag: &Option<String>, name, var| {
        count_setting(flag.as_deref(), name, var)
            .map_err(|e| printer.error(&e))
            .ok()
    };
    let Some(keep_runs) = count(&a.keep_runs, "--keep-runs", dre_core::settings::KEEP_RUNS) else {
        return exit::not_started();
    };
    let Some(row_concurrency) = count(
        &a.row_concurrency,
        "--row-concurrency",
        dre_core::settings::ROW_CONCURRENCY,
    ) else {
        return exit::not_started();
    };
    let Some(threads) = count(&a.threads, "--threads", dre_core::settings::THREADS) else {
        return exit::not_started();
    };
    if !check_project(&project, &a.project, &printer, Checks::PluginUsesAndOptions) {
        return exit::not_started();
    }
    let opts = dre_core::run::RunOptions {
        selector: selection(&a.select, &a.selector),
        set: a.set,
        target: a.project.target.clone(),
        profile: a.profile,
        vars: a.project.vars.iter().cloned().collect(),
        output_name: a.output_name,
        output_path: a.output_path,
        dry_run: a.dry_run,
        no_deliver: a.no_deliver || a.from_results.is_some(),
        output_json: a.output_json,
        from_results: a.from_results,
        row_concurrency,
        hold: a.hold,
        retry_failed: a.retry_failed,
        only_keys: a.only_keys,
        limit_rows: a.limit_rows,
        unmask: a.unmask,
        rate: a.rate,
        preview: a.preview,
        accept_schema_change: a.accept_schema_change,
        interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        date: run_date(),
        scheduled_at: run_at().ok().flatten(),
        timezone: a.project.timezone(),
        live_check: false,
        schedule: a.schedule,
        manifest_checksum,
        cancel,
        keep_runs,
        threads,
    };
    if let Some(name) = &opts.schedule
        && !project.schedules.iter().any(|e| &e.name == name)
    {
        printer.error(&dre_core::run::unknown_schedule(&project, name));
        return exit::not_started();
    }
    // Each Binding records its own date, in its own timezone; this line only logs the request.
    let date = opts
        .date
        .or(opts.scheduled_at.map(|t| t.date_naive()))
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let mut params = serde_json::to_value(opts.params(date)).unwrap_or_default();
    params["target"] = serde_json::json!(project.target_name);
    params["profiles"] = serde_json::json!(project.profiles.path);
    params["target_path"] = serde_json::json!(project.target_dir);
    printer.log_params(&params);
    printer.detail(output::Tone::Note, "Profiles", &profiles_line(&project.profiles));
    signals::set_timeout(run_timeout);
    if let Some(reason) = opts.cancel.reason() {
        // Cancelled before anything ran.
        printer.error("the run was cancelled before it started");
        return ExitCode::from(reason.exit_code());
    }
    let summary = dre_core::run::run(&project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::from(summary.exit_code());
    }
    if let Some(reason) = summary.cancelled {
        let what = match reason {
            dre_core::engine::CancelReason::Interrupt => "was cancelled by Ctrl-C".to_string(),
            dre_core::engine::CancelReason::Terminate => "was cancelled by a termination signal".to_string(),
            dre_core::engine::CancelReason::Timeout => {
                let during = summary
                    .outcomes
                    .iter()
                    .find(|o| o.status == dre_core::run::Status::TimedOut)
                    .map(|o| match &o.set {
                        Some(set) => format!(" while running {} (Set {set})", o.report),
                        None => format!(" while running {}", o.report),
                    })
                    .unwrap_or_default();
                format!(
                    "timed out after {}{during}",
                    run_timeout.map(signals::human).unwrap_or_default()
                )
            }
        };
        let not_run = match summary.not_run {
            0 => String::new(),
            1 => "; 1 Binding didn't run".into(),
            n => format!("; {n} Bindings didn't run"),
        };
        printer.error(&format!("the run {what}{not_run}"));
    }
    printer.finish("run");
    ExitCode::from(summary.exit_code())
}

fn clean(a: CleanArgs) -> ExitCode {
    let root = &a.project_dir;
    let from_file = dre_core::target::project_value(root);
    let t = match dre_core::target::resolve(root, a.target_path.as_deref(), from_file.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::not_started();
        }
    };
    if a.prune {
        let keep = match count_setting(
            a.keep_runs.as_deref(),
            "--keep-runs",
            dre_core::settings::KEEP_RUNS,
        ) {
            Ok(Some(k)) => k,
            Ok(None) => {
                dre_core::target::project_flag_u64(root, "keep_runs").map_or(1, |k| k.max(1) as usize)
            }
            Err(e) => {
                eprintln!("error: {e}");
                return exit::not_started();
            }
        };
        let mut removed = 0;
        for report in std::fs::read_dir(t.dir.join("run"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for binding in std::fs::read_dir(report.path()).into_iter().flatten().flatten() {
                if binding.path().is_dir() {
                    removed += dre_core::runs::BindingRuns::new(&binding.path())
                        .prune(keep)
                        .len();
                }
            }
        }
        eprintln!("Removed {removed} run(s); kept the last {keep} of each report and Set");
        return exit::ok();
    }
    let shown = dre_core::slash(t.dir.strip_prefix(root).unwrap_or(&t.dir));
    match dre_core::target::clean_dir(root, &t) {
        Ok(dre_core::target::Cleaned::Removed) => {
            eprintln!("Removed {}/", shown.display());
            exit::ok()
        }
        Ok(dre_core::target::Cleaned::Missing) => {
            eprintln!("Nothing to clean: no {}/ directory", shown.display());
            exit::ok()
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit::failed()
        }
    }
}

/// A count of 1 or more from `flag_name`, else the environment variable `var`; any fallback
/// after that is the caller's.
fn count_setting(
    flag: Option<&str>,
    flag_name: &'static str,
    var: &'static str,
) -> Result<Option<usize>, String> {
    match dre_core::settings::flag_or_env(flag, flag_name, var) {
        None => Ok(None),
        Some((v, from)) => match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => Ok(Some(n)),
            _ => Err(format!("{from} must be a whole number of 1 or more, got `{v}`")),
        },
    }
}

/// `dre deps` has no `--no-auto-install`, and it doesn't use [`load_project`]: it writes no
/// manifest, and it syncs a project that has other errors, since it only needs what's declared.
fn deps(a: DepsArgs, printer: &output::Printer) -> ExitCode {
    if !plugins::sync_packages(&a.project_dir, printer) {
        return exit::failed();
    }
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir,
        ..Default::default()
    };
    let (project, diags) = project::load(&a.project_dir, &opts);
    let Some(project) = project else {
        for d in diags.sorted() {
            printer.diag(d);
        }
        return exit::not_started();
    };
    if plugins::ensure(&project, true, true, printer) {
        let extensions = match extensions::sync_declared(&a.project_dir, a.accept_proprietary, printer) {
            Ok(count) => count,
            Err(error) => {
                printer.error(&error);
                return exit::failed();
            }
        };
        printer.line(
            output::Tone::Good,
            "Synced",
            &format!(
                "{} plugin(s), {} package(s) and {} companion(s); plugin/package dre.lock is up to date",
                project.plugins.len(),
                project.packages.len(),
                extensions
            ),
        );
        exit::ok()
    } else {
        exit::failed()
    }
}

#[cfg(test)]
mod count_setting_tests {
    use super::count_setting;

    #[test]
    fn counts_are_one_or_more() {
        let max = usize::MAX.to_string();
        for flag in ["--keep-runs", "--row-concurrency", "--threads"] {
            let parse = |v: &str| count_setting(Some(v), flag, "DRE_TEST_COUNT_UNSET");
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
}

#[cfg(test)]
mod cli_reference_tests {
    use clap::CommandFactory;

    /// `docs/cli-reference.md` is generated from the CLI definitions. After changing a command
    /// or flag, `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --bin dre cli_reference` rewrites it.
    #[test]
    fn the_cli_reference_page_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli-reference.md");
        let page = crate::cli_reference::page(super::Cli::command());
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        // The page's previous/next links are .github/scripts/docs_sections.py's; keep them.
        let (body, nav) = match old.find("\n<!-- docs-nav") {
            Some(i) => (&old[..=i], &old[i + 1..]),
            None => (old.as_str(), ""),
        };
        if std::env::var_os("DRE_UPDATE_DOCS").is_some() {
            let nav = if nav.is_empty() {
                String::new()
            } else {
                format!("\n{nav}")
            };
            std::fs::write(&path, format!("{page}{nav}")).unwrap();
        } else {
            assert_eq!(
                format!("{}\n", body.trim_end()),
                page,
                "docs/cli-reference.md is stale: run `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --bin dre cli_reference`"
            );
        }
    }
}
