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
use crate::plugin_host::find_plugin;
use crate::profiles::{LOCAL_TYPE, ProfileTarget, Role};
use crate::project::{PluginKind, Project};

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

/// One plugin's distinct option blocks (keyed by their JSON and template), each with its
/// template check (html and pdf outputs) and where it's used.
type Blocks = BTreeMap<String, (Map<String, Value>, Option<Value>, Vec<Use>)>;

/// Every config block the project gives each plugin: `(kind, plugin)` -> distinct option blocks
/// -> where each is used.
type ByPlugin = BTreeMap<(PluginKind, String), Blocks>;

/// Destination type -> what each entry of it sends: checked against its capabilities.
type Routes = BTreeMap<String, Vec<Route>>;

/// Ask each format and destination plugin to check every config block the project gives it,
/// found by kind and name alone: core knows nothing of any plugin's options. Each destination
/// profile's output is its entry for the run. A declared plugin that can't be found is an error, or
/// a warning with `offline` (`dre validate --no-auto-install`, which doesn't install plugins).
pub fn check(ctx: &crate::Context, project: &Project, offline: bool, diags: &mut Diagnostics) {
    let (by_plugin, routes) = collect(project);
    for ((kind, name), blocks) in by_plugin {
        // Core's own destination.
        if kind == PluginKind::Destination && name == LOCAL_TYPE {
            check_local(routes.get(&name), &blocks, diags);
            continue;
        }
        let Some(mut p) = start_checker(ctx, project, kind, &name, &blocks, offline, diags) else {
            continue;
        };
        if kind == PluginKind::Destination {
            check_routes(p.as_ref(), &name, routes.get(&name), diags);
        }
        validate_blocks(p.as_mut(), kind, &name, blocks, diags);
        p.close();
    }
}

/// Every config block the project gives each plugin, and every destination entry's route.
fn collect(project: &Project) -> (ByPlugin, Routes) {
    let mut blocks = ByPlugin::new();
    let mut routes = Routes::new();
    for (format, options) in project.format_options() {
        let u = Use {
            ctx: format!("`format_options.{format}`"),
            file: PathBuf::from(crate::project::PROJECT_FILE),
            profile: None,
        };
        add(&mut blocks, PluginKind::Format, format, options, None, u);
    }
    for r in project.reports() {
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
            collect_formats(project, r, b, &mut blocks, &at);
            collect_destinations(project, b, &mut blocks, &mut routes, &at);
        }
    }
    (blocks, routes)
}

/// Add a config block of plugin `name` of `kind`, used at `u`.
fn add(
    blocks: &mut ByPlugin,
    kind: PluginKind,
    name: &str,
    options: &Map<String, Value>,
    template: Option<Value>,
    u: Use,
) {
    let key = format!(
        "{}{}",
        serde_json::to_string(options).unwrap_or_default(),
        template.as_ref().map(Value::to_string).unwrap_or_default()
    );
    blocks
        .entry((kind, name.to_string()))
        .or_default()
        .entry(key)
        .or_insert_with(|| (options.clone(), template, Vec::new()))
        .2
        .push(u);
}

/// Each file output's format options, with an html or pdf output's template check.
fn collect_formats(
    project: &Project,
    r: &crate::project::Report,
    b: &crate::project::Binding,
    blocks: &mut ByPlugin,
    at: &dyn Fn(Option<&str>) -> Use,
) {
    for o in b.outputs.iter().filter(|o| !o.is_message()) {
        let template = crate::formats::Format::new(&o.format)
            .is_document()
            .then(|| document_check(project, r, b, o));
        if template.as_ref().is_some_and(|t| t["file"].is_null()) && o.template.is_some() {
            // Its file is missing: nothing to check.
            add(blocks, PluginKind::Format, &o.format, &o.options, None, at(None));
            continue;
        }
        add(
            blocks,
            PluginKind::Format,
            &o.format,
            &o.options,
            template,
            at(None),
        );
    }
}

/// What a document format checks an output's template against.
fn document_check(
    project: &Project,
    r: &crate::project::Report,
    b: &crate::project::Binding,
    o: &crate::project::Output,
) -> Value {
    // A file that can't be resolved is reported by the template checks.
    let file = o
        .template
        .as_ref()
        .and_then(|t| crate::project::resolve_template(project.root(), &r.file, &t.file).ok());
    let queries: Vec<String> = b
        .queries
        .iter()
        .filter(|q| q.tab && o.feeds(&q.query))
        .map(|q| q.query.clone())
        .collect();
    let abs = |p: &std::path::Path| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    serde_json::to_value(crate::document::Check {
        file: file.as_deref().map(abs),
        project_root: abs(project.root()),
        queries,
    })
    .unwrap_or_default()
}

/// Each destination entry's options, and what it sends.
fn collect_destinations(
    project: &Project,
    b: &crate::project::Binding,
    blocks: &mut ByPlugin,
    routes: &mut Routes,
    at: &dyn Fn(Option<&str>) -> Use,
) {
    for (key, d) in b.keyed_destinations() {
        let output = &b.outputs[key.output];
        // The profile as the parse pass rendered it (a Jinja `profile:`).
        let rendered = b.parsed.as_ref().and_then(|p| p.destination(key));
        let profile = rendered.unwrap_or(&d.profile);
        let kind = if project.profiles().is_builtin_local(profile) {
            LOCAL_TYPE
        } else {
            match project.profiles().target(Role::Destination, profile) {
                Some(t) => t.kind.as_str(),
                // A missing profile or entry is reported elsewhere; `deliver: false` takes no options.
                None => continue,
            }
        };
        add(
            blocks,
            PluginKind::Destination,
            kind,
            &d.options,
            None,
            at(Some(profile)),
        );
        routes.entry(kind.to_string()).or_default().push(Route {
            at: at(Some(profile)),
            format: output.format.clone(),
            // A zipping entry packs its attachments into the zip: any destination takes it.
            attach: !d.attach.is_empty() && d.zip.is_none(),
        });
    }
}

/// An option problem `e` where it's used.
fn report(diags: &mut Diagnostics, u: &Use, e: &str) {
    let (code, msg) = match &u.profile {
        Some(p) => (
            Code::InvalidDestinationOption,
            format!("{}: destination `{p}`: {e}", u.ctx),
        ),
        None => (Code::InvalidOutputOption, format!("{}: {e}", u.ctx)),
    };
    diags.error(code, Some(u.file.clone()), None, msg);
}

/// Core's own `local` destination: its options, and no `attach:`.
fn check_local(routes: Option<&Vec<Route>>, blocks: &Blocks, diags: &mut Diagnostics) {
    for r in routes.into_iter().flatten().filter(|r| r.attach) {
        report(
            diags,
            &r.at,
            "`attach:` needs a destination that takes messages and files, but `local` takes files only",
        );
    }
    for (options, _, uses) in blocks.values() {
        for e in local_option_errors(options) {
            for u in uses {
                report(diags, u, &e);
            }
        }
    }
}

/// Start the plugin that checks `blocks`; `None` after saying why it couldn't.
fn start_checker(
    ctx: &crate::Context,
    project: &Project,
    kind: PluginKind,
    name: &str,
    blocks: &Blocks,
    offline: bool,
    diags: &mut Diagnostics,
) -> Option<Box<dyn crate::plugin_host::Plugin>> {
    let plugin = match find_plugin(ctx, project, kind, name) {
        Ok(p) => p,
        // Already an error (`undeclared-plugin`).
        Err(crate::plugins::LocateError::NotProvided(_)) => return None,
        Err(e) => {
            let first = blocks.values().flat_map(|(_, _, u)| u).next();
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
            return None;
        }
    };
    let log: LogSink = Arc::new(|_, _| {});
    match ctx.host().start(&plugin, log, Some(project.root())) {
        Ok(p) => Some(p),
        Err(e) => {
            diags.warning(
                Code::OptionsUnchecked,
                None,
                None,
                format!("can't check the options of {kind} `{name}`: {e}"),
            );
            None
        }
    }
}

/// Each entry of destination `name` against what it can take: messages, files, attachments.
fn check_routes(
    p: &dyn crate::plugin_host::Plugin,
    name: &str,
    routes: Option<&Vec<Route>>,
    diags: &mut Diagnostics,
) {
    for r in routes.into_iter().flatten() {
        let message = crate::formats::Format::new(&r.format).is_message();
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

/// Ask plugin `p` to check each block, reporting its errors and warnings where it's used.
fn validate_blocks(
    p: &mut dyn crate::plugin_host::Plugin,
    kind: PluginKind,
    name: &str,
    blocks: Blocks,
    diags: &mut Diagnostics,
) {
    if !p.has(dre_protocol::CAP_VALIDATE) {
        if blocks.values().any(|(o, _, _)| !o.is_empty()) {
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
        return;
    }
    for (options, template, uses) in blocks.into_values() {
        match p.validate(options, template) {
            Ok((errors, warnings)) => {
                for u in &uses {
                    for e in &errors {
                        report(diags, u, e);
                    }
                    for w in &warnings {
                        let at = format!("{}: {w}", u.ctx);
                        diags.warning(Code::InvalidTemplate, Some(u.file.clone()), None, at);
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
}

/// The options core's built-in `local` destination takes: the shared delivery rules'
/// `if_exists`, `atomic` and `temp_dir`.
pub fn local_options() -> Vec<dre_protocol::options::OptionField> {
    dre_protocol::delivery::option_fields()
}

/// Every problem with a `local` destination entry's options.
pub fn local_option_errors(options: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    dre_protocol::options::check(
        dre_protocol::Kind::Destination,
        LOCAL_TYPE,
        &local_options(),
        options,
    )
}

/// One profile entry to check: its role, profile and entry (target) name.
pub type EntryRef = (Role, String, String);

/// Every entry of every profile, for `dre validate --all-targets`.
pub fn all_entries(project: &Project) -> Vec<EntryRef> {
    let mut out = Vec::new();
    for (role, profiles) in [
        (Role::Connection, &project.profiles().connections),
        (Role::Destination, &project.profiles().destinations),
    ] {
        for (name, p) in profiles {
            for target in p.targets.keys() {
                out.push((role, name.clone(), target.clone()));
            }
        }
    }
    out
}

/// Ask each entry's plugin to check its connection settings without connecting (the protocol's
/// `validate_connection`). A value from an unset `env_var()` is reported once as an
/// `unset-env-var` warning (CI often validates without secrets) and sent unresolved, so it isn't
/// reported again as missing.
pub fn check_connections(
    ctx: &crate::Context,
    project: &Project,
    entries: &[EntryRef],
    diags: &mut Diagnostics,
) {
    let log: LogSink = Arc::new(|_, _| {});
    let file = project.profiles().file.clone();
    let mut unset_reported = std::collections::BTreeSet::new();
    for ((kind, name), entries) in entries_by_plugin(project, entries) {
        // A missing plugin is reported by the plugin checks.
        let Ok(plugin) = find_plugin(ctx, project, kind, &name) else {
            continue;
        };
        let mut p = match ctx.host().start(&plugin, log.clone(), Some(project.root())) {
            Ok(p) => p,
            Err(e) => {
                diags.warning(
                    Code::OptionsUnchecked,
                    file.clone(),
                    None,
                    format!("can't check the settings of {kind} `{name}`: {e}"),
                );
                continue;
            }
        };
        for (entry, t) in entries {
            let line = project.profiles().line_of(entry.0, &entry.1);
            let at = format!("{} `{}` (entry `{}`)", entry.0.as_str(), entry.1, entry.2);
            let Some((rendered, unresolved)) =
                settings_to_check(ctx, t, &at, line, &file, &mut unset_reported, diags)
            else {
                continue;
            };
            if !report_connection_check(
                p.as_mut(),
                (kind, &name),
                rendered,
                unresolved,
                &at,
                (&file, line),
                diags,
            ) {
                break;
            }
        }
        p.close();
    }
}

/// Each plugin and the entries it checks, so each plugin starts once.
fn entries_by_plugin<'e>(
    project: &'e Project,
    entries: &'e [EntryRef],
) -> BTreeMap<(PluginKind, String), Vec<(&'e EntryRef, &'e ProfileTarget)>> {
    let mut by_plugin: BTreeMap<(PluginKind, String), Vec<(&EntryRef, &ProfileTarget)>> = BTreeMap::new();
    for e in entries {
        let (role, profile, target) = e;
        let Some(t) = project
            .profiles()
            .get(*role, profile)
            .and_then(|p| p.targets.get(target))
        else {
            continue;
        };
        if t.kind == LOCAL_TYPE {
            continue;
        }
        let kind = match role {
            Role::Connection => PluginKind::Source,
            Role::Destination => PluginKind::Destination,
        };
        by_plugin.entry((kind, t.kind.clone())).or_default().push((e, t));
    }
    by_plugin
}

/// An entry's settings as the plugin checks them: values with an unset `env_var()` go
/// unresolved (warned about once per variable), the rest rendered as for a run. `None` when
/// they don't render (reported).
fn settings_to_check(
    ctx: &crate::Context,
    t: &ProfileTarget,
    at: &str,
    line: Option<usize>,
    file: &Option<PathBuf>,
    unset_reported: &mut std::collections::BTreeSet<String>,
    diags: &mut Diagnostics,
) -> Option<(Map<String, Value>, Vec<String>)> {
    let mut known = t.clone();
    let mut unresolved = Vec::new();
    for (k, v) in &t.fields {
        let unset: Vec<String> = unset_env_vars(v, ctx.env());
        if unset.is_empty() {
            continue;
        }
        known.fields.insert(k.clone(), Value::Null);
        unresolved.push(k.clone());
        for var in unset {
            // A warning: CI often validates without the secrets a run needs.
            if unset_reported.insert(var.clone()) {
                diags.warning(
                    Code::UnsetEnvVar,
                    file.clone(),
                    line,
                    format!(
                        "{at}: `env_var('{var}')`: environment variable `{var}` is not set and no default is given; `dre run` will need it"
                    ),
                );
            }
        }
    }
    match crate::profiles::render_connection(&known, ctx.env()) {
        Ok(r) => Some((r, unresolved)),
        Err(e) => {
            diags.error(
                Code::InvalidConnectionSetting,
                file.clone(),
                line,
                format!("{at}: {e}"),
            );
            None
        }
    }
}

/// Ask plugin `p` to check one entry's settings and report what it says. False when the plugin
/// couldn't check (it's too old): its other entries aren't asked.
fn report_connection_check(
    p: &mut dyn crate::plugin_host::Plugin,
    (kind, name): (PluginKind, &str),
    rendered: Map<String, Value>,
    unresolved: Vec<String>,
    at: &str,
    (file, line): (&Option<PathBuf>, Option<usize>),
    diags: &mut Diagnostics,
) -> bool {
    match p.validate_connection(rendered, unresolved) {
        Ok((errors, warnings)) => {
            for e in errors {
                diags.error(
                    Code::InvalidConnectionSetting,
                    file.clone(),
                    line,
                    format!("{at}: {e}"),
                );
            }
            for w in warnings {
                diags.warning(
                    Code::UnknownConnectionKey,
                    file.clone(),
                    line,
                    format!("{at}: {w}"),
                );
            }
            true
        }
        Err(e) => {
            diags.warning(
                Code::OptionsUnchecked,
                file.clone(),
                line,
                format!(
                    "{at}: {kind} `{name}` {} couldn't check its settings ({e}); run `dre plugin update {kind}/{name}`",
                    p.info().version
                ),
            );
            false
        }
    }
}

/// The variables of `env_var()` calls without a default that aren't set, anywhere in `v`.
fn unset_env_vars(v: &Value, env: &crate::Env) -> Vec<String> {
    match v {
        Value::String(s) if crate::preflight::is_templated(s) => crate::preflight::calls(s)
            .into_iter()
            .filter(|c| c.func == "env_var" && !c.has_default && env.var(&c.name).is_none())
            .map(|c| c.name)
            .collect(),
        Value::Array(a) => a.iter().flat_map(|x| unset_env_vars(x, env)).collect(),
        Value::Object(o) => o.values().flat_map(|x| unset_env_vars(x, env)).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(profile: Option<&str>) -> Use {
        Use {
            ctx: "report `pack`".into(),
            file: PathBuf::from("reports/pack/pack.yml"),
            profile: profile.map(str::to_string),
        }
    }

    fn messages(diags: &Diagnostics) -> Vec<String> {
        diags.sorted().into_iter().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn equal_blocks_are_checked_once_and_reported_where_each_is_used() {
        let mut blocks = ByPlugin::new();
        let options: Map<String, Value> = serde_json::from_str(r#"{"delimiter": ";"}"#).unwrap();
        add(&mut blocks, PluginKind::Format, "csv", &options, None, at(None));
        add(&mut blocks, PluginKind::Format, "csv", &options, None, at(None));
        add(
            &mut blocks,
            PluginKind::Format,
            "csv",
            &Map::new(),
            None,
            at(None),
        );
        let csv = &blocks[&(PluginKind::Format, "csv".to_string())];
        assert_eq!(csv.len(), 2);
        assert!(csv.values().any(|(_, _, uses)| uses.len() == 2));
    }

    #[test]
    fn the_local_destination_takes_no_attachments() {
        let routes = vec![Route {
            at: at(Some("inbox")),
            format: "message".into(),
            attach: true,
        }];
        let mut diags = Diagnostics::default();
        check_local(Some(&routes), &Blocks::new(), &mut diags);
        assert_eq!(
            messages(&diags),
            [
                "report `pack`: destination `inbox`: `attach:` needs a destination that takes messages and files, but `local` takes files only"
            ]
        );
    }

    #[test]
    fn a_destination_option_problem_names_its_profile() {
        let mut diags = Diagnostics::default();
        report(&mut diags, &at(Some("outbox")), "unknown key `folder`");
        report(&mut diags, &at(None), "unknown key `sheet`");
        let codes: Vec<Code> = diags.sorted().into_iter().map(|d| d.code).collect();
        assert!(
            codes.contains(&Code::InvalidDestinationOption) && codes.contains(&Code::InvalidOutputOption)
        );
        assert!(
            messages(&diags)
                .iter()
                .any(|m| m == "report `pack`: destination `outbox`: unknown key `folder`")
        );
    }
}
