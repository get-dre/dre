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
}

impl Holder {
    fn me(run_id: &str, started: DateTime<Utc>) -> Holder {
        Holder {
            run_id: run_id.to_string(),
            host: host_name(),
            pid: std::process::id(),
            started_at: started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
    /// gone is taken over (the guard says so); one held by a live process, or by any process on
    /// another host (a shared folder), is [`LockError::Held`]: DRE never guesses about a run it
    /// can't see.
    pub fn lock(&self, run_id: &str, started: DateTime<Utc>) -> Result<Guard, LockError> {
        std::fs::create_dir_all(&self.dir).map_err(LockError::Io)?;
        let path = self.dir.join(LOCK);
        let me = Holder::me(run_id, started);
        let body = serde_json::to_string_pretty(&me).expect("a holder serializes") + "\n";
        let mut took_over = None;
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(body.as_bytes()).map_err(LockError::Io)?;
                    f.sync_all().map_err(LockError::Io)?;
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
                    if holder.host != me.host || process_alive(holder.pid) {
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

    /// Move a folder from before run folders (outputs and `run_results.json` straight in the
    /// Binding's folder) into `runs/<started>-legacy/` and make it current, so its drift history
    /// and results carry over. Call it holding the lock. Returns the new run id, if there was
    /// anything to move.
    pub fn migrate_legacy(&self) -> std::io::Result<Option<String>> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(None);
        };
        let old: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                ![CURRENT, LOCK, RUNS].contains(&name.as_str())
                    && !(name.starts_with('.') && name.ends_with(".tmp"))
            })
            .collect();
        if old.is_empty() {
            return Ok(None);
        }
        let started = std::fs::metadata(self.dir.join("run_results.json"))
            .and_then(|m| m.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        let id = format!("{}-legacy", started.format("%Y%m%dT%H%M%SZ"));
        let to = self.run_dir(&id);
        std::fs::create_dir_all(&to)?;
        for p in old {
            std::fs::rename(&p, to.join(p.file_name().unwrap()))?;
        }
        if self.current().is_none() {
            self.switch(&id)?;
        }
        Ok(Some(id))
    }

    /// Remove all but the newest `keep` runs (of any status, finished or not), never the current
    /// run. Call it holding the lock.
    pub fn prune(&self, keep: usize) -> Vec<String> {
        let current = self.current();
        let ids = self.list();
        let cut = ids.len().saturating_sub(keep);
        let mut removed = Vec::new();
        for id in &ids[..cut] {
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
pub fn host_name() -> String {
    if let Ok(v) = std::env::var("DRE_HOST_NAME")
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
    if let Ok(v) = std::env::var("COMPUTERNAME") {
        return v;
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
        let g = r.lock("a", now).unwrap();
        match r.lock("b", now) {
            Err(LockError::Held(h)) => {
                assert_eq!(h.run_id, "a");
                assert_eq!(h.pid, std::process::id());
            }
            other => panic!("{other:?}"),
        }
        drop(g);
        assert!(r.holder().is_none());
        r.lock("b", now).unwrap();
    }

    #[test]
    fn a_dead_processs_lock_is_taken_over_but_another_hosts_never() {
        let (_d, r) = runs();
        std::fs::create_dir_all(r.dir()).unwrap();
        let dead = Holder {
            run_id: "old".into(),
            host: host_name(),
            pid: 999_999,
            started_at: "2026-10-09T06:00:00Z".into(),
        };
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&dead).unwrap()).unwrap();
        let g = r.lock("new", Utc::now()).unwrap();
        assert_eq!(g.took_over.as_ref().map(|h| h.run_id.as_str()), Some("old"));
        drop(g);
        let elsewhere = Holder {
            host: "some-other-host".into(),
            ..dead
        };
        std::fs::write(r.dir().join(LOCK), serde_json::to_string(&elsewhere).unwrap()).unwrap();
        assert!(matches!(r.lock("new", Utc::now()), Err(LockError::Held(_))));
        assert_eq!(r.unlock().unwrap().unwrap().host, "some-other-host");
        r.lock("new", Utc::now()).unwrap();
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
    fn an_old_layout_moves_into_a_legacy_run() {
        let (_d, r) = runs();
        std::fs::create_dir_all(r.dir()).unwrap();
        std::fs::write(r.dir().join("daily.csv"), "a\n").unwrap();
        std::fs::write(r.dir().join("run_results.json"), "{}").unwrap();
        let id = r.migrate_legacy().unwrap().unwrap();
        assert!(id.ends_with("-legacy"));
        assert_eq!(r.current().as_deref(), Some(id.as_str()));
        assert!(r.run_dir(&id).join("daily.csv").is_file());
        assert!(!r.dir().join("daily.csv").exists());
        assert_eq!(r.migrate_legacy().unwrap(), None);
    }

    #[test]
    fn this_process_is_alive() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(999_999));
    }
}
