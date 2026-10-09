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

/// The JSON Schema of a config file type, as committed in `docs/schemas/<name>.schema.json`.
///
/// `version` is DRE's minor version (`x-dre-schema-version`): the schemas are published under
/// `/schemas/v<minor>/`.
pub fn schema<T: schemars::JsonSchema>(version: &str) -> serde_json::Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12();
    let mut schema = settings.into_generator().into_root_schema_for::<T>().to_value();
    // An optional key is one that may be left out, not one that may be `null`.
    without_null(&mut schema);
    let serde_json::Value::Object(mut generated) = schema else {
        unreachable!("a schema is an object")
    };
    generated.insert(
        "$schema".into(),
        "https://json-schema.org/draft/2020-12/schema".into(),
    );
    generated.insert("x-dre-schema-version".into(), version.into());
    let mut out = serde_json::Value::Object(generated);
    in_order(&mut out);
    out
}

/// Every schema object's keys in one order: what it is, then its shape, then the rest.
fn in_order(v: &mut serde_json::Value) {
    const ORDER: [&str; 13] = [
        "$schema",
        "x-dre-schema-version",
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
    ]
}
