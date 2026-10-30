#[cfg(test)]
mod cli_reference;
mod exit;
mod extensions;
mod fonts;
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
use dre_core::start::{Checks, NeedPlugins, ProjectRequest};
use dre_core::{Context, Env};

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
    fn printer(&self, secrets: dre_core::secrets::Secrets) -> output::Printer {
        let v = self.log_level.unwrap_or(if self.verbose {
            output::Verbosity::Debug
        } else if self.quiet {
            output::Verbosity::Quiet
        } else {
            output::Verbosity::Info
        });
        output::Printer::new(v, self.log_format, self.color, secrets)
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
    /// Install or list the fallback fonts PDFs use for other scripts (offline servers, images).
    #[command(subcommand, long_about = fonts::ABOUT)]
    Fonts(fonts::FontsCommand),
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
    fn timezone(&self, env: &Env) -> Option<String> {
        settings::flag_or_env(env, self.timezone.as_deref(), "--timezone", settings::TIMEZONE).map(|(v, _)| v)
    }

    fn load_options(&self, env: &Env) -> LoadOptions {
        LoadOptions {
            select: None,
            profiles_dir: self.profiles_dir.clone(),
            target: self.target.clone(),
            vars: self.vars.iter().cloned().collect(),
            target_path: self.target_path.clone(),
            date: settings::run_date(env),
            scheduled_at: settings::run_at(env).ok().flatten(),
            timezone: self.timezone(env),
            settings: settings::run_settings(env, self.timezone.as_deref()),
        }
    }

    /// The project as core's commands take it.
    fn request(&self, env: &Env) -> ProjectRequest {
        ProjectRequest {
            dir: self.project_dir.clone(),
            load: self.load_options(env),
            install: !self.no_auto_install,
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
    // The one place DRE reads its environment: every command gets it through `ctx`.
    let ctx = Context::subprocess(Env::from_process());
    let env = ctx.env();
    let printer = cli.printer(ctx.secrets().clone());
    let project_args = match &cli.command {
        Command::Validate(a) => Some(&a.project),
        Command::Run(a) => Some(&a.project),
        Command::Compile(a) => Some(&a.project),
        _ => None,
    };
    if let Some(t) = project_args.and_then(|p| p.timezone(env))
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
        && let Err(e) = settings::run_at(env)
    {
        printer.error(&e);
        return exit::not_started();
    }
    match cli.command {
        Command::Explain(a) => explain(&a.code),
        Command::History(a) => history::history(&ctx, a),
        Command::Unlock(a) => history::unlock(&ctx, a),
        Command::Release(a) => release(&ctx, a, &printer),
        Command::Validate(a) => validate(&ctx, a, &printer),
        Command::Run(a) => run(&ctx, *a, printer),
        Command::Compile(a) => compile(&ctx, a, printer),
        Command::Query(a) => query(&ctx, a, &printer),
        Command::Catalog(a) => catalog(&ctx, a, &printer),
        Command::Mcp(a) => extensions::handoff(&ctx, "mcp", a, &printer),
        Command::Extension(a) => extensions::manage(&ctx, a, &printer),
        Command::Clean(a) => clean(&ctx, a),
        Command::Ls(a) => ls::ls(&ctx, a),
        Command::Schedule(schedule::ScheduleCommand::Ls(a)) => schedule::ls(&ctx, a),
        Command::System(system::SystemCommand::Update(a)) => system::update(&ctx, a, &printer),
        Command::Fonts(fonts::FontsCommand::Install(a)) => fonts::install(&ctx, &a, &printer),
        Command::Fonts(fonts::FontsCommand::Ls) => fonts::ls(&ctx),
        Command::Deps(a) => deps(&ctx, a, &printer),
        Command::Init(a) => init::init(&ctx, a.profiles_dir.as_deref(), &printer),
        Command::New(a) => init::new(&a.dir, a.profile, a.plugin, &printer),
        Command::Plugin(PluginCommand::List) => plugin_list(&ctx),
        Command::Plugin(PluginCommand::Install(a)) => {
            plugins::install(&ctx, &a.plugin, &a.project_dir, false, &printer)
        }
        Command::Plugin(PluginCommand::Update(a)) => {
            plugins::install(&ctx, &a.plugin, &a.project_dir, true, &printer)
        }
        Command::Plugin(PluginCommand::Remove(a)) => {
            plugins::remove(&ctx, &a.plugin, &a.project_dir, &printer)
        }
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

fn validate(ctx: &Context, a: ValidateArgs, printer: &output::Printer) -> ExitCode {
    let req = a.project.request(ctx.env());
    // A failed or skipped install isn't fatal here: the load reports a missing package with
    // the rest.
    if req.install {
        dre_core::start::install_packages(&req.dir, printer);
    }
    let selector = selection(&a.select, &a.selector);
    let (project, mut diags) = project::load_with(&req.dir, &req.load, ctx);
    // validate checks every report, so templates that don't render are its errors too.
    for d in project.iter().flat_map(|p| p.all_parse_errors()) {
        diags.push(d.clone());
    }
    let report_errors = project
        .as_ref()
        .map(|p| dre_core::manifest::report_errors(p, &diags))
        .unwrap_or_default();
    if let Some(p) = &project {
        ctx.secrets().set_enabled(p.mask_secrets());
    }
    if let Err(e) = dre_core::start::write_manifest(ctx, project.as_ref(), &report_errors, &req) {
        diags.error(Code::TargetPathUnwritable, None, None, e);
    }
    if let Some(p) = &project {
        // Plugins aren't installed when the project already has errors, so a missing one is then
        // only a warning; the option checks run either way, so every problem shows in one pass.
        let offline = a.project.no_auto_install || diags.has_errors();
        plugins::check_for_validate(ctx, p, req.install, &mut diags, printer);
        dre_core::start::check_plugin_uses(ctx, p, req.install, &mut diags);
        dre_core::options::check(ctx, p, offline, &mut diags);
    }
    // Only a project that checks out gets compiled.
    let (plans, targets) = match &project {
        Some(p) if !diags.has_errors() => {
            compile_for_validate(ctx, p, selector.as_deref(), &a.set, &a.project, &mut diags)
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
            used_entries(targets.as_ref())
        };
        if a.all_targets || !diags.has_errors() {
            dre_core::options::check_connections(ctx, p, &entries, &mut diags);
        }
    }
    // `--strict`: warnings count as errors.
    let ok = !diags.has_errors() && !(a.strict && diags.warning_count() > 0);
    let report = Validation {
        ok,
        project: project.as_ref(),
        diags: &diags,
        targets: targets.as_ref(),
        plans: &plans,
        report_errors: &report_errors,
    };
    if a.json {
        report.print_json(ctx);
    } else {
        report.print(printer, selector.is_some());
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
        return validate_live(ctx, &project, selector, a.set, &a.project, printer.clone());
    }
    exit::ok()
}

/// The profile entries a run would use, which `validate` checks by their plugins.
fn used_entries(targets: Option<&dre_core::run::RunTargets>) -> Vec<dre_core::options::EntryRef> {
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
}

/// What `dre validate` found, to print.
struct Validation<'v> {
    ok: bool,
    project: Option<&'v dre_core::project::Project>,
    diags: &'v dre_core::Diagnostics,
    targets: Option<&'v dre_core::run::RunTargets>,
    plans: &'v [dre_core::run::BindingPlan],
    report_errors: &'v dre_core::manifest::ReportErrors,
}

impl Validation<'_> {
    /// `--json`: one document, secrets masked.
    fn print_json(&self, ctx: &Context) {
        let (project, diags, targets) = (self.project, self.diags, self.targets);
        let out = serde_json::json!({
            "ok": self.ok,
            "profiles": project.map(|p| serde_json::json!({
                "path": p.profiles().path,
                "exists": p.profiles().exists(),
                "found_by": p.profiles().found_by,
            })),
            "target": project.map(|p| serde_json::json!({
                "name": p.target_name(),
                "from": p.run_target().from.to_string(),
                "profiles": targets.map(|t| t.profiles.clone()).unwrap_or_default(),
            })),
            "settings": project.map(|p| p.settings()),
            "errors": diags.error_count(),
            "warnings": diags.warning_count(),
            "diagnostics": diags.sorted(),
            "compiled": self.plans,
            "project": project.map(|p| dre_core::manifest::build(p, self.report_errors)),
        });
        println!(
            "{}",
            ctx.secrets()
                .to_json_pretty(&out)
                .expect("a JSON value serializes")
        );
    }

    /// The diagnostics, what compiled, the profiles and target, and the verdict.
    fn print(&self, printer: &output::Printer, selected: bool) {
        for d in self.diags.sorted() {
            printer.diag(d);
        }
        if selected {
            for p in self.plans {
                printer.plan(p);
            }
        } else if !self.plans.is_empty()
            && let Some(p) = self.project
        {
            println!(
                "Compiled {} Binding{} into {}/",
                self.plans.len(),
                plural(self.plans.len()),
                shown(p, &p.target_dir().join("compiled")).display()
            );
        }
        if let Some(p) = self.project {
            printer.line(
                output::Tone::Note,
                "Profiles",
                &dre_core::start::profiles_line(p.profiles()),
            );
            let line = self.targets.map_or_else(|| target_line(p), |t| t.line());
            printer.line(output::Tone::Note, "Target", &line);
            for s in p.settings().lines() {
                printer.detail(output::Tone::Note, "Setting", &s);
            }
        }
        let (e, w) = (self.diags.error_count(), self.diags.warning_count());
        let verdict = if self.ok { "passed" } else { "failed" };
        println!(
            "Validation {verdict}: {e} error{}, {w} warning{}",
            plural(e),
            plural(w)
        );
    }
}

/// Compiles quietly, collecting what each Binding would do.
#[derive(Default)]
struct Collect {
    plans: Vec<dre_core::run::BindingPlan>,
    targets: Option<dre_core::run::RunTargets>,
}

impl dre_core::engine::EventSink for Collect {
    fn event(&mut self, e: dre_core::engine::RunEvent) {
        use dre_core::engine::RunEvent;
        match e {
            RunEvent::Targets(t) => self.targets = Some(t),
            RunEvent::Compiled(plan) => self.plans.push(plan),
            RunEvent::Block(events) => {
                for e in events {
                    self.event(e);
                }
            }
            _ => {}
        }
    }
}

/// `validate` compiles the selection (every Set of every report by default); a Binding that
/// doesn't render is an error.
fn compile_for_validate(
    ctx: &Context,
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
        date: settings::run_date(ctx.env()),
        scheduled_at: settings::run_at(ctx.env()).ok().flatten(),
        timezone: p.timezone(ctx.env()),
        mode: dre_core::run::Mode::compile(),
        ..Default::default()
    };
    let mut ui = Collect::default();
    let summary = dre_core::run::run(ctx, project, &opts, &mut ui);
    if let Some(e) = summary.error {
        diags.error(core_code(&e.code), None, None, e.message);
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
        if o.error_code == Some(Code::CompileNeedsRun.into()) {
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

fn catalog(ctx: &Context, a: CatalogArgs, printer: &output::Printer) -> ExitCode {
    json_command(ctx, &a.project, a.output, printer, |project| {
        let request =
            std::fs::read_to_string(project.root().join(&a.tables_file)).map_err(|e| e.to_string())?;
        let request =
            serde_json::from_str(&request).map_err(|e| format!("invalid catalogue request: {e}"))?;
        dre_core::catalog::build(ctx, project, &a.profile, &request)
    })
}

fn query(ctx: &Context, a: QueryArgs, printer: &output::Printer) -> ExitCode {
    json_command(ctx, &a.project, a.output, printer, |project| {
        let sql = std::fs::read_to_string(project.root().join(&a.sql_file)).map_err(|e| e.to_string())?;
        dre_core::query::execute(ctx, project, &a.profile, &sql, a.max_rows)
    })
}

/// `dre query` and `dre catalog`: load the project, build one JSON document, write it.
fn json_command(
    ctx: &Context,
    args: &ProjectArgs,
    output: OutputFile,
    printer: &output::Printer,
    build: impl FnOnce(&dre_core::project::Project) -> Result<serde_json::Value, String>,
) -> ExitCode {
    let Some((project, _)) = dre_core::start::load_project(ctx, &args.request(ctx.env()), printer) else {
        return exit::not_started();
    };
    let file = output.output_file;
    let result = build(&project).and_then(|value| {
        let path = project.root().join(&file);
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
fn compile(ctx: &Context, a: CompileArgs, mut printer: output::Printer) -> ExitCode {
    let req = a.project.request(ctx.env());
    let Some((project, manifest_checksum)) = dre_core::start::load_project(ctx, &req, &printer) else {
        return exit::not_started();
    };
    if !dre_core::start::ready_plugins(ctx, &project, req.install, NeedPlugins::IfInstalling, &printer)
        || !dre_core::start::check_project(ctx, &project, req.install, Checks::PluginUses, &printer)
    {
        return exit::not_started();
    }
    let opts = dre_core::run::RunOptions {
        selector: selection(&a.select, &a.selector),
        set: a.set,
        target: a.project.target.clone(),
        vars: a.project.vars.iter().cloned().collect(),
        date: settings::run_date(ctx.env()),
        scheduled_at: settings::run_at(ctx.env()).ok().flatten(),
        timezone: a.project.timezone(ctx.env()),
        mode: dre_core::run::Mode::compile(),
        choose_set: set_chooser(&printer),
        manifest_checksum,
        ..Default::default()
    };
    let summary = dre_core::run::run(ctx, &project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(&e.message);
        return ExitCode::from(summary.exit_code());
    }
    printer.finish("compile");
    ExitCode::from(summary.exit_code())
}

fn validate_live(
    ctx: &Context,
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
        date: settings::run_date(ctx.env()),
        scheduled_at: settings::run_at(ctx.env()).ok().flatten(),
        timezone: p.timezone(ctx.env()),
        mode: dre_core::run::Mode::check(),
        ..Default::default()
    };
    let summary = dre_core::run::run(ctx, project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(&e.message);
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
fn plugin_list(ctx: &Context) -> ExitCode {
    let here = std::path::Path::new(".");
    let in_project = here.join(dre_core::project::PROJECT_FILE).is_file();
    let dir = dre_core::plugins::plugins_dir(ctx.env(), in_project.then_some(here));
    let found = dre_core::plugins::discover(&dir, &dre_core::plugins::Prober::default());
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
        let started = match p.provides.first() {
            Some(first) => {
                let plugin = dre_core::plugins::Located {
                    path: p.path.clone(),
                    plugin: first.clone(),
                    package: p.name.clone(),
                };
                ctx.host()
                    .start(&plugin, quiet.clone(), None)
                    .map_err(|e| e.to_string())
            }
            // An executable that couldn't say what it provides doesn't start.
            None => Err(format!("can't ask {} what it provides", p.path.display())),
        };
        let (version, protocol) = match started {
            Ok(proc_) => {
                let info = proc_.info().clone();
                proc_.close();
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
    print!("{}", ls::columns(rows));
    exit::ok()
}

/// The run's target and where it came from, for when the profiles' entries aren't known.
fn target_line(p: &dre_core::project::Project) -> String {
    format!("{} ({})", p.target_name(), p.run_target().from)
}

/// A path under the project as people read it: relative to the project when it's inside it.
fn shown(project: &dre_core::project::Project, p: &std::path::Path) -> PathBuf {
    dre_core::slash(p.strip_prefix(project.root()).unwrap_or(p))
}

/// `--rate N/unit`.
fn parse_rate(s: &str) -> Result<dre_core::rate::Rate, String> {
    dre_core::rate::Rate::parse(s)
}

/// `dre release`: deliver a run's held deliveries.
fn release(ctx: &Context, a: ReleaseArgs, printer: &output::Printer) -> ExitCode {
    let req = a.project.request(ctx.env());
    let Some((project, _)) = dre_core::start::load_project(ctx, &req, printer) else {
        return exit::not_started();
    };
    if !dre_core::start::ready_plugins(ctx, &project, req.install, NeedPlugins::All, printer) {
        return exit::not_started();
    }
    let by =
        a.by.or_else(|| ctx.env().setting("USER"))
            .or_else(|| ctx.env().setting("USERNAME"))
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
            signals::watch(cancel.clone(), ctx.host().clone());
            cancel
        },
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // The release runs on its own thread; its warnings and progress show as they happen.
    let result = std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let events = dre_core::engine::Events::new(tx);
            dre_core::release::release(ctx, &project, &opts, &events)
        });
        let mut ui = printer.clone();
        for e in rx {
            match e {
                dre_core::engine::RunEvent::Warn(m) => printer.line(output::Tone::Warn, "Warning", m.trim()),
                dre_core::engine::RunEvent::Rows(p) => ui.on_rows(&p),
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

fn run(ctx: &Context, a: RunArgs, mut printer: output::Printer) -> ExitCode {
    // Signals from the start: one that arrives while the project loads still cancels the run.
    let cancel = dre_core::engine::CancelToken::new();
    signals::watch(cancel.clone(), ctx.host().clone());
    let req = dre_core::start::RunRequest {
        project: a.project.request(ctx.env()),
        timeout: a.timeout,
        keep_runs: a.keep_runs,
        row_concurrency: a.row_concurrency,
        threads: a.threads,
        options: dre_core::run::RunOptions {
            selector: selection(&a.select, &a.selector),
            set: a.set,
            target: a.project.target.clone(),
            profile: a.profile,
            vars: a.project.vars.iter().cloned().collect(),
            output_name: a.output_name,
            output_path: a.output_path,
            mode: run_mode(
                a.dry_run,
                a.from_results,
                a.retry_failed,
                a.hold,
                a.no_deliver,
                a.preview,
            ),
            output_json: a.output_json,
            rows: dre_core::run::RowOptions {
                only_keys: a.only_keys,
                limit: a.limit_rows,
                unmask: a.unmask,
                ..Default::default()
            },
            rate: a.rate,
            accept_schema_change: a.accept_schema_change,
            choose_set: set_chooser(&printer),
            timezone: a.project.timezone(ctx.env()),
            schedule: a.schedule,
            cancel,
            ..Default::default()
        },
    };
    let (summary, run_timeout) = match dre_core::start::run(ctx, req, &mut printer) {
        dre_core::start::Started::NotStarted => return exit::not_started(),
        dre_core::start::Started::Cancelled(reason) => return ExitCode::from(reason.exit_code()),
        dre_core::start::Started::Ran { summary, timeout } => (summary, timeout),
    };
    if let Some(e) = &summary.error {
        printer.error(&e.message);
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

/// `dre run`'s mode from its flags (clap keeps the conflicting ones apart).
fn run_mode(
    dry_run: bool,
    from_results: Option<PathBuf>,
    retry_failed: Option<String>,
    hold: bool,
    no_deliver: bool,
    preview: Option<u64>,
) -> dre_core::run::Mode {
    use dre_core::run::{DeliveryMode, Mode, Results};
    let results = match (dry_run, from_results, retry_failed) {
        (true, _, _) => Results::Compile,
        (_, Some(dir), _) => Results::Replay(dir),
        (_, _, Some(id)) => Results::Retry(id),
        _ => Results::Query,
    };
    // `--from-results` delivers nothing: it replays results to look at them.
    let delivery = if no_deliver || matches!(results, Results::Replay(_)) {
        DeliveryMode::Keep
    } else if hold {
        DeliveryMode::Hold
    } else {
        DeliveryMode::Send
    };
    Mode {
        results,
        delivery,
        preview,
    }
}

/// Asks which Set to run, when a person is at the terminal to answer.
fn set_chooser(printer: &output::Printer) -> Option<dre_core::run::SetChooser> {
    use std::io::IsTerminal;
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    let printer = printer.clone();
    Some(dre_core::run::SetChooser::new(move |report, sets| {
        printer.choose_set(report, sets)
    }))
}

/// The core code of an error `dre_core` raised itself.
fn core_code(code: &dre_core::codes::ErrorCode) -> Code {
    match code {
        dre_core::codes::ErrorCode::Core(c) => *c,
        dre_core::codes::ErrorCode::Plugin { .. } => Code::RunFailed,
    }
}

fn clean(ctx: &Context, a: CleanArgs) -> ExitCode {
    let root = &a.project_dir;
    let from_file = dre_core::target::project_value(root);
    let t = match dre_core::target::resolve(root, a.target_path.as_deref(), from_file.as_deref(), ctx.env()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::not_started();
        }
    };
    if a.prune {
        let keep = match dre_core::start::count_setting(
            ctx.env(),
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

/// `dre deps` has no `--no-auto-install`, and it doesn't use [`load_project`]: it writes no
/// manifest, and it syncs a project that has other errors, since it only needs what's declared.
fn deps(ctx: &Context, a: DepsArgs, printer: &output::Printer) -> ExitCode {
    if !dre_core::start::install_packages(&a.project_dir, printer) {
        return exit::failed();
    }
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir,
        ..Default::default()
    };
    let (project, diags) = project::load_with(&a.project_dir, &opts, ctx);
    let Some(project) = project else {
        for d in diags.sorted() {
            printer.diag(d);
        }
        return exit::not_started();
    };
    if dre_core::start::sync_plugins(ctx, &project, true, true, printer) {
        let extensions = match extensions::sync_declared(ctx, &a.project_dir, a.accept_proprietary, printer) {
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
                project.plugins().len(),
                project.packages().len(),
                extensions
            ),
        );
        exit::ok()
    } else {
        exit::failed()
    }
}

#[cfg(test)]
mod tests {
    use dre_core::run::{DeliveryMode, Results, RunTargets, UsedProfile};

    use super::*;

    #[test]
    fn run_flags_pick_the_mode() {
        let m = run_mode(false, None, None, false, false, None);
        assert_eq!((m.results, m.delivery), (Results::Query, DeliveryMode::Send));
        assert_eq!(
            run_mode(true, None, None, false, false, None).results,
            Results::Compile
        );
        // A replay delivers nothing.
        let m = run_mode(false, Some("saved".into()), None, false, false, None);
        assert_eq!(
            (m.results, m.delivery),
            (Results::Replay("saved".into()), DeliveryMode::Keep)
        );
        let m = run_mode(false, None, Some("r1".into()), true, false, Some(5));
        assert_eq!(m.results, Results::Retry("r1".into()));
        assert_eq!((m.delivery, m.preview), (DeliveryMode::Hold, Some(5)));
        assert_eq!(
            run_mode(false, None, None, false, true, None).delivery,
            DeliveryMode::Keep
        );
    }

    #[test]
    fn validate_checks_the_entries_a_run_would_deliver_through() {
        let used = |role, profile: &str, deliver| UsedProfile {
            role,
            profile: profile.into(),
            target: "prod".into(),
            deliver,
        };
        let targets = RunTargets {
            name: "prod".into(),
            from: "--target".into(),
            profiles: vec![
                used("connection", "warehouse", true),
                used("destination", "outbox", true),
                used("destination", "nowhere", false),
            ],
        };
        let entries = used_entries(Some(&targets));
        assert_eq!(
            entries,
            [
                (
                    dre_core::profiles::Role::Connection,
                    "warehouse".into(),
                    "prod".into()
                ),
                (
                    dre_core::profiles::Role::Destination,
                    "outbox".into(),
                    "prod".into()
                ),
            ]
        );
        assert!(used_entries(None).is_empty());
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
