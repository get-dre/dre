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
    let p = TestProject::new(
        &[
            ("dependencies.yml", PLUGINS_YML),
            ("reports/demo/demo.yml", "queries: [q]\n"),
            (
                "reports/demo/q.sql",
                "select length('{{ var(\"payload\", \"\") }}') as width\n",
            ),
        ],
        "connections: {warehouse: {targets: {dev: {type: duckdb, path: ':memory:'}}}}\n",
    );
    p.write(
        "dre_project.yml",
        &format!("name: redaction_check\ndefault_profile: warehouse\nmask_secrets: {mask_secrets}\n"),
    );
    p.write("schedules.yml", &serde_json::json!([{
        "name": "daily", "report": "demo", "cron": "0 7 * * *",
        "vars": {"token": secret, "nested": [{"token": format!("{secret}-tail")}], "public": "visible-value"},
    }]).to_string());
    p
}

fn check_variable_surfaces(masking: bool, mode: &str, colour: &str) {
    let credential = "line\\break\"雪\tend\n";
    let extended = format!("{credential}-tail");
    let p = variable_logging_project(masking, credential);
    let assignment = format!("payload={extended}");
    let run = p.dre_env(
        "run",
        &[
            "--schedule",
            "daily",
            "--var",
            &assignment,
            "-v",
            "--log-format",
            mode,
            "--color",
            colour,
        ],
        &[
            ("DRE_SECRET_BASE", credential),
            ("DRE_SECRET_EXTENDED", &extended),
        ],
    );
    run.ok();
    let logs = p.run_logs();
    let metadata = p.json("target/run/demo/default/run_results.json");
    let expected_base = if masking { "*****" } else { credential };
    let expected_extended = if masking { "*****" } else { &extended };
    assert_eq!(metadata["schedule_vars"]["token"], expected_base);
    assert_eq!(metadata["vars"]["payload"], expected_extended);
    assert_eq!(metadata["schedule_vars"]["nested"][0]["token"], expected_extended);
    // The run log's JSON remains usable by tools that extract its fields.
    for marker in ["Parameters ", "Schedule daily vars ", "Vars "] {
        let json = logs
            .lines()
            .find_map(|line| line.split_once(marker).map(|(_, body)| body))
            .unwrap();
        let fields: serde_json::Value = serde_json::from_str(json).unwrap();
        if marker == "Parameters " {
            assert_eq!(fields["vars"]["payload"], expected_extended);
        } else {
            assert_eq!(fields["token"], expected_base);
            assert_eq!(fields["nested"][0]["token"], expected_extended);
            assert_eq!(fields["public"], "visible-value");
        }
    }
    if mode == "json" {
        let events: Vec<serde_json::Value> = run
            .stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let binding = events
            .iter()
            .find(|event| event["event"] == "binding_vars")
            .unwrap();
        assert_eq!(binding["vars"]["payload"], expected_extended);
        assert_eq!(binding["schedule_vars"]["token"], expected_base);
    }
    for output in [&run.stdout, &logs] {
        assert!(output.contains("visible-value"));
        for value in [credential, extended.as_str()] {
            let encoded = serde_json::to_string(value).unwrap();
            let escaped = &encoded[1..encoded.len() - 1];
            if masking {
                assert!(
                    !output.contains(value) && !output.contains(escaped),
                    "credential escaped redaction: {output}"
                );
                assert!(
                    !output.contains("*****-tail"),
                    "overlapping credential was only partly masked: {output}"
                );
            } else {
                assert!(
                    output.contains(escaped),
                    "masking opt-out lost its value: {output}"
                );
            }
        }
    }
    assert_eq!(
        p.read("target/run/demo/default/demo.csv"),
        format!("width\r\n{}\r\n", extended.chars().count())
    );
}

#[test]
fn variable_output_protects_json_escaped_credentials() {
    for (mode, colour) in [("text", "never"), ("text", "always"), ("json", "never")] {
        check_variable_surfaces(true, mode, colour);
    }
}

#[test]
fn variable_output_respects_the_masking_opt_out() {
    for mode in ["text", "json"] {
        check_variable_surfaces(false, mode, "never");
    }
}

#[test]
fn number_filter_diagnostics_mask_before_debug_escaping() {
    let secret = "audit\"secret\\\n\té";
    for mask in [true, false] {
        let p = variable_logging_project(mask, secret);
        p.write(
            "reports/demo/q.sql",
            "select {{ env_var('DRE_SECRET_PROBE') | number }}\n",
        );
        let r = p.dre_env(
            "compile",
            &["demo", "--no-auto-install"],
            &[("DRE_SECRET_PROBE", secret)],
        );
        r.failed();
        let text = format!("{}{}", r.stdout, r.stderr);
        let debug = format!("{secret:?}");
        if mask {
            assert!(
                !text.contains(&debug),
                "diagnostic leaks debug-escaped secret: {text}"
            );
            assert!(text.contains("*****"), "{text}");
        } else {
            assert!(text.contains(&debug), "{text}");
        }
    }
}

#[test]
fn history_json_masks_secrets_in_existing_run_records() {
    let secret = "audit\"secret\\\n\té";
    let p = variable_logging_project(true, secret);
    // A record made with masking off, or before this value was registered as a secret.
    p.write(
        "target/run/demo/default/runs/20261014_010203/run_results.json",
        &serde_json::json!({
            "status": "error", "error": format!("failed: {secret}"),
        })
        .to_string(),
    );
    let r = p.dre_env(
        "history",
        &["demo", "--output", "json"],
        &[("DRE_SECRET_PROBE", secret)],
    );
    r.ok();
    let rows: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(rows[0]["error"], "failed: *****");
    p.write(
        "dre_project.yml",
        "name: log_redaction\ndefault_profile: warehouse\nmask_secrets: false\n",
    );
    let r = p.dre_env(
        "history",
        &["demo", "--output", "json"],
        &[("DRE_SECRET_PROBE", secret)],
    );
    r.ok();
    let rows: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(rows[0]["error"], format!("failed: {secret}"));
}

#[test]
fn message_logs_mask_markdown_escaped_secrets() {
    let secret = "audit*secret_[value]\\end";
    let escaped = dre_protocol::markdown::escape(secret);
    for mask in [true, false] {
        let p = variable_logging_project(mask, secret);
        p.write("reports/demo/q.sql", "select 1 as n\n");
        p.write("reports/demo/demo.yml", "queries: [q]\noutput:\n  format: message\n  text: \"Value: {{ env_var('DRE_SECRET_PROBE') }}\"\n");
        let r = p.dre_env(
            "run",
            &["demo", "--preview", "1", "-v"],
            &[("DRE_SECRET_PROBE", secret)],
        );
        r.ok();
        let record = p.json("target/run/demo/default/run_results.json");
        let message = record["output_results"][0]["message"]["text"].as_str().unwrap();
        for text in [&r.stdout, &p.run_logs(), message] {
            if mask {
                assert!(
                    !text.contains(&escaped),
                    "message output leaks escaped secret: {text}"
                );
                assert!(text.contains("*****"), "{text}");
            } else {
                assert!(text.contains(&escaped), "{text}");
            }
        }
        // Report payloads keep the original value, even while logs are masked.
        assert!(p.read("target/run/demo/default/demo.md").contains(&escaped));
    }
}

#[test]
fn number_filter_diagnostics_do_not_echo_nested_secret_values() {
    let secret = "audit\"secret\\\n\té";
    let p = variable_logging_project(true, secret);
    p.write(
        "reports/demo/q.sql",
        "select {{ [env_var('DRE_SECRET_PROBE')] | number }}\n",
    );
    let r = p.dre_env(
        "compile",
        &["demo", "--no-auto-install"],
        &[("DRE_SECRET_PROBE", secret)],
    );
    r.failed();
    let debug = format!("{secret:?}");
    assert!(
        !r.stdout.contains(&debug),
        "nested diagnostic leaks secret: {}",
        r.stdout
    );
}

#[test]
fn message_summaries_mask_before_truncating_long_secret_values() {
    let secret = format!("audit*secret_{}end", "x".repeat(150));
    let p = variable_logging_project(true, &secret);
    p.write("reports/demo/q.sql", "select 1 as n\n");
    p.write(
        "reports/demo/demo.yml",
        "queries: [q]\noutput:\n  format: message\n  text: \"{{ env_var('DRE_SECRET_PROBE') }}\"\n",
    );
    let r = p.dre_env("run", &["demo"], &[("DRE_SECRET_PROBE", &secret)]);
    r.ok();
    for text in [&r.stdout, &p.run_logs()] {
        assert!(
            !text.contains("audit"),
            "message excerpt leaks secret prefix: {text}"
        );
        assert!(text.contains("*****"), "{text}");
    }
}

#[test]
fn mutable_template_diagnostics_do_not_echo_nested_secret_values() {
    let secret = "audit\"secret\\\n\té";
    for expression in [
        "{% set xs = list() %}{{ xs.remove([env_var('DRE_SECRET_PROBE')]) }}",
        "{% set xs = list() %}{{ xs.index([env_var('DRE_SECRET_PROBE')]) }}",
        "{% set xs = dict() %}{{ xs.pop([env_var('DRE_SECRET_PROBE')]) }}",
    ] {
        let p = variable_logging_project(true, secret);
        p.write("reports/demo/q.sql", expression);
        let r = p.dre_env(
            "compile",
            &["demo", "--no-auto-install"],
            &[("DRE_SECRET_PROBE", secret)],
        );
        r.failed();
        assert!(
            !r.stdout.contains(&format!("{secret:?}")),
            "mutable diagnostic leaks secret: {}",
            r.stdout
        );
    }
}

#[test]
fn message_titles_mask_whitespace_normalized_secrets() {
    let secret = "audit\n\tsecret value";
    let p = variable_logging_project(true, secret);
    p.write("reports/demo/q.sql", "select 1 as n\n");
    p.write("reports/demo/demo.yml", "queries: [q]\noutput:\n  format: message\n  title: \"{{ env_var('DRE_SECRET_PROBE') }}\"\n  text: visible\n");
    let r = p.dre_env("run", &["demo"], &[("DRE_SECRET_PROBE", secret)]);
    r.ok();
    let normalized = secret.split_whitespace().collect::<Vec<_>>().join(" ");
    let record = p.json("target/run/demo/default/run_results.json");
    assert_eq!(record["output_results"][0]["message"]["title"], "*****");
    for text in [&r.stdout, &p.run_logs()] {
        assert!(
            !text.contains(&normalized),
            "title leaks normalized secret: {text}"
        );
    }
    assert!(p.read("target/run/demo/default/demo.md").contains(&normalized));
}
