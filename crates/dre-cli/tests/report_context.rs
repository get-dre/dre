//! Generic report context is available to external tools through the CLI manifest.
mod common;

use common::TestProject;
use serde_json::json;

#[test]
fn report_description_and_metadata_layers_keep_their_origins() {
    let p = TestProject::new(
        &[
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            (
                "dre_project.yml",
                "name: context\ndefault_profile: warehouse\nmeta: {owner: data}\nreports:\n  +meta: {audience: internal}\n  finance:\n    +meta: {ai: {expose: false}, owner: finance}\n",
            ),
            (
                "reports/finance/revenue/revenue.yml",
                "description: Monthly revenue net of GST\nmeta: {ai: {questions: [How much revenue?]}, units: AUD}\nqueries: [revenue]\n",
            ),
            ("reports/finance/revenue/revenue.sql", "select 42 as revenue\n"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n",
    );
    p.dre("validate", &[]).ok();
    let m = p.json("target/manifest.json");
    let r = &m["reports"]["revenue"];
    assert_eq!(r["description"], "Monthly revenue net of GST");
    assert_eq!(
        r["meta"],
        json!({"ai": {"questions": ["How much revenue?"]}, "units": "AUD"})
    );
    assert_eq!(
        r["meta_layers"],
        json!([
            {"file": "dre_project.yml", "key": "meta", "meta": {"owner": "data"}},
            {"file": "dre_project.yml", "key": "reports.+meta", "meta": {"audience": "internal"}},
            {"file": "dre_project.yml", "key": "reports.finance.+meta", "meta": {"ai": {"expose": false}, "owner": "finance"}},
            {"file": "reports/finance/revenue/revenue.yml", "key": "meta", "meta": {"ai": {"questions": ["How much revenue?"]}, "units": "AUD"}}
        ])
    );
}

#[test]
fn no_delivery_run_exports_typed_rows_and_keeps_scheduled_history_separate() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: rows\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            (
                "reports/revenue/revenue.yml",
                "queries: [amounts]\noutput:\n  format: csv\n  destination: {profile: local, path: delivered.csv}\n",
            ),
            (
                "reports/revenue/amounts.sql",
                "select * from (values (1, 12.50::decimal(12,2), true), (2, NULL, false)) t(id, amount, active)\n",
            ),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: ':memory:'}\n",
    );
    p.dre(
        "run",
        &["-s", "revenue", "--no-deliver", "--output-json", "answers"],
    )
    .ok();
    assert!(!p.path("delivered.csv").exists());
    assert!(!p.path("target/run/revenue").exists());
    let result = p.json("answers/revenue/default/amounts.json");
    assert_eq!(result["schema_version"], "dre/rows/v1");
    assert_eq!(
        result["columns"],
        json!([
            {"name": "id", "type": "Int32"},
            {"name": "amount", "type": "Decimal128(12, 2)"},
            {"name": "active", "type": "Boolean"}
        ])
    );
    // Decimal values remain exact strings, with their type supplied separately.
    assert_eq!(result["rows"], json!([[1, "12.50", true], [2, null, false]]));
    assert_eq!(result["row_count"], 2);
    assert!(p.path("answers/run/revenue/default/current").exists());
}

#[test]
fn declared_variables_supply_defaults_and_reject_invalid_overrides() {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: parameters\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            (
                "reports/revenue/revenue.yml",
                "queries: [period]\nvars: {legacy_map: {type: customer}}\nvar_schema:\n  months: {type: integer, default: 3, allowed: [1, 3, 12], description: Reporting window}\n  as_of: {type: date, default: '2026-01-25'}\n",
            ),
            (
                "reports/revenue/period.sql",
                "select {{ var('months') }} as months, '{{ var('as_of') }}' as as_of\n",
            ),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: ':memory:'}\n",
    );
    p.dre("compile", &[]).ok();
    let m = p.json("target/manifest.json");
    assert_eq!(m["reports"]["revenue"]["bindings"][0]["vars"]["months"], 3);
    assert_eq!(
        m["reports"]["revenue"]["bindings"][0]["vars"]["legacy_map"],
        json!({"type": "customer"})
    );
    assert_eq!(m["reports"]["revenue"]["var_schema"]["months"]["type"], "integer");
    let invalid = p.dre("compile", &["--var", "months=2"]);
    assert_ne!(invalid.code, 0);
    assert!(
        invalid.stdout.contains("months") && invalid.stdout.contains("allowed"),
        "{invalid:?}"
    );
    let invalid = p.dre("compile", &["--var", "as_of=2026-02-30"]);
    assert_ne!(invalid.code, 0);
    assert!(
        invalid.stdout.contains("as_of") && invalid.stdout.contains("date"),
        "{invalid:?}"
    );
}

#[test]
fn read_only_query_uses_a_dre_connection_and_rejects_writes() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: query\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb]\n"),
            ("query.sql", "select 7::integer as n\n"),
            ("write.sql", "create table forbidden(n integer); select 1\n"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n",
    );
    p.duckdb("data.duckdb", "select 1");
    p.dre(
        "query",
        &[
            "--profile",
            "warehouse",
            "--sql-file",
            "query.sql",
            "--output-json",
            "result.json",
        ],
    )
    .ok();
    assert_eq!(p.json("result.json")["rows"], json!([[7]]));
    p.dre(
        "query",
        &[
            "--profile",
            "warehouse",
            "--sql-file",
            "write.sql",
            "--output-json",
            "bad.json",
        ],
    )
    .failed()
    .says("single read-only query");
    assert!(!p.path("bad.json").exists());
}

#[test]
fn catalogue_resolves_names_and_reads_upstream_table_and_column_comments() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: catalogue\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb]\n"),
            (
                "tables.json",
                "{\"tables\":[\"orders\",\"main.customers\"],\"schemas\":[]}",
            ),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n",
    );
    p.duckdb("data.duckdb", "create table orders(customer_id integer, net_amount decimal(12,2)); create table customers(customer_id integer, name varchar); comment on table orders is 'One row per order; revenue net of GST'; comment on column orders.customer_id is 'Join to customers.customer_id'; comment on column orders.net_amount is 'AUD excluding GST';");
    p.dre(
        "catalog",
        &[
            "--profile",
            "warehouse",
            "--tables-file",
            "tables.json",
            "--output-json",
            "catalog.json",
        ],
    )
    .ok();
    let c = p.json("catalog.json");
    assert_eq!(c["schema_version"], "dre/catalog/v1");
    let orders = c["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "orders")
        .unwrap();
    assert_eq!(orders["catalog"], "data");
    assert_eq!(orders["schema"], "main");
    assert_eq!(orders["description"], "One row per order; revenue net of GST");
    assert_eq!(
        orders["columns"][0]["description"],
        "Join to customers.customer_id"
    );
    assert_eq!(orders["columns"][1]["description"], "AUD excluding GST");
    p.write("tables.json", "{\"tables\":[\"MAIN.ORDERS\"]}");
    p.dre(
        "catalog",
        &[
            "--profile",
            "warehouse",
            "--tables-file",
            "tables.json",
            "--output-json",
            "uppercase.json",
        ],
    )
    .ok();
    assert_eq!(p.json("uppercase.json")["tables"][0]["name"], "orders");
}

#[cfg(unix)]
#[test]
fn extension_handoff_forwards_arguments_without_treating_it_as_a_plugin() {
    use std::os::unix::fs::PermissionsExt;
    let p = TestProject::new(&[("dre_project.yml", "name: extensions\n")], "");
    let binary = p.root().join("companion");
    std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 7\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = p.dre_env(
        "mcp",
        &["serve", "--no-report-tables"],
        &[("DRE_MCP_BIN", binary.to_str().unwrap())],
    );
    assert_eq!(result.code, 7);
    assert!(
        result.stdout.contains("serve\n--no-report-tables\n"),
        "{result:?}"
    );
    assert!(result.stdout.contains("--project-dir"), "{result:?}");
    assert!(result.stdout.contains("--profiles-dir"), "{result:?}");
}

#[cfg(unix)]
#[test]
fn extension_install_requires_license_acceptance_and_checks_artifact_hash() {
    use sha2::{Digest, Sha256};
    let p = TestProject::new(&[("dre_project.yml", "name: extensions\n")], "");
    p.write("companion", "#!/bin/sh\necho companion-ready\n");
    let digest: String = Sha256::digest(p.read("companion").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let index = json!({"schema": 2, "plugins": [], "extensions": [{"name": "dre-mcp", "kind": "extension", "license": "Proprietary", "proprietary_license": true,
        "versions": [{"version": "0.1.0-rc.1", "artifacts": {dre_core::manager::platform(): {"url": p.path("companion"), "sha256": digest}}}]}]});
    p.write("index.json", &index.to_string());
    let registry = p.path("index.json");
    let env = [("DRE_REGISTRY_URL", registry.to_str().unwrap())];
    p.dre_env("extension", &["install", "dre-mcp"], &env)
        .failed()
        .says("--accept-proprietary");
    p.dre_env("extension", &["install", "dre-mcp", "--accept-proprietary"], &env)
        .ok();
    p.dre("mcp", &[]).ok().says("companion-ready");
    p.write("companion", "changed after registry was signed\n");
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("checksum mismatch");
    // A rejected update leaves the previously installed binary usable.
    p.dre("mcp", &[]).ok().says("companion-ready");
}

#[cfg(unix)]
#[test]
fn declared_extensions_install_pinned_versions_without_rewriting_owner_acceptance() {
    use sha2::{Digest, Sha256};
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: companions\n"),
            (
                "dependencies.yml",
                "extensions:\n  - name: dre-mcp\n    version: '0.1.0-rc.1'\n    accept_license: '2026-10'\n",
            ),
        ],
        "",
    );
    p.write("companion", "#!/bin/sh\nprintf 'declared-ready\\n'\n");
    let digest: String = Sha256::digest(p.read("companion").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    p.write("index.json", &json!({"schema":2,"plugins":[],"extensions":[{"name":"dre-mcp","kind":"extension","license":"Proprietary","proprietary_license":true,
        "versions":[{"version":"0.1.0-rc.1","artifacts":{dre_core::manager::platform():{"url":p.path("companion"),"sha256":digest}}}]}]}).to_string());
    // A different executable on PATH must not replace the owner's exact managed pin.
    p.write("wrong/dre-mcp", "#!/bin/sh\nprintf 'wrong-path-version\\n'\n");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p.path("wrong/dre-mcp"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let search = format!(
        "{}:{}",
        p.path("wrong").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    p.dre_env(
        "mcp",
        &["serve"],
        &[
            ("DRE_REGISTRY_URL", p.path("index.json").to_str().unwrap()),
            ("PATH", &search),
        ],
    )
    .ok()
    .says("declared-ready");
    let before = p.read("dependencies.yml");
    p.dre_env(
        "deps",
        &[],
        &[("DRE_REGISTRY_URL", p.path("index.json").to_str().unwrap())],
    )
    .ok();
    assert_eq!(p.read("dependencies.yml"), before);
    p.dre("mcp", &["serve"]).ok().says("declared-ready");
}
