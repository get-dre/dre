mod common;

use common::TestProject;

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: contract\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture]\n"),
            ("reports/r/r.yml", "queries: [data]\noutput: {format: message}\n"),
            ("reports/r/data.sql", "rows 2"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    )
}

#[test]
fn a_busy_binding_has_a_distinct_exit_code() {
    let p = project();
    let runs = dre_core::runs::BindingRuns::new(&p.path("target/run/r/default"));
    let _guard = runs.lock("held", chrono::Utc::now(), "contract-test").unwrap();
    let out = p.dre_env("run", &["r"], &[("DRE_HOST_NAME", "contract-test")]);
    assert_eq!(out.code, 75, "{out:?}");
    out.says("already running");
    assert!(!p.path("target/run/r/default/current").exists());
}

#[test]
fn scheduled_invocations_run_data_tests_before_outputs() {
    let p = project();
    p.write(
        "schedules.yml",
        "- name: daily\n  report: r\n  cron: '0 7 * * *'\n  timezone: UTC\n",
    );
    p.write(
        "reports/r/r.yml",
        "queries:\n  - query: data\n    data_tests: [{invalid: {}}]\noutput: {format: message}\n",
    );
    p.write(
        "macros/checks.sql",
        "{% test invalid(relation) %}select * from {{ relation }}{% endtest %}",
    );
    let out = p.dre(
        "schedule",
        &[
            "ls",
            "--from",
            "2026-01-25",
            "--to",
            "2026-01-26",
            "--output",
            "json",
        ],
    );
    out.ok();
    let doc: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let invocation = &doc["occurrences"][0]["invocation"];
    let argv: Vec<&str> = invocation["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let env: Vec<(&str, &str)> = invocation["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
        .collect();
    let run = p.dre_env(argv[1], &argv[2..], &env);
    assert_eq!(run.code, 1, "{run:?}");
    let result = p.json("target/run/r/default/run_results.json");
    assert_eq!(result["error_code"], "data-test-failed");
    assert_eq!(result["outputs"], serde_json::json!([]));
}

#[test]
fn env_var_preserves_default_types_in_profiles_and_sql() {
    let p = project();
    let expression = "{{ 'typed default' if env_var('DRE_CONTRACT_UNSET_DEFAULT', false) is boolean else 'string default' }}";
    std::fs::write(p.dir.path().join("profiles/profiles.yml"), format!("connections:\n  fixture:\n    targets:\n      dev:\n        type: fixture\n        fail: \"{expression}\"\n")).unwrap();
    let out = p.dre("run", &["r"]);
    assert_eq!(out.code, 1, "{out:?}");
    out.says("typed default").lacks("string default");
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
    )
    .unwrap();
    p.write(
        "reports/r/data.sql",
        "rows {{ 2 if env_var('DRE_CONTRACT_UNSET_DEFAULT', false) is boolean else 4 }}",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.json("target/run/r/default/run_results.json")["result_sets"][0]["rows"],
        2
    );
}

#[test]
fn scheduled_runs_refuse_paused_excepted_and_non_firing_instants() {
    for (extra, at) in [
        ("enabled: false", "2026-01-24T20:00:00Z"),
        ("except: ['2026-01-25']", "2026-01-24T20:00:00Z"),
        ("", "2026-01-24T21:00:00Z"),
    ] {
        let p = project();
        p.write(
            "schedules.yml",
            &format!(
                "- name: daily\n  report: r\n  cron: '0 7 * * *'\n  timezone: Australia/Sydney\n  {extra}\n"
            ),
        );
        let out = p.dre_env("run", &["--schedule", "daily"], &[("DRE_RUN_AT", at)]);
        assert_eq!(out.code, 2, "{extra}: {out:?}");
        assert!(!p.path("target/run/r/default/current").exists());
        // A manual report selection is still available.
        p.dre("run", &["r"]).ok();
    }
    let p = project();
    p.write("schedules.yml", "- name: daily\n  report: r\n  cron: '0 7 * * *'\n  timezone: Australia/Sydney\n  except: ['2026-01-25']\n");
    // Date-only dispatch also honours the exception.
    assert_eq!(p.dre("run", &["--schedule", "daily"]).code, 2);
    p.dre_env(
        "run",
        &["--schedule", "daily"],
        &[("DRE_RUN_AT", "2026-01-25T20:00:00Z")],
    )
    .ok();
}

#[test]
fn null_clears_inherited_destination_when_and_template() {
    for inherited in [
        "destination: {profile: local, path: sent.md}",
        "when: false",
        "template: {file: inherited.xlsx}",
    ] {
        let p = project();
        p.write(
            "dre_project.yml",
            &format!("name: contract\ndefault_profile: fixture\ndefault_output: {{{inherited}}}\n"),
        );
        p.write("reports/r/r.yml", "queries: [data]\noutput:\n  format: message\n  destination: null\n  when: null\n  template: null\n");
        let out = p.dre("run", &["r"]);
        out.ok();
        let result = p.json("target/run/r/default/run_results.json");
        assert_eq!(
            result["outputs"].as_array().unwrap().len(),
            1,
            "{inherited}: {result}"
        );
        assert_eq!(result["deliveries"], serde_json::json!([]), "{inherited}");
        assert!(!p.path("sent.md").exists(), "{inherited}");
    }
}

#[test]
fn dry_run_is_removed_and_compile_is_the_preview_command() {
    let p = project();
    let out = p.dre("run", &["r", "--dry-run"]);
    assert_eq!(out.code, 2, "{out:?}");
    out.says("unexpected argument '--dry-run'");
    assert!(!p.path("target/run/r/default/current").exists());
    p.dre("compile", &["r"]).ok();
    assert_eq!(p.read("target/compiled/r/default/data.sql"), "rows 2");
}
