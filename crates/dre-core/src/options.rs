//! Plugin options: a format's `output:` keys and a destination entry's keys. Core owns only the
//! keys every output shares (`format`, `destination`, `template`, `extension`, `profile`,
//! `path`); everything else belongs to the plugin, which checks it (the protocol's `validate`).
//! A new format or destination therefore needs no change to core.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use dre_protocol::host::LogSink;
use serde_json::{Map, Value};

use crate::Diagnostics;
use crate::codes::Code;
use crate::profiles::{LOCAL_TYPE, Role};
use crate::project::{PluginKind, Project};
use crate::run::find_plugin;

/// Parse `B12` into zero-based `(row, col)`.
pub use dre_protocol::util::parse_cell;

pub fn is_cell(s: &str) -> bool {
    parse_cell(s).is_some()
}

/// Where a config block is used: the report (and Set), its file, and for a destination, the
/// profile it names.
struct Use {
    ctx: String,
    file: PathBuf,
    profile: Option<String>,
}

/// One destination entry: its output's format, and whether it attaches other outputs.
struct Route {
    at: Use,
    format: String,
    attach: bool,
}

/// One plugin's distinct option blocks (keyed by their JSON), each with where it's used.
type Blocks = BTreeMap<String, (Map<String, Value>, Vec<Use>)>;

/// Ask each format and destination plugin to check every config block the project gives it,
/// found by kind and name alone: core knows nothing of any plugin's options. Each destination
/// profile's output is its entry for the run. A declared plugin that can't be found is an error, or
/// a warning with `offline` (`dre validate --no-auto-install`, which doesn't install plugins).
pub fn check(project: &Project, offline: bool, diags: &mut Diagnostics) {
    // (kind, plugin) -> distinct option blocks -> where each is used.
    let mut blocks: BTreeMap<(PluginKind, String), Blocks> = BTreeMap::new();
    // Destination type -> what each entry of it sends: checked against its capabilities.
    let mut routes: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut add = |kind, name: &str, options: &Map<String, Value>, u: Use| {
        let key = serde_json::to_string(options).unwrap_or_default();
        blocks
            .entry((kind, name.to_string()))
            .or_default()
            .entry(key)
            .or_insert_with(|| (options.clone(), Vec::new()))
            .1
            .push(u);
    };
    for (format, options) in &project.format_options {
        let u = Use {
            ctx: format!("`format_options.{format}`"),
            file: PathBuf::from(crate::project::PROJECT_FILE),
            profile: None,
        };
        add(PluginKind::Format, format, options, u);
    }
    for r in &project.reports {
        for b in &r.bindings {
            let ctx = match &b.set {
                Some(s) => format!("report `{}`, Set `{s}`", r.name),
                None => format!("report `{}`", r.name),
            };
            let at = |profile: Option<&str>| Use {
                ctx: ctx.clone(),
                file: r.file.clone(),
                profile: profile.map(str::to_string),
            };
            for o in b.outputs.iter().filter(|o| !o.is_message()) {
                add(PluginKind::Format, &o.format, &o.options, at(None));
            }
            let message_of: Vec<&crate::project::Output> = b
                .outputs
                .iter()
                .flat_map(|o| o.destinations.iter().map(move |_| o))
                .collect();
            for (i, d) in b.destinations().enumerate() {
                // The profile as the parse pass rendered it (a Jinja `profile:`).
                let rendered = b
                    .parsed
                    .as_ref()
                    .and_then(|p| p.destinations.get(i).cloned().flatten());
                let profile = rendered.as_deref().unwrap_or(&d.profile);
                let kind = if project.profiles.is_builtin_local(profile) {
                    LOCAL_TYPE
                } else {
                    match project.profiles.target(Role::Destination, profile) {
                        Some(t) => t.kind.as_str(),
                        // A missing profile or entry is reported elsewhere; `deliver: false` takes no options.
                        None => continue,
                    }
                };
                add(PluginKind::Destination, kind, &d.options, at(Some(profile)));
                routes.entry(kind.to_string()).or_default().push(Route {
                    at: at(Some(profile)),
                    format: message_of[i].format.clone(),
                    attach: !d.attach.is_empty(),
                });
            }
        }
    }

    let log: LogSink = Arc::new(|_, _| {});
    for ((kind, name), blocks) in blocks {
        let report = |diags: &mut Diagnostics, u: &Use, e: &str| {
            let (code, msg) = match &u.profile {
                Some(p) => (
                    Code::InvalidDestinationOption,
                    format!("{}: destination `{p}`: {e}", u.ctx),
                ),
                None => (Code::InvalidOutputOption, format!("{}: {e}", u.ctx)),
            };
            diags.error(code, Some(u.file.clone()), None, msg);
        };
        // Core's own destination.
        if kind == PluginKind::Destination && name == LOCAL_TYPE {
            for r in routes.get(&name).into_iter().flatten().filter(|r| r.attach) {
                report(
                    diags,
                    &r.at,
                    "`attach:` needs a destination that takes messages and files, but `local` takes files only",
                );
            }
            for (options, uses) in blocks.values() {
                for e in local_option_errors(options) {
                    for u in uses {
                        report(diags, u, &e);
                    }
                }
            }
            continue;
        }
        let plugin = match find_plugin(project, kind, &name) {
            Ok(p) => p,
            // Already an error (`undeclared-plugin`).
            Err(crate::plugins::LocateError::NotProvided(_)) => continue,
            Err(e) => {
                let first = blocks.values().flat_map(|(_, u)| u).next();
                let file = first.map(|u| u.file.clone());
                let at = first.map(|u| format!("{}: ", u.ctx)).unwrap_or_default();
                if offline {
                    diags.warning(
                        Code::OptionsUnchecked,
                        file,
                        None,
                        format!("{at}{kind} `{name}`'s options weren't checked: {e}"),
                    );
                } else {
                    diags.error(
                        Code::PluginNotFound,
                        file,
                        None,
                        format!("{at}{kind} `{name}` has no plugin to check it against: {e}"),
                    );
                }
                continue;
            }
        };
        let mut p = match plugin.start(log.clone(), Some(&project.root)) {
            Ok(p) => p,
            Err(e) => {
                diags.warning(
                    Code::OptionsUnchecked,
                    None,
                    None,
                    format!("can't check the options of {kind} `{name}`: {e}"),
                );
                continue;
            }
        };
        if kind == PluginKind::Destination {
            for r in routes.get(&name).into_iter().flatten() {
                let message = r.format == crate::project::MESSAGE_FORMAT;
                let problem = if p.has(dre_protocol::CAP_MESSAGE_ONLY) && !message {
                    Some(format!(
                        "`{name}` only takes messages, but this output is `{}`; deliver the file to a file destination (s3, sftp, a Databricks Volume...) and link it from a message output with `outputs.<name>.location`",
                        r.format
                    ))
                } else if r.attach && p.has(dre_protocol::CAP_MESSAGE_ONLY) {
                    Some(format!(
                        "`attach:` needs a destination that takes files, but `{name}` only takes messages; deliver the files elsewhere and link them with `outputs.<name>.location`"
                    ))
                } else if r.attach && !p.has(dre_protocol::CAP_MESSAGE) {
                    Some(format!(
                        "`attach:` needs a destination that takes messages and files, but `{name}` takes files only"
                    ))
                } else {
                    None
                };
                if let Some(e) = problem {
                    report(diags, &r.at, &e);
                }
            }
        }
        if !p.has(dre_protocol::CAP_VALIDATE) {
            if blocks.values().any(|(o, _)| !o.is_empty()) {
                diags.warning(
                    Code::OptionsUnchecked,
                    None,
                    None,
                    format!(
                        "{kind} `{name}` {} predates option checks, so its options weren't checked; run `dre plugin update {kind}/{name}`",
                        p.info().version
                    ),
                );
            }
            let _ = p.close();
            continue;
        }
        for (options, uses) in blocks.into_values() {
            match p.validate(options) {
                Ok(errors) => {
                    for u in &uses {
                        for e in &errors {
                            report(diags, u, e);
                        }
                    }
                }
                Err(e) => {
                    diags.warning(
                        Code::OptionsUnchecked,
                        None,
                        None,
                        format!("{kind} `{name}` failed to check its options: {e}"),
                    );
                    break;
                }
            }
        }
        let _ = p.close();
    }
}

/// The options core's built-in `local` destination takes: the shared delivery rules' `atomic`
/// and `temp_dir`.
pub fn local_options() -> Vec<dre_protocol::options::OptionField> {
    dre_protocol::delivery::option_fields()
        .into_iter()
        .filter(|f| f.name == "atomic" || f.name == "temp_dir")
        .collect()
}

/// Every problem with a `local` destination entry's options.
pub fn local_option_errors(options: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    dre_protocol::options::check(dre_protocol::Kind::Destination, LOCAL_TYPE, &local_options(), options)
}
