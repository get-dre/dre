//! `keep_runs`: how many runs of each Binding stay in `target/run/` (`flags: keep_runs`,
//! `DRE_KEEP_RUNS`, `--keep-runs`), and `dre clean --prune`.

mod common;

use std::time::Duration;

use common::TestProject;

const PROFILES: &str = "\
connections:
  fixture:
    targets:
      dev: {type: fixture}
";

fn project(flags: &str) -> TestProject {
    TestProject::new(
        &[
            (
                "dre_project.yml",
                &format!("name: acme_reports\ndefault_profile: fixture\n{flags}"),
            ),
            ("dependencies.yml", "plugins:\n  - fixture\n  - csv\n"),
            (
                "reports/daily/daily.yml",
                "queries: [rows]\noutput: {format: csv}\n",
            ),
            ("reports/daily/rows.sql", "rows 2"),
        ],
        PROFILES,
    )
}

fn runs(p: &TestProject) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p.root().join("target/run/daily/default/runs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

fn run_times(p: &TestProject, n: usize, args: &[&str], env: &[(&str, &str)]) {
    for _ in 0..n {
        p.dre_env("run", args, env).ok();
        // Run ids carry the start second.
        std::thread::sleep(Duration::from_millis(1050));
    }
}

#[test]
fn flags_keep_runs_keeps_that_many_and_the_newest_is_current() {
    let p = project("flags:\n  keep_runs: 3\n");
    run_times(&p, 4, &["daily"], &[]);
    let kept = runs(&p);
    assert_eq!(kept.len(), 3, "{kept:?}");
    let current = std::fs::read_to_string(p.root().join("target/run/daily/default/current")).unwrap();
    assert_eq!(current.trim(), kept[2]);
    let history = p.dre("history", &["daily"]);
    assert_eq!(
        history.stdout.lines().filter(|l| l.contains("success")).count(),
        3,
        "{}",
        history.stdout
    );
}

#[test]
fn the_flag_and_the_environment_win_over_the_project() {
    let p = project("flags:\n  keep_runs: 5\n");
    run_times(&p, 3, &["daily", "--keep-runs", "2"], &[]);
    assert_eq!(runs(&p).len(), 2);
    run_times(&p, 1, &["daily"], &[("DRE_KEEP_RUNS", "1")]);
    assert_eq!(runs(&p).len(), 1);
    assert_eq!(p.dre("run", &["daily", "--keep-runs", "0"]).code, 2);
}

#[test]
fn clean_prune_removes_old_runs_and_keeps_the_rest() {
    let p = project("flags:\n  keep_runs: 4\n");
    run_times(&p, 3, &["daily"], &[]);
    assert_eq!(runs(&p).len(), 3);
    let out = p.dre("clean", &["--prune", "--keep-runs", "1"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stderr.contains("Removed 2 run(s)"), "{out:?}");
    assert_eq!(runs(&p).len(), 1);
    assert!(
        p.root().join("target/manifest.json").is_file(),
        "--prune leaves the rest of the target path"
    );
}

#[test]
fn a_bad_keep_runs_is_a_config_error() {
    let p = project("flags:\n  keep_runs: 0\n");
    let out = p.dre("validate", &[]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout
            .contains("`flags.keep_runs` must be a whole number of 1 or more"),
        "{out:?}"
    );
}
