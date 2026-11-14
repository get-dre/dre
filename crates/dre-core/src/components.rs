//! Component lists: a destination option whose value is a list holding `component: <name>.json`
//! entries. Core resolves each component, renders it after the queries ran (the context a
//! message's `text:` gets, plus `args` from the entry's `with:`), parses the JSON, and sends the
//! plugin `{"component": <file>, "blocks": [...], ...other keys}` per entry. An entry without
//! `component:` is inline: its strings are rendered and it arrives as `{"blocks": [<entry>]}`.
//! Core knows nothing of what the blocks mean; the plugin does.
//!
//! Lookup is the template rule ([`crate::template_file`]), then installed packages: a
//! `<package>/<file>.json` name, or a bare name not in the project and unique across packages.

use std::path::{Component, Path, PathBuf};

use serde_json::{Map as JsonMap, Value as Json};

use crate::packages::Package;
use crate::render::Renderer;

const COMPONENT: &str = "component";
const WITH: &str = "with";

/// Whether a destination option's value is a component list: a list with a `component:` entry.
pub fn is_list(v: &Json) -> bool {
    v.as_array()
        .is_some_and(|a| a.iter().any(|e| e.get(COMPONENT).is_some()))
}

/// Check every `component:` name in option value `v` (from a report whose YAML is `yaml`)
/// without rendering: each problem is a sentence.
pub fn check(root: &Path, packages: &[Package], yaml: &Path, v: &Json) -> Vec<String> {
    let Some(list) = v.as_array().filter(|_| is_list(v)) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|e| match e.get(COMPONENT)? {
            Json::String(name) => resolve(root, packages, yaml, name).err(),
            _ => Some("`component` must be a file name ending in `.json`".into()),
        })
        .collect()
}

/// Where component `name` is read from, and the name it's shown (and sent) by: its path from
/// the project root, or `<package>/<path in the package>`.
pub fn resolve(
    root: &Path,
    packages: &[Package],
    yaml: &Path,
    name: &str,
) -> Result<(PathBuf, String), String> {
    if !name.ends_with(".json") {
        return Err(format!("component `{name}` must name a `.json` file"));
    }
    let yaml = root.join(yaml);
    let from = yaml.parent().unwrap_or(root);
    let err = match crate::template_file::resolve(root, from, name) {
        Ok(p) => {
            let shown = crate::slash_relative(root, &p).to_string_lossy().into_owned();
            return Ok((p, shown));
        }
        Err(e) => e.replace("template file", "component"),
    };
    if let Some((pkg, rest)) = name.split_once('/') {
        let Some(p) = packages.iter().find(|p| p.name == pkg) else {
            return Err(err);
        };
        let path = p.dir.join(rest);
        let inside = Path::new(rest)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        return if inside && path.is_file() {
            Ok((path, name.to_string()))
        } else {
            Err(format!("component `{name}` doesn't exist in package `{pkg}`"))
        };
    }
    // Only a name missing from the project falls back to packages; an ambiguous one doesn't.
    if !err.contains("doesn't exist") {
        return Err(err);
    }
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for p in packages {
        for e in walkdir::WalkDir::new(&p.dir)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() && e.file_name() == name)
        {
            let inner = crate::slash_relative(&p.dir, e.path());
            found.push((e.into_path(), format!("{}/{}", p.name, inner.display())));
        }
    }
    match found.len() {
        0 => Err(format!("{err}, or in the installed packages")),
        1 => Ok(found.remove(0)),
        n => Err(format!(
            "component `{name}` isn't in the project and matches {n} files in packages ({}); name one as `<package>/<path>`",
            found
                .iter()
                .map(|(_, s)| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Render component list `v` with `ctx` (the message context: `results`, `each`, ...), for a
/// report whose YAML is `yaml`.
pub fn render(
    renderer: &Renderer,
    root: &Path,
    packages: &[Package],
    yaml: &Path,
    v: &Json,
    ctx: &minijinja::Value,
) -> Result<Json, String> {
    let list = v.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let Some(name) = entry.get(COMPONENT) else {
            let block = strings(renderer, yaml, entry, ctx)?;
            out.push(serde_json::json!({ "blocks": [block] }));
            continue;
        };
        let Json::String(name) = name else {
            return Err("`component` must be a file name ending in `.json`".into());
        };
        let (path, shown) = resolve(root, packages, yaml, name)?;
        let args = match entry.get(WITH) {
            None => Json::Object(JsonMap::new()),
            Some(w @ Json::Object(_)) => strings(renderer, yaml, w, ctx)?,
            Some(_) => return Err(format!("component `{shown}`: `with` must be a map")),
        };
        let src = std::fs::read_to_string(&path).map_err(|e| format!("component `{shown}`: {e}"))?;
        let ctx = minijinja::context! { args => minijinja::Value::from(minijinja::value::Serde(&args)), ..ctx.clone() };
        let text = renderer
            .render_with(Path::new(&shown), &src, ctx, false)
            .map_err(|e| e.to_string())?;
        let blocks = match serde_json::from_str::<Json>(&text) {
            Ok(Json::Array(a)) => a,
            Ok(o @ Json::Object(_)) => vec![o],
            Ok(_) => {
                return Err(format!(
                    "component `{shown}` must render to a JSON object or a list of them"
                ));
            }
            Err(e) => {
                let near = text
                    .lines()
                    .nth(e.line().saturating_sub(1))
                    .unwrap_or_default()
                    .trim();
                return Err(format!(
                    "component `{shown}` didn't render to valid JSON: {e}; near `{near}`"
                ));
            }
        };
        let mut sent = JsonMap::new();
        sent.insert(COMPONENT.into(), Json::String(shown));
        sent.insert("blocks".into(), Json::Array(blocks));
        if let Json::Object(o) = entry {
            for (k, v) in o.iter().filter(|(k, _)| *k != COMPONENT && *k != WITH) {
                sent.insert(k.clone(), v.clone());
            }
        }
        out.push(Json::Object(sent));
    }
    Ok(Json::Array(out))
}

/// Render every string inside `v` with `ctx`.
fn strings(renderer: &Renderer, yaml: &Path, v: &Json, ctx: &minijinja::Value) -> Result<Json, String> {
    Ok(match v {
        Json::String(s) => Json::String(
            renderer
                .render_with(yaml, s, ctx.clone(), false)
                .map_err(|e| e.to_string())?,
        ),
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| strings(renderer, yaml, v, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), strings(renderer, yaml, v, ctx)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}
