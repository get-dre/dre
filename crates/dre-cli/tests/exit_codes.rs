//! The documented exit codes (docs/exit-codes.md): 0 success, 1 the command ran and something
//! failed, 2 it couldn't start. 124, 130 and 143 are covered by tests/cancel.rs.

mod common;

use common::TestProject;

const PROFILES: &str = "\
connections:
  fixture:
    targets:
      dev: {type: fixture}
";

fn project(extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: fixture\n",
        ),
        ("dependencies.yml", "plugins:\n  - fixture\n  - csv\n"),
        (
            "reports/ok/ok.yml",
            "queries: [two_rows]\noutput: {format: csv}\n",
        ),
        ("reports/ok/two_rows.sql", "rows 2"),
    ];
    files.extend_from_slice(extra);
    TestProject::new(&files, PROFILES)
}

#[test]
fn a_successful_run_exits_0() {
    let p = project(&[]);
    assert_eq!(p.dre("run", &[]).code, 0);
}

#[test]
fn a_failed_report_exits_1() {
    let p = project(&[
        (
            "reports/broken/broken.yml",
            "queries: [bad]\noutput: {format: csv}\n",
        ),
        ("reports/broken/bad.sql", "fail"),
    ]);
    let out = p.dre("run", &[]);
    assert_eq!(out.code, 1, "{out:?}");
}

#[test]
fn an_invalid_project_or_selection_exits_2() {
    // A config error stops the run from starting (before 0.4 this exited 1).
    let p = project(&[(
        "reports/ok/ok.yml",
        "queries: [two_rows]\noutput: {format: csv}\nnot_a_key: 1\n",
    )]);
    let out = p.dre("run", &[]);
    assert_eq!(out.code, 2, "{out:?}");
    // A selection that matches nothing.
    let p = project(&[]);
    assert_eq!(p.dre("run", &["no_such_report"]).code, 2);
    // A bad flag value.
    assert_eq!(p.dre("run", &["--timeout", "never"]).code, 2);
    assert_eq!(p.dre("ls", &["no_such_report"]).code, 2);
}

#[test]
fn validate_exits_1_for_problems_and_2_without_a_project() {
    let p = project(&[(
        "reports/ok/ok.yml",
        "queries: [two_rows]\noutput: {format: csv}\nnot_a_key: 1\n",
    )]);
    assert_eq!(p.dre("validate", &[]).code, 1);
    let empty = tempfile::tempdir().unwrap();
    let out = assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .args(["validate", "--project-dir"])
        .arg(empty.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn validate_strict_turns_warnings_into_failures() {
    // An unmanaged report: a warning.
    let p = project(&[("reports/scratch/quick_look.sql", "select 1")]);
    let out = p.dre("validate", &[]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains("unmanaged-report") || out.stderr.contains("unmanaged-report"),
        "{out:?}"
    );
    assert_eq!(p.dre("validate", &["--strict"]).code, 1);
}

#[test]
fn explain_with_an_unknown_code_is_a_usage_error() {
    let p = project(&[]);
    let out = assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .args(["explain", "nonsense-code"])
        .current_dir(p.root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
