//! Reading DRE's YAML files into typed config.
//!
//! [`node`] parses a file into a tree that knows each value's line; [`de`] reads typed structs from
//! that tree with serde. The structs are the single description of each file: they also generate
//! its JSON Schema in `docs/schemas/` (with `schemars`).

pub mod de;
pub mod dependencies;
pub mod lookup;
pub mod node;
pub mod project;
pub mod report;
pub mod schedule;
pub mod sources;

/// The JSON Schema of a config file type, as committed in `docs/schemas/<name>.schema.json`.
///
/// `version` is DRE's minor version (`x-dre-schema-version`): the schemas are published under
/// `/schemas/v<minor>/`.
pub fn schema<T: schemars::JsonSchema>(version: &str) -> serde_json::Value {
    let mut out = generated::<T>(schemars::generate::Contract::Deserialize);
    // An optional key is one that may be left out, not one that may be `null`.
    without_null(&mut out);
    if let serde_json::Value::Object(m) = &mut out {
        m.insert("x-dre-schema-version".into(), version.into());
        // Top-level `x-*` keys hold YAML anchors to reuse; DRE ignores them.
        if m.get("type").and_then(|t| t.as_str()) == Some("object") {
            m.insert(
                "patternProperties".into(),
                serde_json::json!({"^x-": {"description": "Anything, for YAML anchors (`&name`) reused with aliases (`*name`) and merges (`<<: *name`). DRE ignores these keys."}}),
            );
        }
    }
    in_order(&mut out);
    out
}

/// The JSON Schema of a file DRE writes (`run_results.json`, the manifest), published at `id`
/// (`docs/<file>.schema.json`). Its version is in the format's own `schema_version`.
pub fn artifact_schema<T: schemars::JsonSchema>(id: &str) -> serde_json::Value {
    // What DRE writes: a key that's always written is required, and `null` is a value.
    let mut out = generated::<T>(schemars::generate::Contract::Serialize);
    if let serde_json::Value::Object(m) = &mut out {
        m.insert("$id".into(), id.into());
    }
    in_order(&mut out);
    out
}

fn generated<T: schemars::JsonSchema>(contract: schemars::generate::Contract) -> serde_json::Value {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.contract = contract;
    let mut schema = settings.into_generator().into_root_schema_for::<T>().to_value();
    if let serde_json::Value::Object(m) = &mut schema {
        m.insert(
            "$schema".into(),
            "https://json-schema.org/draft/2020-12/schema".into(),
        );
    }
    schema
}

/// Every schema object's keys in one order: what it is, then its shape, then the rest.
fn in_order(v: &mut serde_json::Value) {
    const ORDER: [&str; 14] = [
        "$schema",
        "x-dre-schema-version",
        "$id",
        "title",
        "type",
        "description",
        "deprecated",
        "const",
        "x-doc-type",
        "properties",
        "required",
        "additionalProperties",
        "minProperties",
        "$defs",
    ];
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            m.values_mut().for_each(in_order);
            // `properties` and `$defs` are names, in the structs' order; the rest are keywords.
            if m.contains_key("$schema")
                || m.contains_key("type")
                || m.contains_key("$ref")
                || m.contains_key("oneOf")
                || m.contains_key("const")
            {
                let rank = |k: &str| ORDER.iter().position(|o| *o == k).unwrap_or(ORDER.len() - 1);
                let mut entries: Vec<(String, Value)> = std::mem::take(m).into_iter().collect();
                entries.sort_by_key(|(k, _)| rank(k));
                m.extend(entries);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(in_order),
        _ => {}
    }
}

/// Drop the `null` schemars allows for every `Option`: `type: [T, "null"]` and
/// `anyOf: [T, {type: null}]`.
fn without_null(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            // A schema that allows `null` on purpose says so.
            let keep_null = m.remove("x-dre-null").is_some();
            if m.get("default") == Some(&Value::Null) {
                m.remove("default");
            }
            if let Some(Value::Array(types)) = m.get_mut("type")
                && !keep_null
            {
                types.retain(|t| t != "null");
                if types.len() == 1 {
                    let only = types.remove(0);
                    m.insert("type".into(), only);
                }
            }
            if let Some(Value::Array(values)) = m.get_mut("enum") {
                values.retain(|v| !v.is_null());
            }
            // Rust's integer widths aren't part of what a YAML file may hold.
            if m.get("format")
                .and_then(Value::as_str)
                .is_some_and(|f| f.starts_with("int") || f.starts_with("uint"))
            {
                m.remove("format");
            }
            if let Some(Value::Array(any)) = m.get("anyOf") {
                let rest: Vec<Value> = any
                    .iter()
                    .filter(|s| s.get("type") != Some(&Value::from("null")))
                    .cloned()
                    .collect();
                if rest.len() == 1 && rest.len() < any.len() {
                    m.remove("anyOf");
                    if let Value::Object(inner) = rest.into_iter().next().unwrap() {
                        for (k, v) in inner {
                            m.entry(k).or_insert(v);
                        }
                    }
                }
            }
            m.values_mut().for_each(without_null);
        }
        Value::Array(a) => a.iter_mut().for_each(without_null),
        _ => {}
    }
}

/// The schemas of the files DRE writes, by path under `docs/`.
pub fn artifact_schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "run-results.schema.json",
            artifact_schema::<crate::run_results::RunResults>(
                "https://github.com/get-dre/dre/blob/master/docs/run-results.schema.json",
            ),
        ),
        (
            "manifest.schema.json",
            artifact_schema::<crate::manifest::Manifest>(
                "https://github.com/get-dre/dre/blob/master/docs/manifest.schema.json",
            ),
        ),
    ]
}

/// Every generated schema, by file name (`profiles` for `profiles.schema.json`).
pub fn schemas(version: &str) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("project", schema::<project::ProjectFile>(version)),
        ("report", schema::<report::ReportFile>(version)),
        ("sets", schema::<schedule::SetsFile>(version)),
        ("schedules", schema::<schedule::SchedulesFile>(version)),
        ("timings", schema::<schedule::TimingsFile>(version)),
        ("profiles", schema::<crate::profiles::ProfilesFile>(version)),
        ("dependencies", schema::<dependencies::DependenciesFile>(version)),
        ("lookup", schema::<lookup::LookupFile>(version)),
        ("sources", schema::<sources::SourcesFile>(version)),
    ]
}

/// YAML for a file DRE writes itself (`dre.lock`, a profile `dre init` adds): block style,
/// every string on one line.
pub fn to_yaml<T: serde::Serialize>(value: &T) -> Result<String, String> {
    let mut options = serde_saphyr::SerializerOptions::default();
    options.prefer_block_scalars = false;
    serde_saphyr::to_string_with_options(value, options).map_err(|e| e.to_string())
}
