//! A Binding's run folders, so overlapping and interrupted runs are safe:
//!
//! ```text
//! target/run/<report>/<binding>/
//!   current        the run id of the latest finished run (written atomically)
//!   lock           while a run holds the Binding: its run id, host, process and start time
//!   runs/<run id>/ one folder per run: its outputs and run_results.json
//! ```
//!
//! Each run writes only into its own folder and delivers from there. At the end `current` is
//! switched to it, unless a run for a later instant is already current (an older run never
//! replaces a newer one). The same Binding can't run twice at once: the second run finds the
//! `lock` and stops without touching anything. A crash leaves the previous run current.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const CURRENT: &str = "current";
pub const LOCK: &str = "lock";
pub const RUNS: &str = "runs";
/// The OS lock held while a Binding lock is checked and taken.
const CHECKING: &str = ".lock-checking.tmp";

/// The run ids whose Binding lock this process holds.
fn held() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    static HELD: std::sync::Mutex<std::collections::BTreeSet<String>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Make `path` a link to `tmp`, failing with `AlreadyExists` if it exists. Where the file system
/// has no hard links, create it and write `body` instead.
fn create_lock(tmp: &Path, path: &Path, body: &str) -> std::io::Result<()> {
    match std::fs::hard_link(tmp, path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()
        }
        r => r,
    }
}

/// A run id: the UTC start time and four random characters, `20261009T060000Z-k3f9`. Ids sort by
/// start time.
pub fn new_run_id(started: DateTime<Utc>) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut n = nanos ^ std::process::id().wrapping_mul(2_654_435_761);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let suffix: String = (0..4)
        .map(|_| {
            let c = ALPHABET[(n % 32) as usize] as char;
            n /= 32;
            c
        })
        .collect();
    format!("{}-{suffix}", started.format("%Y%m%dT%H%M%SZ"))
}

/// Who holds a Binding's lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub run_id: String,
    pub host: String,
    pub pid: u32,
    /// RFC 3339.
    pub started_at: String,
    /// When the process started, as the OS counts it (see [`process_start`]): a live process
    /// with the same PID but another start time is a different process that reused the PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<u64>,
}

impl Holder {
    fn me(run_id: &str, started: DateTime<Utc>, host: &str) -> Holder {
        Holder {
            run_id: run_id.to_string(),
            host: host.to_string(),
            pid: std::process::id(),
            started_at: started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            process_start: process_start(std::process::id()),
        }
    }

    /// `run 20261009T060000Z-k3f9, started 2026-10-09T06:00:00Z on build-7 (process 4312)`.
    pub fn describe(&self) -> String {
        format!(
            "run {}, started {} on {} (process {})",
            self.run_id, self.started_at, self.host, self.pid
        )
    }
}

/// Why a Binding's lock couldn't be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another run holds it (on this host and alive, or on another host).
    Held(Holder),
    /// The lock file is there but unreadable (a crash mid-write, a hand edit).
    Unreadable(PathBuf),
    Io(std::io::Error),
}

/// Holds a Binding's lock; dropping it releases the lock.
#[derive(Debug)]
#[must_use = "dropping the guard releases the lock at once"]
pub struct Guard {
    path: PathBuf,
    run_id: String,
    /// A stale lock (its process gone) was taken over.
    pub took_over: Option<Holder>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Only our own lock: never one another run took over meanwhile.
        if let Ok(text) = std::fs::read_to_string(&self.path)
            && serde_json::from_str::<Holder>(&text).is_ok_and(|h| h.run_id == self.run_id)
        {
            let _ = std::fs::remove_file(&self.path);
        }
        held().remove(&self.run_id);
    }
}

/// One Binding's folder in `target/run/`.
#[derive(Debug, Clone)]
pub struct BindingRuns {
    dir: PathBuf,
}

impl BindingRuns {
    pub fn new(dir: &Path) -> BindingRuns {
        BindingRuns {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A run's folder.
    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.dir.join(RUNS).join(run_id)
    }

    /// The current run's id.
    pub fn current(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.dir.join(CURRENT)).ok()?;
        let id = id.trim();
        (!id.is_empty() && self.run_dir(id).is_dir()).then(|| id.to_string())
    }

    /// The current run's folder.
    pub fn current_dir(&self) -> Option<PathBuf> {
        self.current().map(|id| self.run_dir(&id))
    }

    /// Every run's id, oldest first.
    pub fn list(&self) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(self.dir.join(RUNS))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// The lock's holder, if any.
    pub fn holder(&self) -> Option<Holder> {
        let text = std::fs::read_to_string(self.dir.join(LOCK)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Take the Binding's lock for `run_id`. A lock left by a process on this host that has
    /// gone is taken over (the guard says so), as is one naming this process that this process
    /// doesn't hold (a restarted container, where DRE gets the same process id again); one held
    /// by a live process, or by any process on another host (a shared folder), is
    /// [`LockError::Held`]: DRE never guesses about a run it can't see. `host` is this machine's
    /// name ([`host_name`]).
    pub fn lock(&self, run_id: &str, started: DateTime<Utc>, host: &str) -> Result<Guard, LockError> {
        std::fs::create_dir_all(&self.dir).map_err(LockError::Io)?;
        let path = self.dir.join(LOCK);
        let me = Holder::me(run_id, started, host);
        let body = serde_json::to_string_pretty(&me).map_err(|e| LockError::Io(e.into()))? + "\n";
        // The content is written first and the lock appears whole (a hard link, which fails if a
        // lock exists): a run checking it never reads a half-written one.
        let mut tmp = tempfile::Builder::new()
            .prefix(".lock-")
            .suffix(".tmp")
            .tempfile_in(&self.dir)
            .map_err(LockError::Io)?;
        {
            use std::io::Write;
            tmp.write_all(body.as_bytes()).map_err(LockError::Io)?;
            tmp.as_file().sync_all().map_err(LockError::Io)?;
        }
        // Closed before linking (Windows), removed when this returns.
        let tmp = tmp.into_temp_path();
        // Checking and taking over a stale lock happen under an OS lock, so two runs can't both
        // take over the same stale lock.
        let _checking = crate::fs::lock_file(&self.dir.join(CHECKING))
            .map_err(|e| LockError::Io(std::io::Error::other(e)))?;
        let mut took_over = None;
        for _ in 0..2 {
            match create_lock(&tmp, &path, &body) {
                Ok(()) => {
                    held().insert(run_id.to_string());
                    return Ok(Guard {
                        path,
                        run_id: run_id.to_string(),
                        took_over,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = std::fs::read_to_string(&path).map_err(LockError::Io)?;
                    let Ok(holder) = serde_json::from_str::<Holder>(&text) else {
                        return Err(LockError::Unreadable(path));
                    };
                    let mine = holder.pid == std::process::id();
                    if holder.host != me.host
                        || (same_process_alive(&holder) && (!mine || held().contains(&holder.run_id)))
                    {
                        return Err(LockError::Held(holder));
                    }
                    // Its process is gone: the run crashed or was killed.
                    let _ = std::fs::remove_file(&path);
                    took_over = Some(holder);
                }
                Err(e) => return Err(LockError::Io(e)),
            }
        }
        Err(LockError::Io(std::io::Error::other(
            "another run took the lock at the same moment",
        )))
    }

    /// Remove the lock whatever holds it (`dre unlock`). Returns the holder it removed.
    pub fn unlock(&self) -> std::io::Result<Option<Holder>> {
        let holder = self.holder();
        match std::fs::remove_file(self.dir.join(LOCK)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(holder),
        }
    }

    /// Point `current` at `run_id`.
    pub fn switch(&self, run_id: &str) -> std::io::Result<()> {
        crate::fs::write_atomic(&self.dir.join(CURRENT), format!("{run_id}\n").as_bytes())
    }

    /// Remove all but the newest `keep` runs (of any status, finished or not), never the current
    /// run. Call it holding the lock.
    pub fn prune(&self, keep: usize) -> Vec<String> {
        let current = self.current();
        let ids = self.list();
        let cut = ids.len().saturating_sub(keep);
        let mut removed = Vec::new();
        for id in ids.iter().take(cut) {
            if Some(id) == current.as_ref() {
                continue;
            }
            if std::fs::remove_dir_all(self.run_dir(id)).is_ok() {
                removed.push(id.clone());
            }
        }
        removed
    }
}

/// This machine's name, for the lock: `DRE_HOST_NAME` if set (containers that share a target
/// folder and reuse names), else the OS's.
pub fn host_name(env: &crate::Env) -> String {
    if let Some(v) = env.var("DRE_HOST_NAME")
        && !v.trim().is_empty()
    {
        return v.trim().to_string();
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most `buf.len()` bytes into the buffer.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).to_string();
        }
    }
    #[cfg(windows)]
    if let Some(v) = env.var("COMPUTERNAME") {
        return v.to_string();
    }
    "unknown".to_string()
}

/// Whether process `pid` on this machine is running.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is checked and closed; the exit code is written into a local.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Whether the holder's process is still running: its PID is alive and, where the OS tells start
/// times, started when the lock says (a restarted container can give its PID to another process).
fn same_process_alive(holder: &Holder) -> bool {
    process_alive(holder.pid)
        && match (holder.process_start, process_start(holder.pid)) {
            (Some(was), Some(now)) => was == now,
            _ => true,
        }
}

/// When process `pid` started, in clock ticks since boot (Linux `/proc/<pid>/stat` field 22);
/// `None` elsewhere or when unknown.
pub fn process_start(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 2, the command name, may hold spaces: count from its closing parenthesis.
        stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs() -> (tempfile::TempDir, BindingRuns) {
        let d = tempfile::tempdir().unwrap();
        let r = BindingRuns::new(&d.path().join("run/daily/default"));
        (d, r)
    }

    #[test]
    fn run_ids_sort_by_start_time() {
        let a = new_run_id("2026-10-09T06:00:00Z".parse().unwrap());
        let b = new_run_id("2026-10-09T06:05:00Z".parse().unwrap());
        assert!(a.starts_with("20261009T060000Z-") && a.len() == 21, "{a}");
        assert!(a < b);
    }

    #[test]
    fn a_second_run_is_refused_while_the_first_holds_the_lock() {
        let (_d, r) = runs();
        let now = Utc::now();
        let g = r.lock("a", now, &host_name(&crate::Env::default())).unwrap();
        match r.lock("b", now, &host_name(&crate::Env::default())) {
            Err(LockError::Held(h)) => {
                assert_eq!(h.run_id, "a");
                assert_eq!(h.pid, std::process::id());
            }
            other => panic!("{other:?}"),
        }
        drop(g);
        assert!(r.holder().is_none());
        drop(r.lock("b", now, &host_name(&crate::Env::default())).unwrap());
    }

    #[test]
    fn a_dead_processs_lock_is_taken_over_but_another_hosts_never() {
        let (_d, r) = runs();
        std::fs::create_dir_all(r.dir()).unwrap();
        let dead = Holder {
            run_id: "old".into(),
            host: host_name(&crate::Env::default()),
            pid: 999_999,
            started_at: "2026-10-09T06:00:00Z".into(),
            process_start: None,
        };
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&dead).unwrap()).unwrap();
        let g = r
            .lock("new", Utc::now(), &host_name(&crate::Env::default()))
            .unwrap();
        assert_eq!(g.took_over.as_ref().map(|h| h.run_id.as_str()), Some("old"));
        drop(g);
        let elsewhere = Holder {
            host: "some-other-host".into(),
            ..dead
        };
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&elsewhere).unwrap()).unwrap();
        assert!(matches!(
            r.lock("new", Utc::now(), &host_name(&crate::Env::default())),
            Err(LockError::Held(_))
        ));
        assert_eq!(r.unlock().unwrap().unwrap().host, "some-other-host");
        drop(
            r.lock("new", Utc::now(), &host_name(&crate::Env::default()))
                .unwrap(),
        );
    }

    #[test]
    fn a_lock_naming_this_process_that_it_doesnt_hold_is_taken_over() {
        // A restarted container: same host name, and DRE is PID 1 again.
        let (_d, r) = runs();
        std::fs::create_dir_all(r.dir()).unwrap();
        let before_restart = Holder {
            run_id: "before-restart".into(),
            host: host_name(&crate::Env::default()),
            pid: std::process::id(),
            started_at: "2026-10-09T06:00:00Z".into(),
            process_start: None,
        };
        std::fs::write(
            r.dir().join(LOCK),
            serde_json::to_string(&before_restart).unwrap(),
        )
        .unwrap();
        let g = r
            .lock("new", Utc::now(), &host_name(&crate::Env::default()))
            .unwrap();
        assert_eq!(g.took_over, Some(before_restart));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_live_process_that_reused_the_holders_pid_doesnt_keep_the_lock() {
        let (_d, r) = runs();
        std::fs::create_dir_all(r.dir()).unwrap();
        let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let start = process_start(other.id()).expect("a Linux process has a start time");
        let mut holder = Holder {
            run_id: "other".into(),
            host: host_name(&crate::Env::default()),
            pid: other.id(),
            started_at: "2026-10-09T06:00:00Z".into(),
            process_start: Some(start),
        };
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&holder).unwrap()).unwrap();
        let alive = r.lock("new", Utc::now(), &host_name(&crate::Env::default()));
        assert!(matches!(alive, Err(LockError::Held(_))), "{alive:?}");
        // Same PID, another start time: the run that wrote the lock is gone.
        holder.process_start = Some(start + 1);
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&holder).unwrap()).unwrap();
        let g = r.lock("new", Utc::now(), &host_name(&crate::Env::default()));
        let _ = other.kill();
        let _ = other.wait();
        assert_eq!(g.unwrap().took_over, Some(holder));
    }

    #[test]
    fn runs_racing_for_a_lock_get_exactly_one_holder() {
        let dead = Holder {
            run_id: "old".into(),
            host: host_name(&crate::Env::default()),
            pid: 999_999,
            started_at: "2026-10-09T06:00:00Z".into(),
            process_start: None,
        };
        for stale in [None, Some(dead)] {
            let (_d, r) = runs();
            if let Some(h) = &stale {
                std::fs::create_dir_all(r.dir()).unwrap();
                std::fs::write(r.dir().join(LOCK), serde_json::to_string(h).unwrap()).unwrap();
            }
            let start = std::sync::Arc::new(std::sync::Barrier::new(8));
            let racers: Vec<_> = (0..8)
                .map(|i| {
                    let (r, start) = (r.clone(), start.clone());
                    std::thread::spawn(move || {
                        start.wait();
                        r.lock(
                            &format!("run-{i}"),
                            Utc::now(),
                            &host_name(&crate::Env::default()),
                        )
                    })
                })
                .collect();
            let results: Vec<_> = racers.into_iter().map(|t| t.join().unwrap()).collect();
            let holders = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(holders, 1, "stale {stale:?}: {results:?}");
            assert!(
                results
                    .iter()
                    .all(|r| matches!(r, Ok(_) | Err(LockError::Held(_)))),
                "{results:?}"
            );
        }
    }

    #[test]
    fn current_switches_and_pruning_keeps_it() {
        let (_d, r) = runs();
        for id in [
            "20261001T000000Z-aaaa",
            "20261002T000000Z-bbbb",
            "20261003T000000Z-cccc",
        ] {
            std::fs::create_dir_all(r.run_dir(id)).unwrap();
        }
        assert_eq!(r.current(), None);
        r.switch("20261001T000000Z-aaaa").unwrap();
        assert_eq!(r.current().as_deref(), Some("20261001T000000Z-aaaa"));
        let removed = r.prune(1);
        assert_eq!(removed, ["20261002T000000Z-bbbb"]);
        assert_eq!(r.list(), ["20261001T000000Z-aaaa", "20261003T000000Z-cccc"]);
    }

    #[test]
    fn this_process_is_alive() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(999_999));
    }
}
