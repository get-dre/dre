//! Run folders (`target/run/<report>/<binding>/runs/<run id>/`), the `current` pointer, the
//! duplicate-run guard, `dre history` and `dre unlock`.

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::TestProject;

const PROFILES: &str = "\
connections:
  fixture:
    targets:
      dev: {type: fixture}
";

fn project() -> TestProject {
    TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: fixture\n",
            ),
            ("dependencies.yml", "plugins:\n  - fixture\n  - csv\n"),
            (
                "reports/daily/daily.yml",
                "queries: [rows]\noutput: {format: csv}\n",
            ),
            ("reports/daily/rows.sql", "rows 2"),
            (
                "reports/slow/slow.yml",
                "queries:\n  - {query: wait, tab: false}\n  - more\noutput: {format: csv}\n",
            ),
            ("reports/slow/wait.sql", "sleep 20"),
            ("reports/slow/more.sql", "rows 1"),
        ],
        PROFILES,
    )
}

fn binding(p: &TestProject, report: &str) -> std::path::PathBuf {
    p.root().join("target/run").join(report).join("default")
}

fn runs(p: &TestProject, report: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(binding(p, report).join("runs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn each_run_gets_its_own_folder_and_current_points_at_the_latest() {
    let p = project();
    p.dre("run", &["daily"]).ok();
    let first = runs(&p, "daily");
    assert_eq!(first.len(), 1);
    let current = std::fs::read_to_string(binding(&p, "daily").join("current")).unwrap();
    assert_eq!(current.trim(), first[0]);
    let results = p.json("target/run/daily/default/run_results.json");
    assert_eq!(results["run_id"], first[0]);
    assert!(
        results["outputs"][0]["path"]
            .as_str()
            .unwrap()
            .starts_with(&format!("target/run/daily/default/runs/{}/", first[0]))
    );
    std::thread::sleep(Duration::from_millis(1100));
    p.dre("run", &["daily"]).ok();
    // The default keeps one run: the new one, now current.
    let second = runs(&p, "daily");
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);
    let current = std::fs::read_to_string(binding(&p, "daily").join("current")).unwrap();
    assert_eq!(current.trim(), second[0]);
    // No lock is left behind.
    assert!(!binding(&p, "daily").join("lock").exists());
}

#[test]
fn a_second_run_of_the_same_binding_is_refused_and_changes_nothing() {
    let p = project();
    let mut first = Command::new(env!("CARGO_BIN_EXE_dre"))
        .args(["run", "slow", "--project-dir"])
        .arg(p.root())
        .arg("--profiles-dir")
        .arg(p.dir.path().join("profiles"))
        .env("DRE_PLUGINS_DIR", &p.plugins)
        .env("HOME", p.dir.path().join("home"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = binding(&p, "slow").join("lock");
    for _ in 0..200 {
        if lock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(lock.exists(), "the first run never took the lock");
    let second = p.dre("run", &["slow"]);
    assert_eq!(second.code, 2, "{second:?}");
    let text = second.stdout.clone() + &second.stderr;
    assert!(
        text.contains("run-in-progress") || text.contains("is already running"),
        "{text}"
    );
    assert!(text.contains("dre unlock slow"), "{text}");
    // `dre unlock` without --yes and without a terminal refuses.
    assert_eq!(p.dre("unlock", &["slow"]).code, 2);
    first.kill().unwrap();
    first.wait().unwrap();
    // The killed run left its lock: this host, a dead process, so the next run takes it over.
    let third = p.dre("run", &["daily"]);
    assert_eq!(third.code, 0, "{third:?}");
}

#[test]
fn unlock_removes_a_lock_left_on_another_host() {
    let p = project();
    p.dre("run", &["daily"]).ok();
    std::fs::write(
        binding(&p, "daily").join("lock"),
        r#"{"run_id": "20260101T000000Z-abcd", "host": "elsewhere", "pid": 1, "started_at": "2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let refused = p.dre("run", &["daily"]);
    assert_eq!(refused.code, 2, "{refused:?}");
    assert!(
        (refused.stdout.clone() + &refused.stderr).contains("on elsewhere"),
        "{refused:?}"
    );
    let out = p.dre("unlock", &["daily", "--yes"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stderr.contains("locked by run 20260101T000000Z-abcd"),
        "{out:?}"
    );
    p.dre("run", &["daily"]).ok();
}

#[test]
fn history_lists_runs_and_finds_the_latest_files() {
    let p = project();
    p.dre("run", &["daily"]).ok();
    let id = runs(&p, "daily").remove(0);
    let out = p.dre("history", &["daily"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains(&format!("* default    {id}")),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("success"), "{}", out.stdout);
    let path = p.dre("history", &["daily", "--latest", "--path"]);
    assert_eq!(path.stdout.trim(), format!("target/run/daily/default/runs/{id}"));
    let json = p.dre("history", &["daily", "--output", "json"]);
    let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(v[0]["run_id"], id.as_str());
    assert_eq!(v[0]["current"], true);
    assert_eq!(p.dre("history", &["nothing_here"]).code, 1);
}

#[test]
fn files_from_before_run_folders_move_into_a_legacy_run() {
    let p = project();
    let b = binding(&p, "daily");
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("daily.csv"), "old\n").unwrap();
    std::fs::write(
        b.join("run_results.json"),
        r#"{"status": "success", "started_at": "2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    p.dre("run", &["daily"]).ok();
    assert!(!b.join("daily.csv").exists());
    // The legacy run was current before this one; the new run replaced it (keep one).
    let all = runs(&p, "daily");
    assert_eq!(all.len(), 1, "{all:?}");
    assert!(!all[0].ends_with("-legacy"));
}

#[test]
fn a_rerun_for_an_earlier_instant_doesnt_replace_a_later_one() {
    let p = project();
    p.dre_env("run", &["daily"], &[("DRE_RUN_AT", "2026-03-02T06:00:00Z")])
        .ok();
    let later = runs(&p, "daily").remove(0);
    let out = p.dre_env("run", &["daily"], &[("DRE_RUN_AT", "2026-03-01T06:00:00Z")]);
    out.ok();
    assert!(
        (out.stdout.clone() + &out.stderr).contains("a run for a later instant is current"),
        "{out:?}"
    );
    let current = std::fs::read_to_string(binding(&p, "daily").join("current")).unwrap();
    assert_eq!(current.trim(), later);
}
