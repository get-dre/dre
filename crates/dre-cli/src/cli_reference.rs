//! The CLI reference page, `docs/cli-reference.md`, generated from the command definitions so it
//! never drifts from the real CLI. A test fails when the committed page is stale;
//! `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --bin dre cli_reference` rewrites it.

use clap::{Arg, Command};

/// Flags with an environment variable behind them (see `dre_core::settings`).
const FLAG_ENV: &[(&str, &str)] = &[
    ("target", dre_core::settings::TARGET),
    ("target_path", dre_core::settings::TARGET_PATH),
    ("profiles_dir", dre_core::settings::PROFILES_DIR),
    ("timezone", dre_core::settings::TIMEZONE),
];

/// A short example per command, by its path after `dre`.
const EXAMPLES: &[(&str, &str)] = &[
    ("validate", "dre validate --live monthly_revenue"),
    ("run", "dre run monthly_revenue --set client_a --target prod"),
    ("compile", "dre compile tag:finance"),
    ("clean", "dre clean"),
    ("ls", "dre ls --schedule daily"),
    ("schedule ls", "dre schedule ls --from 2026-11-01 --to 2026-12-01"),
    ("init", "dre init"),
    ("new", "dre new my_reports --type postgres"),
    ("deps", "dre deps"),
    ("plugin list", "dre plugin list"),
    ("plugin install", "dre plugin install xlsx@^1"),
    ("plugin update", "dre plugin update postgres"),
    ("plugin remove", "dre plugin remove duckdb@1.1.0"),
    ("system update", "dre system update --check"),
    ("explain", "dre explain unknown-key"),
    ("history", "dre history monthly_revenue --latest --path"),
    ("unlock", "dre unlock monthly_revenue --binding client_a"),
];

/// The page for `cli`, the top-level command.
pub fn page(mut cli: Command) -> String {
    cli.build();
    let mut out = String::from(
        "---\ntitle: \"CLI reference\"\ndescription: \"Every dre command, subcommand and flag, with defaults and environment variables.\"\nsection: reference\nposition: 1\n---\n\n\
         # CLI reference\n\n\
         <!-- Generated from the CLI definitions by crates/dre-cli/src/cli_reference.rs. Edit the code, not this page. -->\n\n\
         Every `dre` command and flag. `dre <command> --help` prints the same text. Every command \
         exits with one of the documented [exit codes](exit-codes.md); each problem it reports has an \
         [error code](reference-error-codes.md). \
         Settings that also come from the environment are listed with their variable; every \
         variable is on the [environment variables](environment-variables.md) page.\n",
    );
    out.push_str("\n## Global options\n\nThese work with every command.\n\n");
    let globals: Vec<&Arg> = cli.get_arguments().filter(|a| shown(a)).collect();
    out.push_str(&options_table(&globals));
    for sub in cli
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
    {
        command(&mut out, sub.clone(), &[]);
    }
    out
}

fn shown(a: &Arg) -> bool {
    !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version")
}

fn command(out: &mut String, mut cmd: Command, parents: &[&str]) {
    let mut path: Vec<&str> = parents.to_vec();
    let name = cmd.get_name().to_string();
    path.push(&name);
    let full = path.join(" ");
    out.push_str(&format!("\n## `dre {full}`\n\n"));
    let about = sentence(
        &cmd.get_long_about()
            .or(cmd.get_about())
            .map(|s| s.to_string())
            .unwrap_or_default(),
    );
    out.push_str(&format!("{about}\n\n"));
    let subs: Vec<Command> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .cloned()
        .collect();
    if !subs.is_empty() {
        out.push_str("Subcommands:\n\n");
        for s in &subs {
            let about = sentence(&s.get_about().map(|a| a.to_string()).unwrap_or_default());
            out.push_str(&format!(
                "- [`dre {full} {}`](#dre-{}-{}): {about}\n",
                s.get_name(),
                path.join("-"),
                s.get_name()
            ));
        }
        for s in subs {
            command(out, s, &path);
        }
        return;
    }
    let usage = cmd.render_usage().to_string();
    let usage = usage.trim_start_matches("Usage: ").trim();
    out.push_str(&format!("```text\n{usage}\n```\n\n"));
    let positionals: Vec<&Arg> = cmd
        .get_arguments()
        .filter(|a| a.is_positional() && shown(a) && !a.is_global_set())
        .collect();
    if !positionals.is_empty() {
        out.push_str("| Argument | Description |\n|---|---|\n");
        for a in positionals {
            let names = a
                .get_value_names()
                .map(|v| v.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| a.get_id().to_string().to_uppercase());
            let many = if a.get_num_args().is_some_and(|n| n.max_values() > 1) {
                "..."
            } else {
                ""
            };
            out.push_str(&format!("| `{names}{many}` | {} |\n", help(a)));
        }
        out.push('\n');
    }
    let options: Vec<&Arg> = cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && shown(a) && !a.is_global_set())
        .collect();
    if !options.is_empty() {
        out.push_str(&options_table(&options));
        out.push('\n');
    }
    if let Some((_, ex)) = EXAMPLES.iter().find(|(p, _)| *p == full) {
        out.push_str(&format!("Example:\n\n```bash\n{ex}\n```\n"));
    }
}

fn options_table(args: &[&Arg]) -> String {
    let mut out = String::from("| Option | Default | Description |\n|---|---|---|\n");
    for a in args {
        let mut flag = Vec::new();
        if let Some(s) = a.get_short() {
            flag.push(format!("-{s}"));
        }
        if let Some(l) = a.get_long() {
            flag.push(format!("--{l}"));
        }
        let mut flag = flag.join(", ");
        if a.get_action().takes_values() {
            let value = a
                .get_value_names()
                .map(|v| v.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| a.get_id().to_string().to_uppercase());
            let optional = a.get_num_args().is_some_and(|n| n.min_values() == 0);
            flag.push_str(&if optional {
                format!(" [<{value}>]")
            } else {
                format!(" <{value}>")
            });
        }
        // A switch's `false` says nothing.
        let defaults: Vec<String> = if a.get_action().takes_values() {
            a.get_default_values()
                .iter()
                .map(|v| format!("`{}`", v.to_string_lossy()))
                .collect()
        } else {
            Vec::new()
        };
        let mut desc = help(a);
        let choices: Vec<String> = a
            .get_possible_values()
            .iter()
            .filter(|p| !p.is_hide_set())
            .map(|p| format!("`{}`", p.get_name()))
            .collect();
        if !choices.is_empty() && a.get_action().takes_values() {
            desc.push_str(&format!(" One of {}.", choices.join(", ")));
        }
        if let Some((_, env)) = FLAG_ENV.iter().find(|(id, _)| a.get_id().as_str() == *id) {
            desc.push_str(&format!(" Environment: `{env}`."));
        }
        let aliases: Vec<String> = a
            .get_visible_aliases()
            .unwrap_or_default()
            .iter()
            .map(|al| format!("`--{al}`"))
            .collect();
        if !aliases.is_empty() {
            desc.push_str(&format!(" Also {}.", aliases.join(", ")));
        }
        out.push_str(&format!("| `{flag}` | {} | {desc} |\n", defaults.join(" ")));
    }
    out
}

/// An argument's help as one table cell.
fn help(a: &Arg) -> String {
    let text = a
        .get_long_help()
        .or(a.get_help())
        .map(|s| s.to_string())
        .unwrap_or_default();
    sentence(
        &text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('|', "\\|"),
    )
}

/// `text` ending in a full stop (clap drops the doc comment's last one).
fn sentence(text: &str) -> String {
    let t = text.trim_end();
    if t.is_empty() || t.ends_with(['.', '?', '!', ':']) {
        t.to_string()
    } else {
        format!("{t}.")
    }
}
