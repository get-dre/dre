//! `dre init` (interactive onboarding) and `dre new` (scaffold a project).

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dre_core::manager::{self, Index, IndexPackage};
use dre_core::profiles::Role;
use dre_core::project::{PluginId, PluginKind};
use dre_protocol::msg::ConnectionField;
use serde_json::Value;

/// A profile's settings, in the order they're written.
type Mapping = serde_json::Map<String, Value>;

use crate::output::{Printer, Tone};

/// What a scaffolded project declares.
pub struct Scaffold {
    pub name: String,
    pub profile: String,
    /// Destination plugin names and their profiles.
    pub destinations: Vec<(String, String)>,
    /// The plugin packages to declare, besides `csv` (the default output format's).
    pub packages: Vec<String>,
}

/// The first line of a YAML file DRE reads, pointing editors (the YAML language server in VS Code
/// and JetBrains) at its JSON Schema. Versioned by DRE's minor version, like the schemas.
fn schema_line(kind: &str) -> String {
    let mut v = env!("CARGO_PKG_VERSION").split(['.', '-']);
    let minor = format!("{}.{}", v.next().unwrap_or("0"), v.next().unwrap_or("0"));
    format!("# yaml-language-server: $schema=https://getdre.com/schemas/v{minor}/{kind}.schema.json\n")
}

/// Write a starter project into `dir`, which must be missing or empty.
pub fn scaffold(dir: &Path, s: &Scaffold) -> Result<Vec<PathBuf>, String> {
    if dir.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!("{} already exists and isn't empty", dir.display()));
    }
    let mut packages: Vec<&str> = s.packages.iter().map(String::as_str).collect();
    packages.push("csv");
    let mut plugins = String::from("plugins:\n");
    let mut seen = std::collections::BTreeSet::new();
    for p in packages.into_iter().filter(|p| seen.insert(*p)) {
        plugins.push_str(&format!("  - {p}\n"));
    }
    let output = if s.destinations.is_empty() {
        String::new()
    } else {
        let mut o = String::from(
            "\n# Delivered in addition to the copy in target/run/, to each destination in turn.\n\
             # Adjust the path and options for your destinations.\n\
             output:\n  destination:\n",
        );
        for (kind, profile) in &s.destinations {
            o.push_str(&format!("    - profile: {profile}\n"));
            o.push_str(match kind.as_str() {
                "email" => "      to: someone@example.com\n      subject: \"{{ run.report }} {{ run.date.iso }}\"\n",
                "slack" => "      channel: \"#reports\"\n      message: \"{{ run.report }} for {{ run.date.iso }}\"\n",
                "databricks" => {
                    "      path: \"/Volumes/<catalog>/<schema>/<volume>/{{ run.report }}-{{ run.date.yyyymmdd }}.csv\"\n"
                }
                _ => "      path: \"reports/{{ run.report }}-{{ run.date.yyyymmdd }}.csv\"\n",
            });
        }
        o
    };
    let files: Vec<(&str, String)> = vec![
        (
            "dre_project.yml",
            format!(
                "{}name: {}\n# Connection under `connections:` in ~/.dre/profiles.yml for reports that don't name one.\ndefault_profile: {}\n",
                schema_line("project"),
                s.name,
                s.profile
            ),
        ),
        (
            "dependencies.yml",
            format!(
                "{}# Plugin packages this project needs. `dre deps` installs them.\n{plugins}",
                schema_line("dependencies")
            ),
        ),
        (
            "reports/examples/hello/hello.yml",
            format!(
                "{}# A managed report: its queries, and (optionally) output and Sets.\n\
                 queries:\n  - {{query: hello, tab_name: Hello}}\n{output}",
                schema_line("report")
            ),
        ),
        (
            "reports/examples/hello/hello.sql",
            "-- Jinja works everywhere: run.*, var(), env_var() and your macros.\n\
             select '{{ run.report }}' as report, '{{ run.date }}' as run_date, 'Hello from DRE' as message\n"
                .to_string(),
        ),
        (
            "timings.yml",
            format!(
                "{}# Named timings: write one once, then use it from any schedule in schedules.yml with\n\
                 # `timing: <name>`. `dre schedule ls` shows when your schedules fire.\n\
                 #\n\
                 # month_start:\n\
                 #   cron: \"0 6 1 * *\"            # 06:00 on the 1st\n\
                 #   timezone: Australia/Sydney\n\
                 #   except: [\"2027-01-01\"]       # dates to skip\n\
                 #\n\
                 # second_tuesday:\n\
                 #   rrule: \"FREQ=MONTHLY;BYDAY=2TU\"\n\
                 #   at: \"07:00\"\n\
                 #\n\
                 # Then, in schedules.yml:\n\
                 # - name: hello_monthly\n\
                 #   report: hello\n\
                 #   timing: month_start\n",
                schema_line("timings")
            ),
        ),
        ("macros/.gitkeep", String::new()),
        ("templates/.gitkeep", String::new()),
        (".gitignore", "target/\nlogs/\ndre_deps/\n".to_string()),
    ];
    let mut written = Vec::new();
    for (rel, content) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("the path has a parent folder"))
            .map_err(|e| format!("can't create {}: {e}", p.display()))?;
        std::fs::write(&p, content).map_err(|e| format!("can't write {}: {e}", p.display()))?;
        written.push(p);
    }
    Ok(written)
}

/// `dre new <dir>`.
pub fn new(dir: &Path, profile: String, source: String, printer: &Printer) -> ExitCode {
    let name = project_name(dir);
    match scaffold(
        dir,
        &Scaffold {
            name,
            profile,
            // First-party sources come in a package of the same name.
            packages: vec![source],
            destinations: Vec::new(),
        },
    ) {
        Ok(files) => {
            printer.line(
                Tone::Good,
                "Created",
                &format!("{} ({} files)", dir.display(), files.len()),
            );
            printer.line(Tone::Note, "Next", &format!("cd {} && dre run", dir.display()));
            crate::exit::ok()
        }
        Err(e) => {
            printer.error(&e);
            crate::exit::failed()
        }
    }
}

fn project_name(dir: &Path) -> String {
    let raw = dir
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "my_reports".into());
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "my_reports".into() } else { s }
}

struct Prompter<R: BufRead> {
    input: R,
}

/// Restore terminal echo even when reading fails or setup unwinds.
struct QuietInput {
    #[cfg(unix)]
    mode: libc::termios,
    #[cfg(windows)]
    mode: u32,
}

impl QuietInput {
    fn new() -> Result<Option<Self>, String> {
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        #[cfg(unix)]
        // SAFETY: stdin is a terminal; tcgetattr initializes mode before it is read.
        unsafe {
            let mut mode = std::mem::MaybeUninit::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, mode.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let mode = mode.assume_init();
            let mut quiet = mode;
            quiet.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Some(Self { mode }))
        }
        #[cfg(windows)]
        // SAFETY: the console APIs validate the standard input handle and mode pointer.
        unsafe {
            use windows_sys::Win32::System::Console::{
                ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
            };
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0
                || SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Some(Self { mode }))
        }
    }
}

impl Drop for QuietInput {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: mode came from tcgetattr on this terminal.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.mode);
        }
        #[cfg(windows)]
        // SAFETY: mode came from GetConsoleMode on this standard input handle.
        unsafe {
            use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode};
            SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), self.mode);
        }
    }
}

impl<R: BufRead> Prompter<R> {
    fn ask(&mut self, q: &str, default: Option<&str>) -> Result<String, String> {
        match default {
            Some(d) if !d.is_empty() => eprint!("{q} [{d}]: "),
            _ => eprint!("{q}: "),
        }
        self.read(default)
    }

    /// A bare `>` input line, under a field's name and description.
    fn input(&mut self, default: Option<&str>) -> Result<String, String> {
        match default {
            Some(d) if !d.is_empty() => eprint!("    [{d}] > "),
            _ => eprint!("    > "),
        }
        self.read(default)
    }

    fn read(&mut self, default: Option<&str>) -> Result<String, String> {
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if self.input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return match default {
                Some(d) => Ok(d.to_string()),
                None => Err("input ended before setup finished".into()),
            };
        }
        let a = line.trim().to_string();
        Ok(if a.is_empty() {
            default.unwrap_or_default().to_string()
        } else {
            a
        })
    }

    fn pick<'a>(
        &mut self,
        q: &str,
        options: &[Choice<'a>],
        allow_none: bool,
    ) -> Result<Vec<Choice<'a>>, String> {
        for (i, c) in options.iter().enumerate() {
            eprintln!("  {}) {:<18} {}", i + 1, c.name(), c.package.description);
        }
        loop {
            let a = self.ask(q, if allow_none { Some("") } else { None })?;
            if a.is_empty() && allow_none {
                return Ok(Vec::new());
            }
            let mut picked = Vec::new();
            let mut ok = true;
            for part in a.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let found = part
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| options.get(n.wrapping_sub(1)).copied())
                    .or_else(|| options.iter().find(|c| c.name() == part).copied());
                match found {
                    Some(p) => picked.push(p),
                    None => ok = false,
                }
            }
            if ok && !picked.is_empty() && (allow_none || picked.len() == 1) {
                return Ok(picked);
            }
            eprintln!("Please choose from the list.");
        }
    }
}

/// A plugin to offer in `dre init`, and the package it comes in.
#[derive(Clone, Copy)]
struct Choice<'a> {
    package: &'a IndexPackage,
    plugin: &'a PluginId,
}

impl Choice<'_> {
    fn name(&self) -> &str {
        &self.plugin.name
    }
}

/// Every plugin of `kind` the registry offers, by name.
fn choices(index: &Index, kind: PluginKind) -> Vec<Choice<'_>> {
    let mut out: Vec<Choice> = index
        .plugins
        .iter()
        .flat_map(|package| {
            package
                .provides
                .iter()
                .filter(move |p| p.kind == kind)
                .map(move |plugin| Choice { package, plugin })
        })
        .collect();
    out.sort_by(|a, b| a.name().cmp(b.name()));
    out
}

/// The environment variable a secret defaults to: `dre-demo` + `token` → `DRE_DEMO_TOKEN`.
fn env_name(profile: &str, field: &str) -> String {
    format!("{profile}_{field}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The connection profile just set up, whose values a destination on the same platform can reuse.
struct SourceConn<'a> {
    kind: &'a str,
    profile: &'a str,
    fields: &'a Mapping,
}

/// Prompt for a plugin's connection fields; secrets default to an `env_var()` reference.
///
/// Each field takes three short lines (name, description, input) so prompts fit a narrow
/// terminal.
fn connection<R: BufRead>(
    p: &mut Prompter<R>,
    profile: &str,
    fields: &[ConnectionField],
    source: Option<&SourceConn>,
) -> Result<Mapping, String> {
    let mut m = Mapping::new();
    for f in fields.iter().filter(|f| !f.manual) {
        let inherited = source
            .filter(|_| !f.secret)
            .filter(|s| f.same_as_source.as_deref() == Some(s.kind))
            .and_then(|s| {
                s.fields
                    .get(f.name.as_str())
                    .and_then(Value::as_str)
                    .map(|v| (s.profile, v))
            });
        let default = if let Some((_, v)) = inherited {
            Some(v.to_string())
        } else if f.secret {
            Some(env_name(profile, &f.name))
        } else {
            f.default.as_ref().map(|d| match d {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        };
        eprintln!("  {}{}", f.name, if f.required { " (required)" } else { "" });
        if !f.description.is_empty() {
            eprintln!("    {}", f.description);
        }
        if let Some((from, _)) = inherited {
            eprintln!("    Enter keeps the value from `{from}`");
        }
        if !f.choices.is_empty() {
            eprintln!("    One of: {}", f.choices.join(", "));
        }
        if f.secret {
            eprintln!(
                "    A secret: enter the name of the environment variable that holds it (never the value)."
            );
        }
        loop {
            let quiet = if f.secret { QuietInput::new()? } else { None };
            let input = p.input(default.as_deref());
            if quiet.is_some() {
                eprintln!();
            }
            drop(quiet);
            let mut v = input?;
            if v.is_empty() && f.required {
                eprintln!("    `{}` is required.", f.name);
                continue;
            }
            if v.is_empty() {
                break;
            }
            // A secret is stored only as `env_var('NAME')`, never as typed.
            if f.secret {
                let is_name = v
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !is_name {
                    eprintln!(
                        "    That isn't an environment variable name; secrets aren't stored in profiles.yml. Enter the variable's name, e.g. {}.",
                        env_name(profile, &f.name)
                    );
                    continue;
                }
                v = format!("{{{{ env_var('{v}') }}}}");
            }
            // Checked as the plugin will check it: a choice, or a whole number.
            if !f.choices.is_empty() && !f.choices.contains(&v) && !v.contains("{{") {
                eprintln!("    `{}` must be one of {}.", f.name, f.choices.join(", "));
                continue;
            }
            let value = match f.kind {
                Some(dre_protocol::msg::FieldKind::Integer) if !v.contains("{{") => match v.parse::<i64>() {
                    Ok(n) => Value::Number(n.into()),
                    Err(_) => {
                        eprintln!("    `{}` must be a whole number.", f.name);
                        continue;
                    }
                },
                _ => Value::String(v),
            };
            m.insert(f.name.clone(), value);
            break;
        }
    }
    Ok(m)
}

fn install_and_describe(
    ctx: &dre_core::Context,
    choice: Choice,
    printer: &Printer,
) -> Result<Vec<ConnectionField>, String> {
    let package = choice.package;
    let v = package.best(&semver::VersionReq::STAR).ok_or_else(|| {
        format!(
            "no release of the plugin package `{}` for {}",
            package.name,
            manager::platform()
        )
    })?;
    let dir = dre_core::plugins::plugins_dir(ctx.env(), None);
    let vdir = dre_core::plugins::version_dir(&dir, &package.name, &v.version);
    // A package already installed for an earlier choice isn't downloaded again.
    if dre_core::plugins::read_manifest(&vdir).is_none() {
        let locked = manager::install(ctx, &dir, package, v, None)?;
        printer.line(
            Tone::Good,
            "Installed",
            &format!("plugin package `{}` {}", package.name, locked.version),
        );
    }
    let m = dre_core::plugins::read_manifest(&vdir)
        .ok_or_else(|| format!("{} has no plugin manifest after installing", vdir.display()))?;
    let path = vdir.join(&m.executable);
    let plugin = dre_core::plugins::Located {
        path,
        plugin: choice.plugin.clone(),
        package: package.name.clone(),
    };
    let mut proc_ = ctx
        .host()
        .start(&plugin, std::sync::Arc::new(|_, _| {}), None)
        .map_err(|e| e.to_string())?;
    let fields = proc_.describe().map(|d| d.connection_fields);
    proc_.close();
    fields.map_err(|e| e.to_string())
}

/// Add one profile to its section of profiles.yml, refusing to overwrite an existing one.
/// Edits the text rather than re-serialising, so the user's comments and layout survive.
fn add_profile(
    path: &Path,
    role: Role,
    name: &str,
    target: &str,
    kind: &str,
    fields: Mapping,
) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = dre_core::config::node::parse(&existing).ok().map(|n| n.to_json());
    // A DRE 0.1 file keeps its connections under `sources:`; add to that rather than start a
    // second section.
    let section = match &parsed {
        Some(v)
            if role == Role::Connection
                && v.get(role.section()).is_none()
                && v.get(dre_core::profiles::OLD_CONNECTIONS_SECTION).is_some() =>
        {
            dre_core::profiles::OLD_CONNECTIONS_SECTION
        }
        _ => role.section(),
    };
    if let Some(v) = &parsed
        && v.get(section).and_then(|s| s.get(name)).is_some()
    {
        return Err(format!(
            "{} profile `{name}` already exists in {}",
            role.as_str(),
            path.display()
        ));
    }
    let mut settings = Mapping::new();
    settings.insert("type".into(), Value::String(kind.into()));
    settings.extend(fields);
    let mut targets = Mapping::new();
    targets.insert(target.into(), Value::Object(settings));
    let mut profile = Mapping::new();
    // Each profile's entry defaults to `dev`; any other name becomes this profile's default.
    if target != dre_core::profiles::DEFAULT_TARGET {
        profile.insert("target".into(), Value::String(target.into()));
    }
    profile.insert("targets".into(), Value::Object(targets));
    let mut root = Mapping::new();
    root.insert(name.into(), Value::Object(profile));
    let block: String = dre_core::config::to_yaml(&root)?
        .lines()
        .map(|l| format!("  {l}\n"))
        .collect();

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let header = format!("{section}:");
    match lines.iter().position(|l| l.trim_end() == header) {
        Some(start) => {
            // The section runs until the next top-level key; add the profile at its end.
            // `start` is a line's index, so `start + 1 <= lines.len()`.
            let mut end = lines[start + 1..]
                .iter()
                .position(|l| !l.is_empty() && !l.starts_with([' ', '#']))
                .map_or(lines.len(), |i| start + 1 + i);
            while end > start + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            let mut insert: Vec<String> = Vec::new();
            if end > start + 1 {
                insert.push(String::new());
            }
            insert.extend(block.lines().map(str::to_string));
            lines.splice(end..end, insert);
        }
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(header);
            lines.extend(block.lines().map(str::to_string));
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    // The splice can't follow every hand-written layout (a comment after the section key, other
    // indentation): write only a file that still parses and holds the new profile.
    let added = dre_core::config::node::parse(&text)
        .ok()
        .is_some_and(|n| n.to_json().get(section).and_then(|s| s.get(name)).is_some());
    if !added {
        return Err(format!(
            "can't add the profile to {} without changing its layout; add this block by hand under `{section}:`:\n{block}",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    dre_core::fs::write_atomic(path, text.as_bytes())
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// `dre init`.
pub fn init(ctx: &dre_core::Context, profiles_dir: Option<&Path>, printer: &Printer) -> ExitCode {
    match init_inner(ctx, profiles_dir, printer, std::io::stdin().lock()) {
        Ok(()) => crate::exit::ok(),
        Err(e) => {
            printer.error(&e);
            crate::exit::failed()
        }
    }
}

fn init_inner(
    ctx: &dre_core::Context,
    profiles_dir: Option<&Path>,
    printer: &Printer,
    input: impl BufRead,
) -> Result<(), String> {
    let mut p = Prompter { input };
    let index = Index::load(ctx)?;
    let profiles_path =
        dre_core::profiles::profiles_dir(profiles_dir, ctx.env()).join(dre_core::profiles::PROFILES_FILE);
    eprintln!("Welcome to DRE. This sets up a connection, installs its plugin and can start a project.\n");

    let sources = choices(&index, PluginKind::Source);
    if sources.is_empty() {
        return Err("the plugin registry lists no sources".into());
    }
    eprintln!("Which database do you want to report from?");
    // Without `allow_none`, `pick` returns exactly one choice.
    let source = p.pick("Source (number or name)", &sources, false)?[0];
    let fields = install_and_describe(ctx, source, printer)?;
    let profile = p.ask("Name for this connection profile", Some("warehouse"))?;
    let target = p.ask("Target (environment) name", Some("dev"))?;
    eprintln!("\nConnection details for `{}`.", source.name());
    eprintln!("For secrets, enter only an environment variable name; DRE stores an env_var() reference.\n");
    let conn = connection(&mut p, &profile, &fields, None)?;
    add_profile(
        &profiles_path,
        Role::Connection,
        &profile,
        &target,
        source.name(),
        conn.clone(),
    )?;
    let source_conn = SourceConn {
        kind: source.name(),
        profile: &profile,
        fields: &conn,
    };
    printer.line(
        Tone::Good,
        "Saved",
        &format!("profile `{profile}` to {}", profiles_path.display()),
    );

    let dests = choices(&index, PluginKind::Destination);
    let mut destinations = Vec::new();
    let mut packages = vec![source.package.name.clone()];
    if !dests.is_empty() {
        eprintln!(
            "\nWhere should reports be delivered? A copy always stays in target/.\n\
             Pick any number (e.g. 1,3), or press Enter for none."
        );
        for d in p.pick("Destinations", &dests, true)? {
            let fields = install_and_describe(ctx, d, printer)?;
            let name = p.ask(
                &format!("Profile name for `{}`", d.name()),
                Some(&format!("{}_{}", d.name(), "out")),
            )?;
            eprintln!("\nConnection details for `{}`.\n", d.name());
            let conn = connection(&mut p, &name, &fields, Some(&source_conn))?;
            add_profile(&profiles_path, Role::Destination, &name, &target, d.name(), conn)?;
            printer.line(
                Tone::Good,
                "Saved",
                &format!("profile `{name}` to {}", profiles_path.display()),
            );
            destinations.push((d.name().to_string(), name));
            packages.push(d.package.name.clone());
        }
    }

    let yes = p.ask("\nCreate a starter project now? (y/n)", Some("y"))?;
    if yes.eq_ignore_ascii_case("y") || yes.eq_ignore_ascii_case("yes") {
        let dir = PathBuf::from(p.ask("Project directory", Some("my_reports"))?);
        let s = Scaffold {
            name: project_name(&dir),
            profile,
            destinations,
            packages,
        };
        let files = scaffold(&dir, &s)?;
        printer.line(
            Tone::Good,
            "Created",
            &format!("{} ({} files)", dir.display(), files.len()),
        );
        printer.line(Tone::Note, "Next", &format!("cd {} && dre run", dir.display()));
    } else {
        printer.line(Tone::Note, "Next", "run `dre new <dir>` when you want a project");
    }
    Ok(())
}
