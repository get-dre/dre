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
fn string_variables_can_require_a_pattern_and_a_max_length() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: patterns\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            (
                "reports/customer/customer.yml",
                "queries: [q]\nvar_schema:\n  code: {type: string, default: ABC1234, pattern: '[A-Z]{3}[0-9]{4}', max_length: 7}\n",
            ),
            ("reports/customer/q.sql", "select '{{ var('code') }}' as code\n"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: ':memory:'}\n",
    );
    p.dre("validate", &[]).ok();
    p.dre("run", &["--var", "code=XYZ9876"]).ok();
    p.dre("run", &["--var", "code=ABC1234' or 1=1 --"])
        .failed()
        .says("variable `code` is longer than its max_length of 7");
    p.dre("validate", &["--var", "code=abc1234"])
        .failed()
        .says("variable `code` does not match its pattern");
    // A broken declaration is reported by validate, before any run.
    p.write(
        "reports/customer/customer.yml",
        "queries: [q]\nvar_schema:\n  code: {type: integer, default: 1, pattern: '('}\n",
    );
    let r = p.dre("validate", &[]);
    r.failed().says("`pattern` applies only to type string");
    r.says("invalid `pattern`");
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
            "--output-file",
            "result.json",
        ],
    )
    .ok();
    assert_eq!(p.json("result.json")["rows"], json!([[7]]));
    // The 0.4 flag name still works, with a warning.
    p.dre(
        "query",
        &[
            "--profile",
            "warehouse",
            "--sql-file",
            "query.sql",
            "--output-json",
            "old.json",
        ],
    )
    .ok()
    .says("--output-json here is now --output-file");
    assert_eq!(p.json("old.json")["rows"], json!([[7]]));
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
fn read_only_query_refuses_file_program_and_network_functions() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: query\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb]\n"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n",
    );
    p.duckdb("data.duckdb", "select 1");
    p.write("secret.txt", "top secret\n");
    for (name, sql) in [
        ("text", "select content from read_text('secret.txt')"),
        (
            "nested",
            "select upper((select content from read_text('secret.txt')))",
        ),
        ("csv", "select * from READ_CSV_AUTO('secret.txt')"),
        ("path", "select * from 'secret.txt'"),
        ("env", "select getenv('HOME')"),
    ] {
        p.write(&format!("{name}.sql"), sql);
        let r = p.dre(
            "query",
            &[
                "--profile",
                "warehouse",
                "--sql-file",
                &format!("{name}.sql"),
                "--output-file",
                "out.json",
            ],
        );
        r.failed();
        assert!(
            r.stdout.contains("nothing was run") || r.stderr.contains("nothing was run"),
            "{name}: {r:?}"
        );
        assert!(!p.path("out.json").exists(), "{name}");
    }
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
    assert_eq!(
        c["dialect"],
        json!({"identifier_quote": "\"", "identifier_case": "insensitive", "literal_style": "doubling", "hex_literal": null})
    );
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
    // The companion runs this `dre`, never another one from the PATH.
    let dre = common::bin_dir().join(format!("dre{}", std::env::consts::EXE_SUFFIX));
    assert!(
        result
            .stdout
            .starts_with(&format!("--dre-bin\n{}\n", dre.display())),
        "{result:?}"
    );
}

/// A signing key for test registries, trusted by debug builds through `DRE_TEST_EXTENSION_KEY`.
struct Signer(minisign::KeyPair);

impl Signer {
    fn new() -> Signer {
        Signer(minisign::KeyPair::generate_unencrypted_keypair().unwrap())
    }
    fn key(&self) -> String {
        self.0.pk.to_base64()
    }
    /// Sign as `minisign -S` does: the trusted comment names the file.
    fn sign(&self, bytes: &[u8]) -> String {
        self.sign_as(bytes, "companion")
    }
    fn sign_as(&self, bytes: &[u8], file: &str) -> String {
        let comment = format!("timestamp:0\tfile:{file}\thashed");
        minisign::sign(Some(&self.0.pk), &self.0.sk, bytes, Some(&comment), None)
            .unwrap()
            .into_string()
    }
}

/// A one-extension registry in `p` serving `companion` (already written) as `dre-mcp`.
fn companion_registry(p: &TestProject, version: &str, minisig: Option<String>, url: Option<&str>) {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(p.read("companion").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let url = url.map_or_else(|| p.path("companion").display().to_string(), str::to_owned);
    let artifact = match minisig {
        Some(sig) => json!({"url": url, "sha256": digest, "minisig": sig}),
        None => json!({"url": url, "sha256": digest}),
    };
    p.write(
        "index.json",
        &json!({"schema": 2, "plugins": [], "extensions": [{"name": "dre-mcp", "kind": "extension",
            "license": "LicenseRef-Proprietary", "proprietary_license": true,
            "versions": [{"version": version, "artifacts": {dre_core::manager::platform(): artifact}}]}]})
        .to_string(),
    );
}

#[cfg(unix)]
#[test]
fn extensions_install_only_with_consent_and_a_trusted_signature() {
    let p = TestProject::new(&[("dre_project.yml", "name: extensions\n")], "");
    let signer = Signer::new();
    p.write("companion", "#!/bin/sh\necho companion-ready\n");
    let signed = signer.sign(p.read("companion").as_bytes());
    companion_registry(&p, "0.1.0-rc.2", Some(signed.clone()), None);
    let key = signer.key();
    let registry = p.path("index.json");
    let env = [
        ("DRE_REGISTRY_URL", registry.to_str().unwrap()),
        ("DRE_TEST_EXTENSION_KEY", key.as_str()),
    ];
    // Not interactive and no flag: nothing is installed, whatever the project says.
    p.dre_env("extension", &["install", "dre-mcp"], &env)
        .failed()
        .says("--accept-proprietary");
    p.dre_env("mcp", &["serve"], &env)
        .failed()
        .says("dre mcp --accept-proprietary");
    p.dre_env("extension", &["install", "dre-mcp", "--accept-proprietary"], &env)
        .ok();
    p.dre("mcp", &[]).ok().says("companion-ready");

    // Changed after signing: the checksum no longer matches.
    p.write("companion", "#!/bin/sh\necho tampered\n");
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("checksum mismatch");
    // A matching checksum but another key's signature.
    companion_registry(
        &p,
        "0.1.0-rc.3",
        Some(Signer::new().sign(p.read("companion").as_bytes())),
        None,
    );
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("signature check failed");
    // A good signature for another file can't be replayed.
    companion_registry(
        &p,
        "0.1.0-rc.3",
        Some(signer.sign_as(p.read("companion").as_bytes(), "other.tar.gz")),
        None,
    );
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("another file");
    // No signature at all.
    companion_registry(&p, "0.1.0-rc.3", None, None);
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("not signed");
    // Plain http, even with a good signature.
    companion_registry(
        &p,
        "0.1.0-rc.3",
        Some(signer.sign(p.read("companion").as_bytes())),
        Some("http://127.0.0.1:9/companion"),
    );
    p.dre_env("extension", &["update", "dre-mcp", "--accept-proprietary"], &env)
        .failed()
        .says("over https");
    // Every rejected update leaves the installed binary usable.
    p.dre("mcp", &[]).ok().says("companion-ready");
}

#[cfg(unix)]
#[test]
fn declared_extensions_keep_their_own_settings_and_still_need_consent() {
    // An `accept_license` from DRE 0.4.1/0.4.2, and a setting DRE has never heard of: both
    // belong to the extension, and neither installs anything by itself.
    let deps = "extensions:\n  - name: dre-mcp\n    version: '0.1.0-rc.2'\n    accept_license: '2026-10'\n    team: {seats: 5}\n";
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: companions\n"),
            ("dependencies.yml", deps),
        ],
        "",
    );
    let signer = Signer::new();
    p.write("companion", "#!/bin/sh\nprintf 'declared-ready\\n'\n");
    companion_registry(
        &p,
        "0.1.0-rc.2",
        Some(signer.sign(p.read("companion").as_bytes())),
        None,
    );
    let key = signer.key();
    let registry = p.path("index.json");
    let env = [
        ("DRE_REGISTRY_URL", registry.to_str().unwrap()),
        ("DRE_TEST_EXTENSION_KEY", key.as_str()),
    ];
    p.dre("validate", &[]).ok();
    // The manifest hands the entry, settings included, to the companion.
    let declared = &p.json("target/manifest.json")["extensions"][0];
    assert_eq!(declared["accept_license"], "2026-10");
    assert_eq!(declared["team"]["seats"], 5);
    p.dre_env("deps", &[], &env)
        .ok()
        .says("needs a yes at the prompt or --accept-proprietary");
    p.dre_env("mcp", &["serve"], &env).failed();
    p.dre_env("deps", &["--accept-proprietary"], &env).ok();
    assert_eq!(p.read("dependencies.yml"), deps);
    // A different executable on PATH never replaces the project's pinned install.
    p.write("wrong/dre-mcp", "#!/bin/sh\nprintf 'wrong-path-version\\n'\n");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p.path("wrong/dre-mcp"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let search = format!(
        "{}:{}",
        p.path("wrong").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    p.dre_env("mcp", &["serve"], &[("PATH", &search)])
        .ok()
        .says("declared-ready");
}

#[test]
fn catalogue_refuses_a_source_type_with_unknown_quoting_rules() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: catalogue\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb]\n"),
            ("tables.json", "{\"tables\":[\"orders\"]}"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n  other:\n    targets:\n      dev: {type: mystery_db, host: example}\n",
    );
    p.duckdb("data.duckdb", "select 1");
    p.dre(
        "catalog",
        &[
            "--profile",
            "other",
            "--tables-file",
            "tables.json",
            "--output-file",
            "c.json",
        ],
    )
    .failed()
    .says("not yet supported for `mystery_db`");
    assert!(!p.path("c.json").exists());
}
