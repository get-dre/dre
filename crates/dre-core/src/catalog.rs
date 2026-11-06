//! Live relation context for integrations. Descriptions are read from the database;
//! DRE does not create or maintain upstream comments.

use crate::{
    plugin_host::Open,
    profiles::{Entry, Role},
    project::Project,
};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<String>,
}

/// Read live catalogue metadata from the configured source plugin.
pub fn build(
    ctx: &crate::Context,
    project: &Project,
    profile: &str,
    request: &Request,
) -> Result<Value, String> {
    let Entry::Use(target) = project.profiles().entry(Role::Connection, profile) else {
        return Err(format!("connection `{profile}` is unavailable"));
    };
    plugin_catalog(ctx, project, profile, target, request).map_err(|(found, error)| {
        if found {
            error
        } else {
            format!(
                "live catalogue comments are not yet supported for `{}`; the plugin must advertise `catalog`",
                target.kind
            )
        }
    })
}

/// Discovery for a source that advertises native `catalog` metadata. The plugin owns vendor
/// metadata calls; core validates the returned contract, including scoped requested objects.
fn plugin_catalog(
    ctx: &crate::Context,
    project: &Project,
    profile: &str,
    target: &crate::profiles::ProfileTarget,
    request: &Request,
) -> Result<Value, (bool, String)> {
    let plugin =
        crate::plugin_host::find_plugin(ctx, project, crate::project::PluginKind::Source, &target.kind)
            .map_err(|e| (true, e.to_string()))?;
    let connection = crate::profiles::render_connection(target, ctx.env()).map_err(|e| (true, e))?;
    let secrets = ctx.secrets().clone();
    let log: dre_plugin_host::host::LogSink = std::sync::Arc::new(move |name, message| {
        eprintln!("{name}: {}", secrets.mask_source_error(message, false));
    });
    let mut process = ctx
        .host()
        .start(&plugin, log, Some(project.root()))
        .map_err(|e| (true, e.to_string()))?;
    if !process.has(dre_protocol::CAP_CATALOG) {
        process.close();
        return Err((false, String::new()));
    }
    let masked = |e: dre_plugin_host::host::HostError| {
        ctx.secrets()
            .mask_source_error(&e.to_string(), false)
            .into_owned()
    };
    let open = if process.has(dre_protocol::CAP_READ_ONLY) {
        Open::ReadOnly
    } else {
        Open::ReadWrite
    };
    let result = (|| {
        process.open(connection, open).map_err(|e| (true, masked(e)))?;
        let mut value = process
            .catalog(request.tables.clone(), request.schemas.clone())
            .map_err(|e| (true, masked(e)))?;
        validate_plugin_catalog(&value, request).map_err(|e| (true, e))?;
        let object = value.as_object_mut().expect("validated above");
        object.insert("connection".into(), json!(profile));
        object.insert("target".into(), json!(project.target_name()));
        object.insert("source_type".into(), json!(target.kind));
        Ok(value)
    })();
    process.close();
    result
}

fn validate_plugin_catalog(value: &Value, request: &Request) -> Result<(), String> {
    value
        .as_object()
        .ok_or("plugin catalog response must be a JSON object")?;
    if request.tables.is_empty() && request.schemas.is_empty() {
        return Err("catalog request needs at least one table or schema".into());
    }
    if value["schema_version"] != "dre/catalog/v1" {
        return Err("plugin catalog response must use schema version `dre/catalog/v1`".into());
    }
    let quote = value["identifier_quote"]
        .as_str()
        .filter(|s| s.chars().count() == 1)
        .ok_or("plugin catalog response has no single-character identifier_quote")?
        .to_string();
    let dialect = &value["dialect"];
    dialect
        .as_object()
        .ok_or("plugin catalog response has no dialect")?;
    if !matches!(
        dialect["identifier_case"].as_str(),
        Some("insensitive" | "lower_unless_quoted" | "upper_unless_quoted")
    ) || !matches!(
        dialect["literal_style"].as_str(),
        Some("doubling" | "backslash" | "hex")
    ) || dialect["identifier_quote"].as_str() != Some(&quote)
        || (dialect["literal_style"] == "hex"
            && dialect["hex_literal"]
                .as_str()
                .is_none_or(|s| s.matches("{hex}").count() != 1))
    {
        return Err("plugin catalog response has unsupported dialect rules".into());
    }
    let tables = value["tables"]
        .as_array()
        .ok_or("plugin catalog response has no tables array")?;
    for table in tables {
        table
            .as_object()
            .ok_or("plugin catalog response has a non-object table")?;
        if table["name"].as_str().is_none()
            || table["schema"].as_str().is_none()
            || table["catalog"].as_str().is_none()
            || table["columns"].as_array().is_none()
        {
            return Err("plugin catalog response has a table without catalog/schema/name/columns".into());
        }
        for column in table["columns"].as_array().unwrap() {
            if column["name"].as_str().is_none() || column["type"].as_str().is_none() {
                return Err("plugin catalog response has a column without name and type".into());
            }
        }
    }
    for wanted in &request.tables {
        let found = tables.iter().any(|t| {
            t["requested"]
                .as_array()
                .is_some_and(|requested| requested.iter().any(|name| name.as_str() == Some(wanted)))
        });
        if !found {
            return Err(format!(
                "table `{wanted}` is missing or not visible through plugin catalog metadata"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_prefers_read_only_sessions_and_closes_failed_requests() {
        use crate::plugin_host::memory::{Call, MemoryHost, Script};
        use std::sync::Arc;

        for (capabilities, expected) in [
            (
                vec![dre_protocol::CAP_CATALOG, dre_protocol::CAP_READ_ONLY],
                "ReadOnly",
            ),
            (vec![dre_protocol::CAP_CATALOG], "ReadWrite"),
        ] {
            for failure in [Call::Open, Call::Catalog] {
                let script = Script::new(&capabilities).fail(failure, 1, "fixture failure");
                let host = MemoryHost::new().plugin(dre_protocol::Kind::Source, "fixture", script);
                let ctx = crate::Context::new(crate::Env::default(), Arc::new(host.clone()));
                let dir = tempfile::tempdir().unwrap();
                std::fs::write(
                    dir.path().join("dre_project.yml"),
                    "name: catalogue\nallow_unmanaged: true\n",
                )
                .unwrap();
                let (project, diags) = crate::project::load_with(dir.path(), &Default::default(), &ctx);
                assert!(!diags.has_errors(), "{:?}", diags.sorted());
                let target = crate::profiles::ProfileTarget {
                    kind: "fixture".into(),
                    fields: serde_json::Map::new(),
                };
                let error = plugin_catalog(
                    &ctx,
                    &project.unwrap(),
                    "warehouse",
                    &target,
                    &Request {
                        tables: vec!["orders".into()],
                        schemas: vec![],
                    },
                )
                .unwrap_err();
                assert!(error.1.contains("fixture failure"), "{error:?}");
                let calls = host.calls();
                assert!(
                    calls.contains(&format!("source/fixture Open {expected}")),
                    "{calls:?}"
                );
                assert_eq!(calls.last().unwrap(), "source/fixture Close");
            }
        }
    }

    #[test]
    fn incomplete_plugin_catalogs_return_errors_without_panicking() {
        let request = Request {
            tables: vec!["orders".into()],
            schemas: vec![],
        };
        for value in [
            json!({}),
            json!({"schema_version":"dre/catalog/v1"}),
            json!({"schema_version":"dre/catalog/v1", "identifier_quote":"\"", "dialect":{}}),
        ] {
            assert!(validate_plugin_catalog(&value, &request).is_err());
        }
    }
}
