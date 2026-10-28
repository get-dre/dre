//! `dre query` must close the plugin child process with the protocol `close` request even when a
//! step fails, and leave no plugin process behind.
//!
//! The fixture source plugin writes its pid to `$DRE_FIXTURE_PID_FILE` when it opens a session. In
//! its `close_probe` mode it writes `$DRE_FIXTURE_CLOSE_FILE` when it receives a `close` request,
//! and nothing when its input only ends.

mod common;

use std::time::{Duration, Instant};

use common::{Run, TestProject, alive, wait_for_pid};

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

/// `dre query` against the fixture, with `env` added for the plugin.
fn run_query(p: &TestProject, env: &[(&str, &str)]) -> Run {
    p.dre_env(
        "query",
        &[
            "--profile",
            "fixture",
            "--sql-file",
            "q.sql",
            "--output-file",
            "out.json",
        ],
        env,
    )
}

/// The step must fail, and report through the plugin's own error.
fn assert_fixture_error(r: &Run) {
    assert_ne!(r.code, 0, "expected the query to fail\n{r:?}");
    assert!(
        r.stderr.contains("fixture can't run") || r.stdout.contains("fixture can't run"),
        "expected the fixture error: {r:?}"
    );
}

/// A query whose `execute` fails (`select 1` isn't a statement the fixture understands) sends
/// the plugin the protocol `close`. A closed pipe alone doesn't count: the fixture records only
/// the request.
#[test]
fn a_failing_step_sends_the_plugin_the_close_request() {
    let p = project("select 1");
    let close_file = p.dir.path().join("plugin.closed");
    let r = run_query(
        &p,
        &[
            ("DRE_FIXTURE_MODE", "close_probe"),
            ("DRE_FIXTURE_CLOSE_FILE", close_file.to_str().unwrap()),
        ],
    );
    assert_fixture_error(&r);
    assert!(
        close_file.exists(),
        "the plugin didn't receive the protocol close request after the query failed"
    );
}

/// The same failing query leaves no plugin process behind. This also holds without the close
/// request (dropping the process closes its input, and the SDK exits on end of input), so it
/// guards the outcome, not the request.
#[test]
fn a_failing_step_reaps_the_plugin_process() {
    let p = project("select 1");
    let pid_file = p.dir.path().join("plugin.pid");
    let r = run_query(&p, &[("DRE_FIXTURE_PID_FILE", pid_file.to_str().unwrap())]);
    assert_fixture_error(&r);
    let plugin = wait_for_pid(&pid_file);
    // Give the process a moment to exit, then it must be gone: no orphan child.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(plugin) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !alive(plugin),
        "the plugin process is still running after the query failed"
    );
}
