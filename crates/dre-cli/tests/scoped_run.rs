//! Selected runs isolate report errors; validate remains the whole-project gate.
mod common;

use common::TestProject;
use serde_json::json;

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: scoped\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture, csv]\n"),
            ("reports/good/good.yml", "queries: [good]\n"),
            ("reports/good/good.sql", "rows 2"),
            ("reports/bad/bad.yml", "queries: [bad]\nunknown: true\n"),
            ("reports/bad/bad.sql", "rows 1"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture", "dre-plugin-csv"],
    )
}

#[test]
fn selected_run_records_other_report_errors_without_building_a_manifest() {
    let p = project();
    let result = p.dre("run", &["good"]);
    result.ok();
    assert!(
        result
            .stderr
            .contains("1 other report has errors; run dre validate"),
        "{result:?}"
    );
    let results = p.json("target/run/good/default/run_results.json");
    assert_eq!(
        results["project_errors"],
        json!([{
            "path": "reports/bad/bad.yml",
            "code": "unknown-key",
            "message": "unknown report key `unknown`"
        }])
    );
    assert!(results["manifest_checksum"].is_null());
    assert!(!p.path("target/manifest.json").exists());
}

#[test]
fn unselected_reports_do_not_need_their_plugins_or_profiles() {
    let p = project();
    p.write(
        "reports/bad/bad.yml",
        "queries: [bad]\nprofile: absent\noutput: {format: unavailable}\n",
    );
    p.write(
        "reports/plugin/plugin.yml",
        "queries: [plugin]\noutput: {format: unavailable}\n",
    );
    p.write("reports/plugin/plugin.sql", "rows 1");
    let run = p.dre("run", &["good"]);
    run.ok();
    assert!(run.stderr.contains("2 other reports have errors"), "{run:?}");
    let results = p.json("target/run/good/default/run_results.json");
    let errors = results["project_errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0]["code"], "unknown-profile");
    assert_eq!(errors[0]["path"], "reports/bad/bad.yml");
    assert_eq!(errors[1]["code"], "undeclared-plugin");
    assert_eq!(errors[1]["path"], "reports/plugin/plugin.yml");
    p.dre("validate", &[]).failed().says("absent");
}

#[test]
fn source_selectors_still_resolve_before_scoping_the_run() {
    let p = project();
    p.write(
        "sources.yml",
        "sources: [{name: sales, profile: fixture, tables: [{name: orders}]}]\n",
    );
    p.write(
        "reports/good/good.sql",
        "select * from {{ source('sales', 'orders') }}",
    );
    p.dre("run", &["source:sales.orders", "--dry-run"]).ok();
    assert!(p.path("target/compiled/good/default/good.sql").exists());
    assert!(!p.path("target/compiled/bad/default/bad.sql").exists());
}

#[test]
fn schedule_runs_isolate_reports_outside_the_schedule() {
    let p = project();
    p.write(
        "schedules.yml",
        "- {name: nightly, report: good, cron: '0 6 * * *'}\n",
    );
    p.dre("run", &["--schedule", "nightly"]).ok();
    assert!(p.path("target/run/good/default/run_results.json").exists());
    assert!(!p.path("target/run/bad/default/run_results.json").exists());
}

#[test]
fn other_report_template_errors_are_discovered_without_parsing_their_bindings() {
    let p = project();
    p.write("reports/bad/bad.yml", "queries: [bad]\n");
    p.write("reports/bad/bad.sql", "select {{ var('missing') }}");
    p.dre("run", &["good"]).ok();
    let results = p.json("target/run/good/default/run_results.json");
    assert_eq!(results["project_errors"][0]["code"], "unresolved-var");
    assert_eq!(results["project_errors"][0]["path"], "reports/bad/bad.sql");
    p.dre("validate", &["-s", "good"]).failed().says("unresolved-var");
}

#[test]
fn an_uninstalled_package_used_only_elsewhere_does_not_block_a_selected_run() {
    let p = project();
    p.write("dependencies.yml", "plugins: [fixture, csv, uninstalled]\n");
    p.write(
        "reports/bad/bad.yml",
        "queries: [bad]\noutput: {format: uninstalled}\n",
    );
    p.dre("run", &["good", "--no-auto-install"]).ok();
}

#[test]
fn an_empty_schedule_intersection_still_explains_the_schedules_real_members() {
    let p = project();
    p.write("reports/bad/bad.yml", "queries: [bad]\n");
    p.write(
        "schedules.yml",
        "- {name: nightly, report: good, cron: '0 6 * * *'}\n",
    );
    p.dre("run", &["bad", "--schedule", "nightly"])
        .failed()
        .says("it runs: good");
}

#[test]
fn scoped_results_validate_against_the_published_schema_even_when_the_query_fails() {
    let p = project();
    p.write("reports/good/good.sql", "fail");
    p.dre("run", &["good"]).failed();
    let results = p.json("target/run/good/default/run_results.json");
    assert_eq!(results["status"], "error");
    assert_eq!(results["project_errors"][0]["code"], "unknown-key");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/run-results.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&results).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn run_preserves_an_existing_manifest_on_success_and_load_failure() {
    let p = project();
    p.write("target/manifest.json", "a prior whole-project snapshot\n");
    p.dre("run", &["good"]).ok();
    assert_eq!(p.read("target/manifest.json"), "a prior whole-project snapshot\n");
    p.write("dre_project.yml", "name: [\n");
    p.dre("run", &["good"]).failed();
    assert_eq!(p.read("target/manifest.json"), "a prior whole-project snapshot\n");
}

#[test]
fn malformed_reports_are_reported_elsewhere_but_fail_when_selected() {
    let p = project();
    p.write("reports/bad/bad.yml", "queries: [\n");
    let run = p.dre("run", &["good"]);
    run.ok();
    assert!(run.stderr.contains("1 other report has errors"), "{run:?}");
    assert_eq!(
        p.json("target/run/good/default/run_results.json")["project_errors"][0]["code"],
        "yaml-syntax"
    );
    p.dre("run", &["bad"]).failed().says("yaml-syntax");
    p.dre("validate", &["-s", "good"]).failed().says("yaml-syntax");
}

#[test]
fn selected_transitive_refs_keep_shared_dependency_errors_fatal() {
    let p = project();
    p.write("reports/bad/bad.yml", "queries: [bad]\n");
    p.write("reports/bad/bad.sql", "select {{ ref('missing') }}");
    p.dre("run", &["good"]).ok();
    assert_eq!(
        p.json("target/run/good/default/run_results.json")["project_errors"][0]["code"],
        "unknown-ref"
    );
    p.write("reports/good/good.sql", "select * from ({{ ref('bad') }})");
    p.dre("run", &["good"]).failed().says("unknown-ref");
}

#[test]
fn project_wide_dependencies_and_malformed_mixed_role_yaml_still_block_a_run() {
    let p = project();
    p.write("sources.yml", "sources: [42]\n");
    p.dre("run", &["good"]).failed().says("invalid-source");
    std::fs::remove_file(p.path("sources.yml")).unwrap();
    p.write("reports/bad/bad.yml", "sources: [\nqueries: [bad]\n");
    p.dre("run", &["good"]).failed().says("yaml-syntax");
}

#[test]
fn other_report_messages_follow_secret_masking_in_text_and_json_logs() {
    let p = project();
    p.write("reports/bad/bad.yml", "queries: [bad]\n");
    p.write("reports/bad/bad.sql", "select {{ var('sensitive-value') }}");
    let run = p.dre_env(
        "run",
        &["good", "--log-format", "json"],
        &[("DRE_SECRET_TEST", "sensitive-value")],
    );
    run.ok();
    assert!(!run.stdout.contains("sensitive-value"), "{run:?}");
    assert!(!run.stderr.contains("sensitive-value"), "{run:?}");
    let results = p.json("target/run/good/default/run_results.json");
    assert!(!results["project_errors"].to_string().contains("sensitive-value"));
    assert!(
        results["project_errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("*****")
    );
    assert!(run.stderr.contains("1 other report has errors"), "{run:?}");
    for line in run.stdout.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[test]
fn a_missing_profile_records_every_report_that_uses_it() {
    for setting in [
        "profile: absent",
        "output: {destination: {profile: absent, path: out.csv}}",
    ] {
        let p = project();
        p.write("reports/bad/bad.yml", &format!("queries: [bad]\n{setting}\n"));
        p.write(
            "reports/other/other.yml",
            &format!("queries: [other]\n{setting}\n"),
        );
        p.write("reports/other/other.sql", "rows 1");
        let run = p.dre("run", &["good"]);
        run.ok();
        assert!(
            run.stderr.contains("2 other reports have errors"),
            "{setting}: {run:?}"
        );
        let results = p.json("target/run/good/default/run_results.json");
        let errors = results["project_errors"].as_array().unwrap();
        assert_eq!(errors.len(), 2, "{setting}: {errors:?}");
        assert_eq!(errors[0]["path"], "reports/bad/bad.yml");
        assert_eq!(errors[1]["path"], "reports/other/other.yml");
        assert!(
            errors.iter().all(|error| error["code"] == "unknown-profile"),
            "{setting}: {errors:?}"
        );
        p.write("reports/good/good.yml", &format!("queries: [good]\n{setting}\n"));
        p.dre("run", &["good"]).failed().says("absent");
    }
}

#[test]
fn a_schedule_for_an_unbuilt_report_does_not_block_another_report() {
    let p = project();
    p.write("reports/bad/bad.yml", "name: bad\noutput: {format: csv}\n");
    std::fs::remove_file(p.path("reports/bad/bad.sql")).unwrap();
    p.write(
        "schedules.yml",
        "- {name: nightly, report: bad, cron: '0 6 * * *'}\n",
    );
    p.dre("run", &["good"]).ok();
    let results = p.json("target/run/good/default/run_results.json");
    assert_eq!(results["project_errors"][0]["path"], "reports/bad/bad.yml");
    p.dre("run", &["bad"]).failed();
    p.dre("run", &["--schedule", "nightly"]).failed();
    p.dre("validate", &[]).failed();
    p.write(
        "schedules.yml",
        "- {name: nightly, report: nonexistent, cron: '0 6 * * *'}\n",
    );
    p.dre("run", &["good"]).failed().says("doesn't exist");
}

#[test]
fn a_schedule_can_name_a_set_of_a_report_that_cannot_be_built() {
    let p = project();
    p.write("reports/bad/bad.yml", "name: bad\nsets: [{name: daily}]\n");
    std::fs::remove_file(p.path("reports/bad/bad.sql")).unwrap();
    p.write(
        "schedules.yml",
        "- {name: nightly, report: bad, set: daily, cron: '0 6 * * *'}\n",
    );
    p.dre("run", &["good"]).ok();
    p.dre("run", &["--schedule", "nightly"]).failed();
    p.dre("validate", &[]).failed();
}

#[test]
fn malformed_flow_yaml_with_global_keys_still_blocks_a_selected_report() {
    for yaml in [
        "{sources: [], queries: [bad]",
        r#"{"sour\u0063es": [], queries: [bad]"#,
        "{queries: [bad], 'plugins': [csv]",
    ] {
        let p = project();
        p.write("reports/bad/bad.yml", yaml);
        p.dre("run", &["good"]).failed().says("yaml-syntax");
    }
    let p = project();
    p.write(
        "reports/bad/bad.yml",
        "{queries: [bad], meta: {sources: [], note: 'plugins: [csv]'}",
    );
    p.dre("run", &["good"]).ok();
}

#[test]
fn unreadable_yaml_whose_role_is_unknown_is_a_project_error() {
    let p = project();
    std::fs::write(p.path("reports/bad/bad.yml"), [0xff]).unwrap();
    p.dre("run", &["good"]).failed().says("io-error");
}
