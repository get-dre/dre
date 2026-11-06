//! Machine-readable CLI contracts, exercised through a real fixture plugin.

mod common;

use common::TestProject;
use serde_json::Value;

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: contracts\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture]\n"),
            ("reports/r/r.yml", "queries: [data]\noutput: {format: message}\n"),
            ("reports/r/data.sql", "rows 2"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    )
}

#[test]
fn every_json_log_event_names_its_contract_version() {
    let p = project();
    let run = p.dre("run", &["r", "--log-format", "json"]);
    run.ok();
    let events: Vec<Value> = run
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["event"] == "binding_end"), "{run:?}");
    for event in events {
        assert_eq!(event["schema_version"], "dre/events/v1", "{event}");
    }
}

#[test]
fn validation_json_is_versioned_for_success_and_failure() {
    for valid in [true, false] {
        let p = project();
        if !valid {
            p.write("reports/r/r.yml", "queries: [data]\nunknown_option: true\n");
        }
        let run = p.dre("validate", &["--json", "--no-auto-install"]);
        if valid {
            run.ok();
        } else {
            run.failed();
        }
        let document: Value = serde_json::from_str(&run.stdout).unwrap();
        assert_eq!(document["schema_version"], "dre/validate/v1", "{document}");
        assert_eq!(document["ok"], valid);
    }
}

#[test]
fn binding_events_results_and_exit_codes_agree_on_success_and_failure() {
    for (sql, status, code) in [("rows 2", "succeeded", 0), ("coded", "failed", 1)] {
        let p = project();
        p.write("reports/r/data.sql", sql);
        let run = p.dre("run", &["r", "--log-format", "json"]);
        assert_eq!(run.code, code, "{run:?}");
        let end = run
            .stdout
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["event"] == "binding_end")
            .unwrap();
        assert_eq!(end["status"], status, "{end}");
        let results = p.json("target/run/r/default/run_results.json");
        assert_eq!(results["status"], status);
        assert_eq!(results["schema_version"], "dre/run-results/v2");
    }
}

#[test]
fn plans_and_manifests_expose_outputs_without_legacy_first_output_fields() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries: [data]\noutput:\n  - {name: first, format: message}\n  - {name: second, format: message}\n",
    );
    let run = p.dre("validate", &["--json", "--no-auto-install"]);
    run.ok();
    let document: Value = serde_json::from_str(&run.stdout).unwrap();
    let plan = &document["compiled"][0];
    for legacy in ["format", "output", "destinations"] {
        assert!(plan.get(legacy).is_none(), "legacy `{legacy}` in {plan}");
    }
    assert_eq!(plan["outputs"][0]["name"], "first");
    assert_eq!(plan["outputs"][1]["name"], "second");
    let manifest = p.json("target/manifest.json");
    let binding = &manifest["reports"]["r"]["bindings"][0];
    for legacy in ["output", "destinations"] {
        assert!(binding.get(legacy).is_none(), "legacy `{legacy}` in {binding}");
    }
    assert_eq!(binding["outputs"][0]["name"], "first");
    assert_eq!(binding["outputs"][1]["name"], "second");
    assert_eq!(manifest["schema_version"], "dre/manifest/v4");
}

#[test]
fn output_name_refuses_to_silently_ignore_a_per_row_first_output() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries: [data]\noutput: {format: message, for_each: {query: data, key: n}}\n",
    );
    let run = p.dre("run", &["r", "--output-name", "override.md"]);
    assert_eq!(run.code, 2, "{run:?}");
    run.says("--output-name").says("for_each");
    assert!(!p.path("target/run/r/default/current").exists());
}
