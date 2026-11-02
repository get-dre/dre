//! Query data checks use saved results inside core and never format or deliver reports.

mod common;

use common::TestProject;

fn project(report: &str, macros: &str) -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: checks\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture]\n"),
            ("reports/r/r.yml", report),
            ("reports/r/data.sql", "rows 7"),
            ("macros/checks.sql", macros),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    )
}

const POSITIVE: &str = "{% test positive(relation, column='n') %}select * from {{ relation }} where {{ column }} < 0{% endtest %}";

#[test]
fn local_checks_need_no_duckdb_source_or_formatter_and_record_defaults() {
    let p = project(
        "queries:\n  - query: data\n    data_tests:\n      - positive: {arguments: {column: n}}\noutput: {format: message}\n",
        POSITIVE,
    );
    p.dre("test", &["r"]).ok().says("PASS");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["params"]["command"], "test");
    assert_eq!(result["tests"][0]["status"], "pass");
    assert_eq!(result["tests"][0]["failures"], 0);
    assert_eq!(result["tests"][0]["execution"], "local");
    assert_eq!(result["tests"][0]["severity"], "error");
    assert_eq!(result["result_sets"][0]["rows"], 7);
    assert_eq!(result["outputs"], serde_json::json!([]));
    assert_eq!(result["deliveries"], serde_json::json!([]));
    assert_eq!(result["output_results"][0]["status"], "skipped");
    assert!(!p.path("target/run/r/default/.spool").exists());
    let listed = p.dre("ls", &["--output", "json"]);
    listed.ok();
    let manifest: serde_json::Value = serde_json::from_str(&listed.stdout).unwrap();
    assert_eq!(
        manifest["reports"]["r"]["queries"][0]["data_tests"][0]["name"],
        "positive"
    );
}

#[test]
fn data_failures_collect_current_query_checks_and_stop_before_next_query() {
    for command in ["test", "build"] {
        let p = project(
            "queries:\n  - query: data\n    data_tests:\n      - invalid: {}\n      - positive: {}\n  - {query: later, tab: false}\noutput: {format: message}\n",
            &format!(
                "{POSITIVE}\n{{% test invalid(relation) %}}select * from {{{{ relation }}}} where n >= 4{{% endtest %}}"
            ),
        );
        p.write("reports/r/later.sql", "log LATER_SHOULD_NOT_RUN");
        p.dre(command, &["r"])
            .failed()
            .says("3 failure rows")
            .says("PASS")
            .lacks("LATER_SHOULD_NOT_RUN");
        let result = p.json("target/run/r/default/run_results.json");
        assert_eq!(result["error_code"], "data-test-failed");
        assert_eq!(result["tests"].as_array().unwrap().len(), 2);
        assert_eq!(result["tests"][0]["failures"], 3);
        assert_eq!(result["tests"][1]["status"], "pass");
        assert!(!p.run_logs().contains("LATER_SHOULD_NOT_RUN"));
        assert!(!p.path("target/run/r/default/.spool").exists());
    }
}

#[test]
fn execution_error_stops_immediately_and_has_no_invented_failure_count() {
    for command in ["test", "build"] {
        let p = project(
            "queries:\n  - query: data\n    data_tests:\n      - broken: {config: {severity: warn}}\n      - positive: {}\noutput: {format: message}\n",
            &format!(
                "{POSITIVE}\n{{% test broken(relation) %}}select missing_column from {{{{ relation }}}}{{% endtest %}}"
            ),
        );
        p.dre(command, &["r"])
            .failed()
            .says("ERROR")
            .says("missing_column");
        let result = p.json("target/run/r/default/run_results.json");
        assert_eq!(result["error_code"], "data-test-error");
        assert_eq!(result["tests"].as_array().unwrap().len(), 1);
        assert_eq!(result["tests"][0]["status"], "error");
        assert!(result["tests"][0].get("failures").is_none());
        assert!(!p.path("target/run/r/default/.spool").exists());
    }
}

#[test]
fn unsupported_options_fail_clearly_without_opening_the_source() {
    for config in ["execution: invalid", "severity: invalid", "store_failures: true"] {
        let p = project(
            &format!(
                "queries:\n  - query: data\n    data_tests:\n      - positive:\n          config: {{{config}}}\noutput: {{format: message}}\n"
            ),
            POSITIVE,
        );
        p.dre("test", &["r"]).failed().says("data_tests");
        assert!(!p.run_logs().contains("fixture: opened"));
    }
}

#[test]
fn run_skips_declared_checks_and_still_produces_outputs() {
    let p = TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: checks\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture, csv]\n"),
            (
                "reports/r/r.yml",
                "queries:\n  - query: data\n    data_tests: [{broken: {}}]\noutput: {format: csv}\n",
            ),
            ("reports/r/data.sql", "rows 2"),
            (
                "macros/checks.sql",
                "{% test broken(relation) %}this is invalid SQL{% endtest %}",
            ),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture", "dre-plugin-csv"],
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.csv"), "n\r\n0\r\n1\r\n");
    assert!(
        p.json("target/run/r/default/run_results.json")
            .get("tests")
            .is_none()
    );
}

#[test]
fn several_tests_share_the_once_executed_source_result() {
    let p = TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: checks\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb, csv, xlsx]\n"),
            (
                "reports/r/r.yml",
                "queries:\n  - query: data\n    data_tests: [{first_value: {}}, {same_value: {}}]\n  - query: next_value\n    data_tests: [{second_value: {}}]\noutput: [{name: flat, format: csv}, {name: workbook, format: xlsx}]\n",
            ),
            (
                "reports/r/data.sql",
                "select nextval('calls') as id, random() as value",
            ),
            ("reports/r/next_value.sql", "select nextval('calls') as id"),
            (
                "macros/checks.sql",
                "{% test first_value(relation) %}select * from {{ relation }} where id != 1{% endtest %}\n{% test same_value(relation) %}{{ test_first_value(relation) }}{% endtest %}\n{% test second_value(relation) %}select * from {{ relation }} where id != 2{% endtest %}",
            ),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n",
        &["dre-plugin-duckdb", "dre-plugin-csv", "dre-plugin-xlsx"],
    );
    p.duckdb("data.duckdb", "create sequence calls start 1");
    p.dre("build", &["r"]).ok();
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["tests"].as_array().unwrap().len(), 3);
    assert!(p.path("target/run/r/default/workbook.xlsx").exists());
    assert!(!result["outputs"].as_array().unwrap().is_empty());
    assert!(
        result["tests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|test| test["status"] == "pass")
    );
}

#[test]
fn a_set_can_override_checks_and_a_failed_binding_does_not_stop_another() {
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{invalid: {}}]\nsets:\n  - {name: bad, profile: fixture}\n  - name: good\n    profile: fixture\n    queries: [{query: data, data_tests: []}]\noutput: {format: message}\n",
        "{% test invalid(relation) %}select * from {{ relation }}{% endtest %}",
    );
    p.dre("test", &["r", "--set", "all"]).failed();
    let bad = p.json("target/run/r/bad/run_results.json");
    let good = p.json("target/run/r/good/run_results.json");
    assert_eq!(bad["status"], "error");
    assert_eq!(bad["tests"][0]["failures"], 7);
    assert_eq!(good["status"], "success");
    assert!(good.get("tests").is_none());
    assert_eq!(good["result_sets"][0]["rows"], 7);
}

#[test]
fn a_timeout_interrupts_the_local_check_and_removes_its_workspace() {
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{slow: {}}]\noutput: {format: message}\n",
        "{% test slow(relation) %}select sum(a.range * b.range) from range(1000000) a, range(1000000) b{% endtest %}",
    );
    let run = p.dre("test", &["r", "--timeout", "1s"]);
    assert_eq!(run.code, 124, "{run:?}");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["status"], "timed_out");
    assert_eq!(result["tests"][0]["status"], "error");
    assert!(!p.path("target/run/r/default/.spool").exists());
}

#[test]
fn declarations_fail_before_source_execution_and_validate_never_runs_test_bodies() {
    for (declaration, definitions, expected) in [
        ("missing: {}", POSITIVE, "data test `missing` is not defined"),
        ("pkg.positive: {}", POSITIVE, "dre deps"),
        (
            "helper: {}",
            "{% macro helper(relation) %}select 1{% endmacro %}",
            "is not defined",
        ),
        ("positive: {arguments: {typo: n}}", POSITIVE, "no argument `typo`"),
        (
            "needs: {}",
            "{% test needs(relation, column) %}{% endtest %}",
            "requires argument `column`",
        ),
        (
            "needs: {}",
            "{% test needs(column) %}{% endtest %}",
            "must accept the `relation`",
        ),
        (
            "positive: {}",
            "{% test positive(relation) %}{{ config(severity='warn') }}{% endtest %}",
            "configure the declaration in YAML",
        ),
    ] {
        let p = project(
            &format!("queries:\n  - query: data\n    data_tests: [{{{declaration}}}]\n"),
            definitions,
        );
        p.dre("test", &["r"]).failed().says(expected);
        assert!(!p.run_logs().contains("fixture: opened"));
    }
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{opaque: {}}]\noutput: {format: message}\n",
        "{% test opaque(relation, column=run_query('SHOULD_NOT_RUN')) %}{{ run_query('NEVER_RUN') }} select missing_column from {{ relation }}{% endtest %}",
    );
    p.dre("validate", &[]).ok();
    assert!(!p.run_logs().contains("fixture: opened"));
}

#[test]
fn local_package_tests_use_helpers_defaults_and_the_local_duckdb_dialect() {
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{quality.positive: {}}]\noutput: {format: message}\n",
        "",
    );
    p.write("packages.yml", "packages: [{local: ../checks}]\n");
    p.write("../checks/dre_package.yml", "name: quality\n");
    p.write("../checks/macros/checks.sql", "{% macro predicate(column) %}{{ dispatch('predicate', 'quality')(column) }}{% endmacro %}\n{% macro default__predicate(column) %}MUST_NOT_USE_SOURCE_DIALECT{% endmacro %}\n{% macro duckdb__predicate(column) %}{{ column }} < 0{% endmacro %}\n{% test positive(relation, column='n') %}select * from {{ relation }} where {{ predicate(column) }}{% endtest %}");
    p.dre("validate", &[]).ok();
    p.dre("test", &["r"]).ok().says("PASS");
}

#[test]
fn test_definition_errors_keep_the_original_file_and_line() {
    let p = project(
        "queries: [data]\n",
        "{% macro helper() %}ok{% endmacro %}\n\n{% test broken(relation) %}\n{% if %}\n{% endtest %}",
    );
    p.dre("validate", &[]).failed().says("macros/checks.sql:4");
    p.write("macros/checks.sql", "{% test positive(relation) %}{% endtest %}");
    p.write(
        "macros/other.sql",
        "{% macro test_positive(relation) %}{% endmacro %}",
    );
    p.dre("validate", &[])
        .failed()
        .says("duplicate macro/test definition `test_positive`");
}

#[test]
fn explicit_test_macros_work_and_runtime_errors_keep_definition_lines() {
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{positive: {}}]\noutput: {format: message}\n",
        "{% macro helper(column) %}{{ column }} < 0{% endmacro %}\n{% macro test_positive(relation, column='n') %}select * from {{ relation }} where {{ helper(column) }}{% endmacro %}",
    );
    p.dre("test", &["r"]).ok().says("PASS");
    p.write("macros/checks.sql", "{% macro helper() %}ok{% endmacro %}\n\n{%- test positive(relation) -%}\n{{ unavailable_helper() }}\n{%- endtest -%}");
    p.dre("test", &["r"])
        .failed()
        .says("macros/checks.sql:4")
        .says("undefined is not callable");
}

fn production_project(report: &str, macros: &str, record: &std::path::Path) -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: checks\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture, csv]\n"),
            ("reports/r/r.yml", report),
            ("reports/r/data.sql", "rows 7"),
            ("reports/r/later.sql", "log LATER_QUERY_RAN"),
            ("macros/checks.sql", macros),
        ],
        &format!(
            "connections:\n  fixture:\n    targets:\n      dev: {{type: fixture}}\ndestinations:\n  rec:\n    targets:\n      dev: {{type: fixture, dir: '{}' }}\n",
            record.display()
        ),
        &["dre-source-fixture", "dre-destination-fixture", "dre-plugin-csv"],
    )
}

const BAD_DATA: &str = "{% test invalid(relation) %}select * from {{ relation }}{% endtest %}";
const DELIVER: &str = "output: {format: csv, destination: {profile: rec, path: report.csv}}\n";

#[test]
fn warnings_allow_later_queries_formatting_and_delivery_but_test_never_produces_reports() {
    for command in ["test", "build"] {
        let record = tempfile::tempdir().unwrap();
        let p = production_project(
            &format!(
                "queries:\n  - query: data\n    data_tests: [{{invalid: {{config: {{severity: warn}}}}}}, {{positive: {{}}}}]\n  - {{query: later, tab: false}}\n{DELIVER}"
            ),
            &format!("{BAD_DATA}\n{POSITIVE}"),
            record.path(),
        );
        p.dre(command, &["r"]).ok().says("WARN").says("PASS");
        let result = p.json("target/run/r/default/run_results.json");
        assert_eq!(result["params"]["command"], command);
        assert_eq!(result["status"], "success");
        assert_eq!(result["tests"][0]["status"], "warn");
        assert_eq!(result["tests"][0]["failures"], 7);
        assert_eq!(result["tests"][0]["severity"], "warn");
        assert!(p.run_logs().contains("LATER_QUERY_RAN"));
        assert_eq!(
            record.path().join("deliveries.jsonl").exists(),
            command == "build"
        );
        assert_eq!(
            p.path("target/run/r/default/report.csv").exists(),
            command == "build"
        );
    }
}

#[test]
fn failed_build_collects_checks_skips_outputs_and_delivers_only_independent_sets() {
    let record = tempfile::tempdir().unwrap();
    let p = production_project(
        &format!(
            "queries:\n  - query: data\n    data_tests: [{{invalid: {{}}}}, {{positive: {{}}}}]\n  - {{query: later, tab: false}}\nsets:\n  - {{name: bad, profile: fixture}}\n  - name: good\n    profile: fixture\n    queries: [{{query: data, data_tests: []}}]\n{DELIVER}"
        ),
        &format!("{BAD_DATA}\n{POSITIVE}"),
        record.path(),
    );
    p.dre("build", &["r", "--set", "all", "--threads", "1"])
        .failed()
        .says("PASS");
    let bad = p.json("target/run/r/bad/run_results.json");
    assert_eq!(bad["tests"].as_array().unwrap().len(), 2);
    assert_eq!(bad["output_results"][0]["status"], "skipped");
    assert_eq!(bad["output_results"][0]["reason"], "data_test_failed");
    assert_eq!(bad["outputs"], serde_json::json!([]));
    assert_eq!(bad["deliveries"], serde_json::json!([]));
    assert!(!p.path("target/run/r/bad/report.csv").exists());
    assert_eq!(p.json("target/run/r/good/run_results.json")["status"], "success");
    assert_eq!(
        std::fs::read_to_string(record.path().join("deliveries.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(!p.read("target/run/r/bad/dre.log").contains("LATER_QUERY_RAN"));
    assert!(p.path("target/run/r/good/report.csv").exists());
}

#[test]
fn checked_commands_resolve_schedule_set_profile_target_and_variables() {
    for command in ["test", "build"] {
        let record = tempfile::tempdir().unwrap();
        let p = production_project(
            &format!(
                "queries:\n  - query: data\n    data_tests: [{{positive: {{}}}}]\nsets: [chosen, excluded]\n{DELIVER}"
            ),
            POSITIVE,
            record.path(),
        );
        p.write("sets.yml", "chosen: {}\nexcluded: {}\n");
        p.write(
            "schedules.yml",
            "- name: daily\n  report: r\n  set: chosen\n  cron: '0 7 * * *'\n  vars: {count: 3}\n",
        );
        p.write("reports/r/data.sql", "rows {{ var('count') }}");
        p.dre(
            command,
            &[
                "--schedule",
                "daily",
                "--set",
                "chosen",
                "--profile",
                "fixture",
                "--target",
                "dev",
                "--var",
                "count=4",
            ],
        )
        .ok();
        let result = p.json("target/run/r/chosen/run_results.json");
        assert_eq!(result["schedule"], "daily");
        assert_eq!(result["result_sets"][0]["rows"], 4);
        assert_eq!(result["tests"][0]["status"], "pass");
        assert!(!p.path("target/run/r/excluded").exists());
        assert_eq!(
            record.path().join("deliveries.jsonl").exists(),
            command == "build"
        );
    }
}

#[test]
fn warning_execution_errors_skip_remaining_checks_queries_and_all_report_production() {
    let record = tempfile::tempdir().unwrap();
    let p = production_project(
        &format!(
            "queries:\n  - query: data\n    data_tests: [{{broken: {{config: {{severity: warn}}}}}}, {{positive: {{}}}}]\n  - {{query: later, tab: false}}\n{DELIVER}"
        ),
        &format!(
            "{POSITIVE}\n{{% test broken(relation) %}}select missing_column from {{{{ relation }}}}{{% endtest %}}"
        ),
        record.path(),
    );
    p.dre("build", &["r"]).failed().says("ERROR").lacks("PASS");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["error_code"], "data-test-error");
    assert_eq!(result["tests"].as_array().unwrap().len(), 1);
    assert_eq!(result["tests"][0]["severity"], "warn");
    assert!(result["tests"][0].get("failures").is_none());
    assert_eq!(result["output_results"][0]["status"], "skipped");
    assert!(!p.run_logs().contains("LATER_QUERY_RAN"));
    assert!(!p.path("target/run/r/default/report.csv").exists());
    assert!(!record.path().join("deliveries.jsonl").exists());
}

fn source_project(tests: &str, definitions: &str) -> TestProject {
    TestProject::with_plugins(
        &[
            (
                "dre_project.yml",
                "name: source_checks\ndefault_profile: unused\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, fixture]\n"),
            (
                "reports/r/r.yml",
                &format!(
                    "queries:\n  - query: prepare\n    profile: actual\n    tab: false\n    data_tests: [{tests}]\n  - {{query: later, profile: actual}}\noutput: {{format: message}}\n"
                ),
            ),
            (
                "reports/r/prepare.sql",
                "create temp table saved as select nextval('calls') as id; create temp view checked as select * from saved",
            ),
            ("reports/r/later.sql", "select nextval('calls') as id"),
            ("macros/checks.sql", definitions),
        ],
        "connections:\n  unused:\n    targets:\n      dev: {type: fixture}\n  actual:\n    targets:\n      dev: {type: duckdb, path: source.duckdb}\n",
        &["dre-plugin-duckdb", "dre-source-fixture"],
    )
}

#[test]
fn source_tests_inspect_temp_relations_on_actual_session_before_next_query() {
    let p = source_project(
        "native: {arguments: {relation: checked}, config: {execution: source}}, once: {arguments: {relation: saved}, config: {execution: source}}",
        "{% macro default__predicate() %}INVALID_DIALECT{% endmacro %}\n{% macro duckdb__predicate() %}id != 1 or list_extract([id], 1) != 1{% endmacro %}\n{% test native(relation) %}select * from {{ relation }} where {{ dispatch('predicate')() }}{% endtest %}\n{% test once(relation) %}select * from {{ relation }} where id != 1{% endtest %}",
    );
    p.duckdb("source.duckdb", "create sequence calls start 1");
    p.dre("test", &["r"]).ok().says("PASS");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["tests"].as_array().unwrap().len(), 2);
    assert!(
        result["tests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["execution"] == "source" && t["status"] == "pass")
    );
    // Two sequence calls: preparation and the later query, regardless of check count.
    p.write("reports/r/prepare.sql", "select nextval('calls') as id");
    p.write(
        "reports/r/r.yml",
        "queries: [{query: prepare, profile: actual}]\noutput: {format: message}\n",
    );
    p.dre("run", &["r", "--output-json", "saved"]).ok();
    assert!(p.read("saved/r/default/prepare.json").contains('3'));
}

#[test]
fn source_failures_collect_checks_and_errors_stop_even_at_warn_severity() {
    for command in ["test", "build"] {
        for broken in [false, true] {
            let p = source_project(
                &format!(
                    "check: {{arguments: {{relation: checked}}, config: {{execution: source, severity: {}}}}}, pass: {{arguments: {{relation: saved}}, config: {{execution: source}}}}",
                    if broken { "warn" } else { "error" }
                ),
                &format!(
                    "{{% test check(relation) %}}select {} from {{{{ relation }}}}{{% endtest %}}\n{{% test pass(relation) %}}select * from {{{{ relation }}}} where id != 1{{% endtest %}}",
                    if broken { "missing_column" } else { "*" }
                ),
            );
            p.duckdb("source.duckdb", "create sequence calls start 1");
            p.write("reports/r/later.sql", "select * from LATER_SHOULD_NOT_RUN");
            p.dre(command, &["r"]).failed().lacks("LATER_SHOULD_NOT_RUN");
            let result = p.json("target/run/r/default/run_results.json");
            assert_eq!(
                result["tests"].as_array().unwrap().len(),
                if broken { 1 } else { 2 }
            );
            assert_eq!(
                result["tests"][0]["status"],
                if broken { "error" } else { "fail" }
            );
            if broken {
                assert!(result["tests"][0].get("failures").is_none());
            } else {
                assert_eq!(result["tests"][0]["failures"], 1);
            }
        }
    }
}

#[test]
fn source_tests_require_sessions_without_falling_back_to_local() {
    let p = project(
        "queries:\n  - query: data\n    data_tests: [{positive: {arguments: {relation: explicit_table}, config: {execution: source, severity: warn}}}]\noutput: {format: message}\n",
        POSITIVE,
    );
    p.dre_env("test", &["r"], &[("DRE_FIXTURE_MODE", "no_sessions")])
        .failed()
        .says("support sessions");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["tests"][0]["status"], "error");
    assert!(result["tests"][0].get("failures").is_none());
}

#[test]
fn source_tests_require_an_explicit_nonempty_string_relation_before_execution() {
    for arguments in ["{}", "{relation: null}", "{relation: 123}", "{relation: ''}"] {
        let p = project(
            &format!(
                "queries:\n  - query: data\n    data_tests: [{{positive: {{arguments: {arguments}, config: {{execution: source}}}}}}]\noutput: {{format: message}}\n"
            ),
            POSITIVE,
        );
        p.dre("test", &["r"])
            .failed()
            .says("explicit nonempty string arguments.relation");
        assert!(!p.run_logs().contains("fixture: opened"));
    }
}

#[test]
fn source_warning_records_count_and_continues_to_next_query() {
    let p = source_project(
        "invalid: {arguments: {relation: saved}, config: {execution: source, severity: warn}}",
        "{% test invalid(relation) %}select * from {{ relation }}{% endtest %}",
    );
    p.duckdb("source.duckdb", "create sequence calls start 1");
    p.dre("test", &["r"]).ok().says("WARN");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["tests"][0]["failures"], 1);
    assert_eq!(result["tests"][0]["status"], "warn");
    assert_eq!(result["result_sets"][0]["query"], "later");
}

#[test]
fn empty_preparation_query_does_not_silently_skip_source_checks() {
    let p = source_project(
        "check: {arguments: {relation: saved}, config: {execution: source}}",
        "{% test check(relation) %}select * from {{ relation }}{% endtest %}",
    );
    p.write("reports/r/prepare.sql", "-- no statement");
    p.dre("test", &["r"])
        .failed()
        .says("attached data tests but no executable statements");
    assert!(!p.run_logs().contains("fixture: opened"));
}
