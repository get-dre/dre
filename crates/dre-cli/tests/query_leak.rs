//! `dre query` must close (and reap) the plugin child process even when a step fails.
//!
//! The fixture source plugin writes its pid to `$DRE_FIXTURE_PID_FILE` when it opens a session,
//! so a failing `dre query` can be checked for a leftover (orphan) plugin process afterwards.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::TestProject;

const PROFILES: &str = "\
connections:
  fixture:
    targets:
      dev: {type: fixture}
";

fn project(sql: &str) -> TestProject {
    TestProject::new(
        &[
            ("dre_project.yml", "name: query\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins:\n  - fixture\n"),
            ("q.sql", sql),
        ],
        PROFILES,
    )
}

/// `dre query` against the fixture, capturing the plugin's pid via `DRE_FIXTURE_PID_FILE`.
fn run_query(p: &TestProject, pid_file: &Path) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dre"));
    c.args(["query", "--project-dir"])
        .arg(p.root())
        .arg("--profiles-dir")
        .arg(p.dir.path().join("profiles"))
        .arg("--profile")
        .arg("fixture")
        .arg("--sql-file")
        .arg("q.sql")
        .arg("--output-file")
        .arg("out.json")
        .env("DRE_PLUGINS_DIR", &p.plugins)
        .env("DRE_FIXTURE_PID_FILE", pid_file)
        .env("HOME", p.dir.path().join("home"))
        .env_remove("DRE_PROFILES_DIR")
        .env_remove("DRE_TARGET")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
}

/// The fixture's pid once it has opened its session, or panic if it never opened.
fn wait_for_pid(pid_file: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the fixture plugin never opened its session")
}

/// A query whose `execute` fails (`select 1` isn't a statement the fixture understands).
#[test]
fn a_failing_step_reaps_the_plugin_process() {
    let p = project("select 1");
    let pid_file = p.dir.path().join("plugin.pid");
    let (code, out) = run_query(&p, &pid_file);
    // The step must fail, and report through the plugin's own error.
    assert_ne!(code, 0, "expected the query to fail\n{out}");
    assert!(
        out.contains("fixture can't run"),
        "expected the fixture error: {out}"
    );
    let plugin = wait_for_pid(&pid_file);
    // Give the (fixed) close a moment, then the plugin must be gone: no orphan child.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(plugin) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !alive(plugin),
        "the plugin process is still running after the query failed"
    );
}
