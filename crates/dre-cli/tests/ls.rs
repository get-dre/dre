//! `dre ls`: the reports and Bindings a selection or schedule covers, offline and read-only.

mod common;

use common::TestProject;
use serde_json::{Value, json};

const PROFILES: &str = "\
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
destinations:
  inbox:
    targets:
      dev: {type: local}
";

fn project() -> TestProject {
    TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins:\n  - duckdb\n  - csv\n"),
            (
                "sets.yml",
                "client_a: {vars: {client: a}}\nclient_b: {vars: {client: b}}\n",
            ),
            (
                "schedules.yml",
                "- {name: close_a, report: monthly, set: client_a, cron: \"0 6 1 * *\"}\n\
                 - {name: regulatory, select: \"tag:regulatory\", cron: \"0 6 2 * *\"}\n",
            ),
            (
                "reports/ops/daily/daily.yml",
                "queries: [s]\ntags: [regulatory]\noutput:\n  destination: {profile: inbox, path: out/daily.csv}\n",
            ),
            ("reports/ops/daily/s.sql", "select 1 as n\n"),
            (
                "reports/finance/monthly/monthly.yml",
                "queries: [m]\nsets: [client_a, client_b]\n",
            ),
            ("reports/finance/monthly/m.sql", "select 2 as n\n"),
        ],
        PROFILES,
    )
}

/// `dre ls` data rows (the header dropped), each as its whitespace-separated columns.
fn rows(stdout: &str) -> Vec<Vec<String>> {
    stdout
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

#[test]
fn no_selector_lists_every_binding() {
    let p = project();
    let r = p.dre("ls", &[]);
    r.ok();
    assert!(r.stdout.starts_with("REPORT"), "{}", r.stdout);
    assert_eq!(
        rows(&r.stdout),
        [
            vec!["daily", "-", "warehouse", "csv", "inbox:out/daily.csv"],
            vec!["monthly", "client_a", "warehouse", "csv", "-"],
            vec!["monthly", "client_b", "warehouse", "csv", "-"],
        ]
    );
    assert!(r.stderr.is_empty(), "diagnostics only: {}", r.stderr);
}

#[test]
fn a_selector_and_a_set_narrow_it() {
    let p = project();
    let r = p.dre("ls", &["-s", "tag:regulatory"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["daily", "-", "warehouse", "csv", "inbox:out/daily.csv"]]
    );

    let r = p.dre("ls", &["monthly", "--set", "client_b"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["monthly", "client_b", "warehouse", "csv", "-"]]
    );
}

#[test]
fn a_schedule_lists_exactly_what_it_runs() {
    let p = project();
    let r = p.dre("ls", &["--schedule", "close_a"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["monthly", "client_a", "warehouse", "csv", "-"]]
    );
}

#[test]
fn json_is_the_manifests_shape_for_what_matched() {
    let p = project();
    p.dre("compile", &["--set", "all"]).ok();
    let full = p.json("target/manifest.json");

    let r = p.dre("ls", &["--schedule", "close_a", "--output", "json"]);
    r.ok();
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(j["schema_version"], full["schema_version"]);
    assert_eq!(j["version"], full["version"]);
    assert_eq!(j["project"], full["project"]);
    assert_eq!(j["schedules"], json!({"close_a": full["schedules"]["close_a"]}));
    assert_eq!(j["reports"].as_object().unwrap().len(), 1);
    let monthly = &j["reports"]["monthly"];
    assert_eq!(
        monthly["bindings"],
        json!([full["reports"]["monthly"]["bindings"][0]])
    );
    assert_eq!(monthly["checksum"], full["reports"]["monthly"]["checksum"]);

    // With no selector it's the whole manifest.
    let r = p.dre("ls", &["--output", "json"]);
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    let mut all = full;
    all["schedules"] = json!({});
    assert_eq!(j, all);
}

#[test]
fn nothing_matched_is_an_error() {
    let p = project();
    let r = p.dre("ls", &["nope"]);
    r.failed().says("nope");
    assert!(r.stdout.is_empty());
    p.dre("ls", &["--schedule", "nope"])
        .failed()
        .says("no schedule `nope`");
    p.dre("ls", &["daily", "--set", "client_a"])
        .failed()
        .says("client_a");
}

#[test]
fn missing_profiles_still_list_without_writing_anything() {
    let p = project();
    std::fs::remove_file(p.dir.path().join("profiles/profiles.yml")).unwrap();
    for args in [vec![], vec!["--output", "json"]] {
        let r = p.dre("ls", &args);
        assert_eq!(r.code, 1);
        assert!(r.stderr.contains("error[profiles-missing]"), "{}", r.stderr);
        assert!(r.stdout.contains("daily"), "{}", r.stdout);
    }
    assert!(!p.path("target").exists());
    assert!(!p.path("logs").exists());
}

fn diagnostic_project() -> TestProject {
    TestProject::with_plugins(
        &[
            (
                "dre_project.yml",
                "name: diagnostics\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            ("reports/example/example.yml", "queries: [query]\n"),
            ("reports/example/query.sql", "select 1 as n\n"),
            (
                "sources.yml",
                "sources:\n  - name: sales\n    profile: warehouse\n    tables: [{name: orders}]\n",
            ),
        ],
        PROFILES,
        &["dre-plugin-csv"],
    )
}

#[test]
fn loaded_project_errors_reach_stderr_and_fail_every_listing() {
    let p = diagnostic_project();
    std::fs::remove_file(p.path("reports/example/query.sql")).unwrap();
    for resource in ["report", "source"] {
        for output in ["text", "json"] {
            let r = p.dre("ls", &["--resource-type", resource, "--output", output]);
            assert_eq!(r.code, 1, "stdout: {}\nstderr: {}", r.stdout, r.stderr);
            assert!(r.stderr.contains("error["), "{}", r.stderr);
            assert!(r.stderr.contains("query"), "{}", r.stderr);
            assert!(!r.stdout.is_empty());
            if output == "json" {
                let doc: Value = serde_json::from_str(&r.stdout).unwrap();
                if resource == "report" {
                    assert!(
                        !doc["reports"]["example"]["errors"].as_array().unwrap().is_empty(),
                        "{doc}"
                    );
                } else {
                    assert_eq!(doc.as_object().unwrap().len(), 1);
                    assert!(doc["sources"]["sales"]["tables"]["orders"].is_object());
                }
            }
        }
    }
}

#[test]
fn clean_projects_and_warnings_only_succeed() {
    let p = diagnostic_project();
    for resource in ["report", "source"] {
        p.dre("ls", &["--resource-type", resource]).ok();
    }
    p.write("ignored.yml", "[unrecognized]\n");
    for resource in ["report", "source"] {
        for output in ["text", "json"] {
            let r = p.dre("ls", &["--resource-type", resource, "--output", output]);
            r.ok();
            assert!(r.stderr.contains("warning[unrecognized-yaml]"), "{}", r.stderr);
            if output == "json" {
                serde_json::from_str::<Value>(&r.stdout).unwrap();
            }
        }
    }
}

#[test]
fn failed_project_load_reports_diagnostics_without_listing() {
    let p = diagnostic_project();
    p.write("dre_project.yml", "name: [\n");
    for resource in ["report", "source"] {
        for output in ["text", "json"] {
            let r = p.dre("ls", &["--resource-type", resource, "--output", output]);
            assert_eq!(r.code, 2);
            assert!(r.stderr.contains("error["), "{}", r.stderr);
            assert!(r.stdout.is_empty(), "{}", r.stdout);
        }
    }
}
