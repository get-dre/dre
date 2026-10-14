//! What people (and CI) see: levels, JSON lines, colour and the log file.

mod common;

/// Status lines: those with a verb in the 10-column gutter (not wrapped error text).
fn status_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.get(..10).is_some_and(|g| !g.trim().is_empty()))
        .collect()
}

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

fn project() -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            (
                "reports/ops/daily/daily.yml",
                "queries:\n  - {query: setup, tab: false}\n  - summary\n",
            ),
            (
                "reports/ops/daily/setup.sql",
                "create temp table t as select 1 as n union all select 2\n",
            ),
            ("reports/ops/daily/summary.sql", "select * from t\n"),
            ("reports/ops/broken/broken.yml", "queries: [bad]\n"),
            ("reports/ops/broken/bad.sql", "select nope\n"),
        ],
        DUCK_PROFILES,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

#[test]
fn default_output_is_one_line_per_binding_and_a_summary() {
    let p = project();
    let r = p.dre("run", &[]);
    r.failed();
    let lines = status_lines(&r.stdout);
    assert_eq!(lines.len(), 5, "{}", r.stdout);
    assert_eq!(lines[0], "    Target  dev (default)");
    assert_eq!(lines[1], "   Running  2 Bindings");
    assert!(
        lines[2].starts_with("    Failed  [") && lines[2].contains("broken  reports/ops/broken/bad.sql:1:"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].starts_with(" Succeeded  [")
            && lines[3].ends_with("daily  1 result set, 2 rows → daily.csv"),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].starts_with("  Finished  'run' in ") && lines[4].ends_with(" · 1 succeeded, 1 failed"),
        "{}",
        lines[4]
    );
}

#[test]
fn verbose_shows_every_step() {
    let p = project();
    let r = p.dre("run", &["daily", "-v"]);
    r.ok();
    for needle in [
        "   Started  daily",
        "  Rendered  reports/ops/daily/setup.sql",
        "  Executed  [",
        "reports/ops/daily/setup.sql:1  no result set",
        "reports/ops/daily/summary.sql:1  2 rows",
        "     Wrote  [",
        "      Kept  no destination declared",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in:\n{}", r.stdout);
    }
}

#[test]
fn quiet_shows_only_failures_and_the_summary() {
    let p = project();
    let r = p.dre("run", &["-q"]);
    let lines = status_lines(&r.stdout);
    assert_eq!(lines.len(), 2, "{}", r.stdout);
    assert!(lines[0].trim_start().starts_with("Failed"));
    assert!(lines[1].trim_start().starts_with("Finished"));
}

#[test]
fn json_log_format_is_one_object_per_line() {
    let p = project();
    let r = p.dre("run", &["--log-format", "json", "-v"]);
    let events: Vec<serde_json::Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(kinds[..3], ["run_parameters", "targets", "plan"]);
    assert_eq!(events[1]["target"]["name"], "dev");
    assert_eq!(events[1]["target"]["from"], "default");
    assert!(kinds.contains(&"binding_vars"));
    assert_eq!(kinds.last(), Some(&"finished"));
    assert!(kinds.contains(&"step") && kinds.contains(&"binding_end"));
    let end = events
        .iter()
        .find(|e| e["event"] == "binding_end" && e["report"] == "daily")
        .unwrap();
    assert_eq!(end["status"], "success");
    assert!(events.iter().all(|e| e["ts"].is_string()));
}

#[test]
fn json_log_format_reports_diagnostics_as_events() {
    let p = project();
    p.write(
        "reports/ops/daily/daily.yml",
        "queries:\n  - {query: setup, tab: false}\n  - summary\noutput: {format: csv, delimeter: \"|\"}\n",
    );
    let r = p.dre("run", &["--log-format", "json"]);
    assert_ne!(r.code, 0);
    let events: Vec<serde_json::Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let d = events
        .iter()
        .find(|e| e["event"] == "diagnostic")
        .expect("a diagnostic event");
    assert_eq!(d["severity"], "error");
    assert_eq!(d["code"], "invalid-output-option");
    assert!(d["message"].as_str().unwrap().contains("`delimeter`"), "{d}");
}

#[test]
fn colour_is_off_unless_asked_for_when_not_a_terminal() {
    let p = project();
    assert!(!p.dre("run", &["daily"]).stdout.contains('\u{1b}'));
    let r = p.dre("run", &["daily", "--color", "always"]);
    assert!(
        r.stdout.contains("\u{1b}[1m\u{1b}[32m")
            || r.stdout.contains("\u{1b}[32m")
            || r.stdout.contains("\u{1b}[1;32m"),
        "{:?}",
        r.stdout
    );
}

#[test]
fn every_run_writes_a_debug_log_with_the_sql_it_ran_in_its_folder() {
    let p = project();
    p.dre("run", &["daily"]).ok();
    // In the run's own folder.
    let log = p.read("target/run/daily/default/dre.log");
    assert!(
        log.contains("DEBUG Setting run_date = 2026-01-25"),
        "the run's settings open its log: {log}"
    );
    assert!(log.contains("DEBUG [daily] Executed"), "{log}");
    assert!(log.contains("INFO  [daily] Succeeded"), "{log}");
    // The full statement, indented under a label naming its file and line.
    assert!(
        log.contains("DEBUG [daily] SQL reports/ops/daily/setup.sql:1:\n    create temp table t as select 1 as n union all select 2\n"),
        "{log}"
    );
    assert!(
        log.contains("SQL reports/ops/daily/summary.sql:1:\n    select * from t\n"),
        "{log}"
    );
}

#[test]
fn each_run_has_its_own_log_and_other_commands_write_none() {
    let p = project();
    p.dre("run", &["daily", "--keep-runs", "2"]).ok();
    std::thread::sleep(std::time::Duration::from_millis(1050));
    p.dre("run", &["daily", "--keep-runs", "2"]).ok();
    let runs: Vec<_> = std::fs::read_dir(p.root().join("target/run/daily/default/runs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 2);
    for r in &runs {
        let log = std::fs::read_to_string(r.join("dre.log")).unwrap();
        // One run's lines only.
        assert_eq!(log.matches("[daily] Started").count(), 1, "{log}");
    }
    p.dre("compile", &[]).ok();
    p.dre("validate", &[]).ok();
    assert!(!p.path("logs").exists(), "no shared project log");
}

#[test]
fn verbose_says_which_profiles_file_it_used() {
    let p = project();
    p.dre("run", &["daily", "-v"])
        .ok()
        .says("  Profiles  ")
        .says("profiles.yml (from --profiles-dir)");
}

fn variable_logging_project(mask_secrets: bool, secret: &str) -> TestProject {
    let config = format!("name: log_redaction\ndefault_profile: warehouse\nmask_secrets: {mask_secrets}\n");
    let schedule_vars = serde_json::json!({
        "scheduled_secret": secret,
        "nested": [{"value": format!("{secret}-long")}],
        "public": "visible-value",
    });
    let schedules = format!("- name: daily\n  report: demo\n  cron: '0 7 * * *'\n  vars: {schedule_vars}\n");
    TestProject::new(
        &[
            ("dre_project.yml", &config),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            ("schedules.yml", &schedules),
            ("reports/demo/demo.yml", "queries: [q]\n"),
            ("reports/demo/q.sql", "select length('{{ var('probe') }}') as n\n"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: ':memory:', threads: 1}\n",
    )
}

#[test]
fn variable_json_is_redacted_before_it_is_embedded_in_text_output() {
    let secret = "audit\"secret\\\n\té";
    let long = format!("{secret}-long");
    for (format, color) in [("text", "never"), ("text", "always"), ("json", "never")] {
        let p = variable_logging_project(true, secret);
        let argument = format!("probe={long}");
        let r = p.dre_env(
            "run",
            &[
                "--schedule",
                "daily",
                "--var",
                &argument,
                "-v",
                "--log-format",
                format,
                "--color",
                color,
            ],
            &[("DRE_SECRET_SHORT", secret), ("DRE_SECRET_LONG", &long)],
        );
        r.ok();
        let log = p.run_logs();
        let record = p.read("target/run/demo/default/run_results.json");
        for (surface, text) in [("stdout", &r.stdout), ("dre.log", &log), ("run record", &record)] {
            for value in [secret, long.as_str()] {
                let encoded = serde_json::to_string(value).unwrap();
                for form in [value, &encoded[1..encoded.len() - 1]] {
                    assert!(!text.contains(form), "{surface} leaks {form:?}:\n{text}");
                }
            }
            assert!(text.contains("visible-value"), "{surface}:\n{text}");
            assert!(text.contains("*****"), "{surface}:\n{text}");
            assert!(
                !text.contains("*****-long"),
                "{surface} only masked a substring:\n{text}"
            );
        }
        // Each JSON fragment in the text log remains parseable after masking.
        for prefix in ["Parameters ", "Schedule daily vars ", "Vars "] {
            let fragment = log
                .lines()
                .find_map(|line| line.split_once(prefix).map(|(_, json)| json))
                .unwrap_or_else(|| panic!("missing {prefix:?}: {log}"));
            let value: serde_json::Value = serde_json::from_str(fragment).unwrap();
            if prefix == "Parameters " {
                assert_eq!(value["vars"]["probe"], "*****");
            } else {
                assert_eq!(value["scheduled_secret"], "*****");
                assert_eq!(value["nested"][0]["value"], "*****");
            }
        }
        if format == "json" {
            let events: Vec<serde_json::Value> = r
                .stdout
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let event = events.iter().find(|e| e["event"] == "binding_vars").unwrap();
            assert_eq!(event["vars"]["probe"], "*****");
            assert_eq!(event["schedule_vars"]["scheduled_secret"], "*****");
        }
        // Only presentation is masked: the database still receives the original variable.
        assert_eq!(
            p.read("target/run/demo/default/demo.csv"),
            format!("n\r\n{}\r\n", long.chars().count())
        );
    }
}

#[test]
fn variable_json_stays_visible_when_secret_masking_is_disabled() {
    let secret = "audit\"secret\\\n\té";
    let long = format!("{secret}-long");
    let encoded = serde_json::to_string(&long).unwrap();
    for format in ["text", "json"] {
        let p = variable_logging_project(false, secret);
        let argument = format!("probe={long}");
        let r = p.dre_env(
            "run",
            &[
                "--schedule",
                "daily",
                "--var",
                &argument,
                "-v",
                "--log-format",
                format,
                "--color",
                "never",
            ],
            &[("DRE_SECRET_SHORT", secret), ("DRE_SECRET_LONG", &long)],
        );
        r.ok();
        for text in [&r.stdout, &p.run_logs()] {
            assert!(text.contains(&encoded[1..encoded.len() - 1]), "{text}");
        }
        let record = p.json("target/run/demo/default/run_results.json");
        assert_eq!(record["params"]["vars"]["probe"], long);
        assert_eq!(record["schedule_vars"]["scheduled_secret"], secret);
    }
}
