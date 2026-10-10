//! Clean cancellation of `dre run`: a termination signal (SIGTERM; Ctrl-Break on Windows) or
//! Ctrl-C during a long query stops the plugin, records the Binding as cancelled, runs nothing
//! more, delivers nothing, and exits 143 or 130.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::TestProject;

const PROFILES: &str = "\
connections:
  fixture:
    targets:
      dev: {type: fixture}
destinations:
  inbox:
    targets:
      dev: {type: local}
";

/// Two reports: `a_slow` runs `slow_sql` then a tab and delivers it; `b_next` would run after.
fn project(slow_sql: &str) -> TestProject {
    TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: fixture\n",
            ),
            ("dependencies.yml", "plugins:\n  - fixture\n  - csv\n"),
            (
                "reports/a_slow/a_slow.yml",
                "queries:\n  - {query: slow, tab: false}\n  - rows\noutput:\n  format: csv\n  destination: {profile: inbox, path: out/a.csv}\n",
            ),
            ("reports/a_slow/slow.sql", slow_sql),
            ("reports/a_slow/rows.sql", "rows 2"),
            (
                "reports/b_next/b_next.yml",
                "queries: [more_rows]\noutput: {format: csv}\n",
            ),
            ("reports/b_next/more_rows.sql", "rows 2"),
        ],
        PROFILES,
    )
}

fn spawn(p: &TestProject, pid_file: &Path) -> Child {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dre"));
    c.args(["run", "--project-dir"])
        .arg(p.root())
        .arg("--profiles-dir")
        .arg(p.dir.path().join("profiles"))
        .env("DRE_PLUGINS_DIR", &p.plugins)
        .env("DRE_FIXTURE_PID_FILE", pid_file)
        .env("HOME", p.dir.path().join("home"))
        .env_remove("DRE_PROFILES_DIR")
        .env_remove("DRE_TARGET")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Its own process group, so Ctrl-Break can be sent to it alone.
        c.creation_flags(0x0000_0200);
    }
    c.spawn().unwrap()
}

/// Wait until the fixture plugin has opened its session (it writes its pid), then return it.
fn plugin_pid(child: &mut Child, pid_file: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let mut out = String::new();
            use std::io::Read;
            let _ = child.stdout.take().unwrap().read_to_string(&mut out);
            let _ = child.stderr.take().unwrap().read_to_string(&mut out);
            panic!("dre ended ({status}) before the plugin started:\n{out}");
        }
        if let Ok(s) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = s.trim().parse()
        {
            // Give it a moment to get into the slow statement.
            std::thread::sleep(Duration::from_millis(500));
            return pid;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the plugin never started");
}

#[cfg(unix)]
fn signal(pid: u32, sig: &str) {
    assert!(
        Command::new("kill")
            .args([sig, &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
fn terminate(child: &Child) {
    signal(child.id(), "-TERM");
}

#[cfg(windows)]
fn terminate(child: &Child) {
    unsafe extern "system" {
        fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    }
    // SAFETY: sends Ctrl-Break to the child's own process group.
    assert_ne!(unsafe { GenerateConsoleCtrlEvent(1, child.id()) }, 0);
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

fn wait(child: Child, limit: Duration) -> (i32, String) {
    let started = Instant::now();
    let out = child.wait_with_output().unwrap();
    assert!(
        started.elapsed() < limit,
        "dre took {:?} to stop",
        started.elapsed()
    );
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn assert_cancelled(p: &TestProject, plugin: u32) {
    let results = p.json("target/run/a_slow/default/run_results.json");
    assert_eq!(results["status"], "cancelled", "{results}");
    assert_eq!(results["error_code"], "run-cancelled", "{results}");
    assert_eq!(results["error_kind"], "cancelled", "{results}");
    assert!(!p.path("out/a.csv").exists(), "a cancelled run delivered");
    assert!(
        !p.path("target/run/b_next").exists(),
        "a Binding started after the cancel"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(plugin) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(plugin), "the plugin is still running");
}

#[test]
fn a_termination_signal_cancels_the_running_query() {
    let p = project("sleep 60");
    let pid_file = p.dir.path().join("plugin.pid");
    let mut child = spawn(&p, &pid_file);
    let plugin = plugin_pid(&mut child, &pid_file);
    terminate(&child);
    // The plugin stops at once on `cancel`: well inside the 8-second grace.
    let (code, out) = wait(child, Duration::from_secs(6));
    assert_eq!(code, 143, "{out}");
    assert!(
        out.contains("cancelled by a termination signal; 1 Binding didn't run"),
        "{out}"
    );
    assert_cancelled(&p, plugin);
}

#[test]
fn a_plugin_that_ignores_cancel_is_killed_after_the_grace_period() {
    let p = project("stubborn 60");
    let pid_file = p.dir.path().join("plugin.pid");
    let mut child = spawn(&p, &pid_file);
    let plugin = plugin_pid(&mut child, &pid_file);
    terminate(&child);
    let (code, out) = wait(child, Duration::from_secs(12));
    assert_eq!(code, 143, "{out}");
    assert_cancelled(&p, plugin);
}

#[cfg(unix)]
#[test]
fn ctrl_c_exits_130_and_a_second_one_stops_at_once() {
    let p = project("sleep 60");
    let pid_file = p.dir.path().join("plugin.pid");
    let mut child = spawn(&p, &pid_file);
    let plugin = plugin_pid(&mut child, &pid_file);
    signal(child.id(), "-INT");
    let (code, out) = wait(child, Duration::from_secs(6));
    assert_eq!(code, 130, "{out}");
    assert_cancelled(&p, plugin);

    let p = project("stubborn 60");
    let pid_file = p.dir.path().join("plugin.pid");
    let mut child = spawn(&p, &pid_file);
    let plugin = plugin_pid(&mut child, &pid_file);
    signal(child.id(), "-INT");
    std::thread::sleep(Duration::from_millis(300));
    signal(child.id(), "-INT");
    let (code, _) = wait(child, Duration::from_secs(3));
    assert_eq!(code, 130);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(plugin) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(plugin), "the plugin is still running");
}
