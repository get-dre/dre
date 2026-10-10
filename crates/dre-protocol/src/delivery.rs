//! The delivery and network rules every file destination shares, written once.
//!
//! A destination implements a few primitives on its server ([`Store`]: write a file, rename,
//! exists, delete, and what it can do atomically). [`deliver`] applies the rules on top:
//!
//! - `if_exists` ([`IfExists`]): replace a file already at the path, fail, or keep both by
//!   numbering the new one (`report_2.xlsx`);
//! - `atomic` and `temp_dir`: upload under a temporary name (`.<name>.dre-part`, in the same
//!   folder or in `temp_dir`), then rename, so a half-written file never appears under its
//!   final name;
//! - `retries`: try again on a temporary error (a dropped connection, a 429 or 5xx), with
//!   backoff and `Retry-After`;
//! - `connect_timeout` and `timeout`, which the plugin applies to its client ([`Rules`] parses
//!   them).
//!
//! The Go plugin module has the same rules (`go/plugin/delivery.go`). [`LocalStore`] is the local
//! file system, used by core's built-in `local` destination.

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::msg::ConnectionField;
use crate::options::{OptionField, OptionType};
use crate::plugin::{ErrorKind, PluginError};

/// What to do when a file already exists at the delivery path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfExists {
    /// Replace it (the default: a rerun replaces a bad file).
    Overwrite,
    /// Fail this delivery.
    Error,
    /// Keep both: the new file is saved as `<name>_2.<ext>`, `_3`, and so on.
    Number,
}

impl IfExists {
    pub fn parse(s: &str) -> Option<IfExists> {
        match s {
            "overwrite" => Some(IfExists::Overwrite),
            "error" => Some(IfExists::Error),
            "number" => Some(IfExists::Number),
            _ => None,
        }
    }
}

/// The rules for one delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    pub if_exists: IfExists,
    /// Write under a temporary name, then rename.
    pub atomic: bool,
    /// Where the temporary file goes: absolute, or relative to the final file's folder. `None`:
    /// next to the final file.
    pub temp_dir: Option<String>,
    /// How many times to try again after a temporary error (0: never).
    pub retries: u32,
    /// For the plugin's client: how long to wait for a connection.
    pub connect_timeout: Duration,
    /// For the plugin's client: how long a read or write may make no progress.
    pub timeout: Duration,
}

pub const DEFAULT_RETRIES: u32 = 3;
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

impl Default for Rules {
    fn default() -> Rules {
        Rules {
            if_exists: IfExists::Overwrite,
            atomic: true,
            temp_dir: None,
            retries: DEFAULT_RETRIES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Rules {
    /// How destinations delivered before these rules: replace, straight to the final name, no
    /// retries.
    pub fn legacy() -> Rules {
        Rules {
            atomic: false,
            retries: 0,
            ..Rules::default()
        }
    }

    /// The rules from a profile target's `connection` (`connect_timeout`, `timeout`,
    /// `retries`) and a destination entry's `options` (`if_exists`, `atomic`, `temp_dir`),
    /// starting from `base`. `aliases` maps deprecated keys to these (`("upload_timeout",
    /// "timeout")`): an alias still works, with a warning. Returns the rules and the warnings.
    pub fn from_settings(
        base: Rules,
        connection: &Map<String, Value>,
        options: &Map<String, Value>,
        aliases: &[(&str, &str)],
    ) -> Result<(Rules, Vec<String>), String> {
        let mut warnings = Vec::new();
        let get = |key: &str, warnings: &mut Vec<String>| -> Option<Value> {
            for map in [options, connection] {
                if let Some(v) = map.get(key).filter(|v| !v.is_null()) {
                    return Some(v.clone());
                }
                for (old, new) in aliases {
                    if *new == key
                        && let Some(v) = map.get(*old).filter(|v| !v.is_null())
                    {
                        warnings.push(format!("`{old}` is deprecated; use `{new}`"));
                        return Some(v.clone());
                    }
                }
            }
            None
        };
        let mut r = base;
        if let Some(v) = get("if_exists", &mut warnings) {
            r.if_exists = v.as_str().and_then(IfExists::parse).ok_or_else(|| {
                format!("`if_exists` must be one of `overwrite`, `error`, `number`, got {v}")
            })?;
        }
        if let Some(v) = get("atomic", &mut warnings) {
            r.atomic = v
                .as_bool()
                .ok_or_else(|| format!("`atomic` must be true or false, got {v}"))?;
        }
        if let Some(v) = get("temp_dir", &mut warnings) {
            let s = v
                .as_str()
                .ok_or_else(|| format!("`temp_dir` must be a path, got {v}"))?;
            r.temp_dir = Some(s.to_string()).filter(|s| !s.is_empty());
        }
        if let Some(v) = get("retries", &mut warnings) {
            r.retries = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("`retries` must be a whole number, got {v}"))?;
        }
        if let Some(v) = get("connect_timeout", &mut warnings) {
            r.connect_timeout = parse_duration(&v).map_err(|e| format!("`connect_timeout` {e}"))?;
        }
        if let Some(v) = get("timeout", &mut warnings) {
            r.timeout = parse_duration(&v).map_err(|e| format!("`timeout` {e}"))?;
        }
        Ok((r, warnings))
    }
}

/// A duration: `30s`, `5m`, `2h`, `1500ms`, or a bare number of seconds.
pub fn parse_duration(v: &Value) -> Result<Duration, String> {
    let bad = || format!("must be a duration such as `30s`, `5m` or a number of seconds, got {v}");
    if let Some(n) = v.as_f64() {
        return (n >= 0.0 && n.is_finite())
            .then(|| Duration::from_secs_f64(n))
            .ok_or_else(bad);
    }
    let s = v.as_str().ok_or_else(bad)?.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let n: f64 = num.parse().map_err(|_| bad())?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => return Err(bad()),
    };
    Ok(Duration::from_secs_f64(secs))
}

/// The connection fields every network plugin shares, for its `connection_fields()`.
pub fn connection_fields() -> Vec<ConnectionField> {
    vec![
        ConnectionField::new(
            "connect_timeout",
            "how long to wait for a connection (`30s`, `2m`, or seconds)",
        )
        .default("30s")
        .manual(),
        ConnectionField::new(
            "timeout",
            "how long a read or write may make no progress before it fails",
        )
        .default("60s")
        .manual(),
        ConnectionField::new(
            "retries",
            "how many times to try again after a temporary error (0: never)",
        )
        .default(DEFAULT_RETRIES)
        .manual(),
    ]
}

/// The options every file destination shares, for its `options()`.
pub fn option_fields() -> Vec<OptionField> {
    vec![
        OptionField::new(
            "if_exists",
            OptionType::String,
            "when a file is already at the path: `overwrite` it, fail with `error`, or `number` the new one",
        )
        .choices(&["overwrite", "error", "number"])
        .default("overwrite"),
        OptionField::new(
            "atomic",
            OptionType::Boolean,
            "upload under a temporary name, then rename, so no half-written file appears",
        )
        .default(true),
        OptionField::new(
            "temp_dir",
            OptionType::String,
            "where the temporary file goes (on the same server), for receivers that pick up any new file",
        ),
    ]
}

/// What a store can do in one step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    /// `write` with `exclusive` fails with [`StoreError::Exists`] in the same step.
    pub create_exclusive: bool,
    /// `rename` without `replace` fails with [`StoreError::Exists`] in the same step.
    pub rename_no_replace: bool,
    /// A file only appears once it's complete (object stores), so no temporary name is needed.
    pub visible_when_complete: bool,
}

/// Why a store operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// Something is already at the path.
    Exists,
    /// Trying again may work: a dropped connection, a timeout before anything was accepted, a
    /// 429 or a 5xx. `retry_after` is the server's `Retry-After`.
    Temporary {
        message: String,
        retry_after: Option<Duration>,
    },
    /// Trying again won't help: refused credentials or permissions, a bad path.
    Failed(String),
}

impl StoreError {
    pub fn temporary(message: impl Into<String>) -> StoreError {
        StoreError::Temporary {
            message: message.into(),
            retry_after: None,
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Exists => f.write_str("a file is already there"),
            StoreError::Temporary { message, .. } | StoreError::Failed(message) => f.write_str(message),
        }
    }
}

/// A destination's primitives. Paths are the destination's own (`outbound/report.xlsx`).
pub trait Store {
    fn caps(&self) -> Caps;
    /// Write `local` to `remote`, creating its folders. With `exclusive`, fail with
    /// [`StoreError::Exists`] if something is there (only asked when `caps().create_exclusive`).
    fn write(&mut self, local: &Path, remote: &str, exclusive: bool) -> Result<(), StoreError>;
    /// Rename `from` to `to`, creating `to`'s folders. Without `replace`, fail with
    /// [`StoreError::Exists`] if something is there (only asked when `caps().rename_no_replace`).
    fn rename(&mut self, from: &str, to: &str, replace: bool) -> Result<(), StoreError>;
    fn exists(&mut self, remote: &str) -> Result<bool, StoreError>;
    /// Remove `remote`; a missing file isn't an error.
    fn delete(&mut self, remote: &str) -> Result<(), StoreError>;
}

/// Where a delivery landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    /// The final path (with `number`, possibly `report_2.xlsx`).
    pub path: String,
    /// How many tries it took (1 without a retry).
    pub attempts: u32,
}

/// A failed delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryError {
    pub message: String,
    /// It failed because a file was already at the path (`if_exists: error`).
    pub exists: bool,
    pub attempts: u32,
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DeliveryError {}

impl From<DeliveryError> for PluginError {
    fn from(e: DeliveryError) -> PluginError {
        let pe = PluginError::new(ErrorKind::Delivery, e.message);
        if e.exists { pe.code("file-exists") } else { pe }
    }
}

/// Deliver `local` to `remote` under `rules`, sleeping between retries.
pub fn deliver(
    store: &mut dyn Store,
    local: &Path,
    remote: &str,
    rules: &Rules,
) -> Result<Delivered, DeliveryError> {
    deliver_with(store, local, remote, rules, &mut std::thread::sleep)
}

/// [`deliver`], with the wait between retries supplied (tests don't wait).
pub fn deliver_with(
    store: &mut dyn Store,
    local: &Path,
    remote: &str,
    rules: &Rules,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Delivered, DeliveryError> {
    let mut attempt = 1;
    loop {
        match try_once(store, local, remote, rules) {
            Ok(path) => {
                return Ok(Delivered {
                    path,
                    attempts: attempt,
                });
            }
            Err(StoreError::Temporary { message, retry_after }) if attempt <= rules.retries => {
                let wait = retry_after.unwrap_or_else(|| backoff(attempt));
                crate::log::info!(
                    "{remote}: {message}; trying again in {}s (attempt {} of {})",
                    wait.as_secs(),
                    attempt + 1,
                    rules.retries + 1
                );
                sleep(wait);
                attempt += 1;
            }
            Err(e) => {
                let exists = e == StoreError::Exists;
                let message = if exists {
                    format!("{remote} already exists (`if_exists: error`)")
                } else if attempt > 1 {
                    format!("{e} (after {attempt} tries)")
                } else {
                    e.to_string()
                };
                return Err(DeliveryError {
                    message,
                    exists,
                    attempts: attempt,
                });
            }
        }
    }
}

/// About 1s, then 4s, then 16s, give or take a quarter.
fn backoff(attempt: u32) -> Duration {
    let base = 4f64.powi(attempt as i32 - 1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = 0.75 + (nanos % 1000) as f64 / 2000.0;
    Duration::from_secs_f64(base * jitter)
}

/// One try: the final path, or why it failed.
fn try_once(store: &mut dyn Store, local: &Path, remote: &str, rules: &Rules) -> Result<String, StoreError> {
    let caps = store.caps();
    let candidates = |i: u32| numbered(remote, i);
    let limit = if rules.if_exists == IfExists::Number {
        1000
    } else {
        1
    };
    if rules.atomic && !caps.visible_when_complete {
        let temp = temp_path(remote, rules.temp_dir.as_deref());
        if let Err(e) = store.write(local, &temp, false) {
            let _ = store.delete(&temp);
            return Err(e);
        }
        let replace = rules.if_exists == IfExists::Overwrite;
        for i in 1..=limit {
            let to = candidates(i);
            let r = if !replace && !caps.rename_no_replace {
                // No single-step check here (FTP): look, then rename.
                match store.exists(&to) {
                    Ok(true) => Err(StoreError::Exists),
                    Ok(false) => store.rename(&temp, &to, false),
                    Err(e) => Err(e),
                }
            } else {
                store.rename(&temp, &to, replace)
            };
            match r {
                Ok(()) => return Ok(to),
                Err(StoreError::Exists) if i < limit => continue,
                Err(e) => {
                    let _ = store.delete(&temp);
                    return Err(e);
                }
            }
        }
        unreachable!("the loop returns on its last candidate")
    }
    let exclusive = rules.if_exists != IfExists::Overwrite;
    for i in 1..=limit {
        let to = candidates(i);
        let r = if exclusive && !caps.create_exclusive {
            match store.exists(&to) {
                Ok(true) => Err(StoreError::Exists),
                Ok(false) => store.write(local, &to, false),
                Err(e) => Err(e),
            }
        } else {
            store.write(local, &to, exclusive)
        };
        match r {
            Ok(()) => return Ok(to),
            Err(StoreError::Exists) if i < limit => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns on its last candidate")
}

/// `remote`, or for `i` > 1 the numbered name: `out/report_2.xlsx`.
fn numbered(remote: &str, i: u32) -> String {
    if i == 1 {
        return remote.to_string();
    }
    let (dir, file) = split(remote);
    let (stem, ext) = match file.rfind('.') {
        Some(p) if p > 0 => (&file[..p], &file[p..]),
        _ => (file, ""),
    };
    format!("{dir}{stem}_{i}{ext}")
}

/// The temporary name for `remote`: `.<name>.dre-part`, next to it or in `temp_dir`.
fn temp_path(remote: &str, temp_dir: Option<&str>) -> String {
    let (dir, file) = split(remote);
    let name = format!(".{file}.dre-part");
    match temp_dir {
        None => format!("{dir}{name}"),
        Some(t) if t.starts_with('/') || Path::new(t).is_absolute() => {
            format!("{}/{name}", t.trim_end_matches(['/', '\\']))
        }
        Some(t) => format!("{dir}{}/{name}", t.trim_end_matches(['/', '\\'])),
    }
}

/// `("out/", "report.xlsx")`: the folder (with its separator) and the file name.
fn split(remote: &str) -> (&str, &str) {
    match remote.rfind(['/', '\\']) {
        Some(p) => (&remote[..=p], &remote[p + 1..]),
        None => ("", remote),
    }
}

/// The local file system, for core's `local` destination. Paths are file system paths.
#[derive(Debug, Default)]
pub struct LocalStore;

fn io_failed(what: &str, path: &str, e: std::io::Error) -> StoreError {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        StoreError::Exists
    } else {
        StoreError::Failed(format!("can't {what} {path}: {e}"))
    }
}

fn make_parent(path: &str) -> Result<(), StoreError> {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => std::fs::create_dir_all(p)
            .map_err(|e| StoreError::Failed(format!("can't create {}: {e}", p.display()))),
        _ => Ok(()),
    }
}

impl Store for LocalStore {
    fn caps(&self) -> Caps {
        Caps {
            create_exclusive: true,
            rename_no_replace: true,
            visible_when_complete: false,
        }
    }

    fn write(&mut self, local: &Path, remote: &str, exclusive: bool) -> Result<(), StoreError> {
        make_parent(remote)?;
        if !exclusive {
            return std::fs::copy(local, remote)
                .map(|_| ())
                .map_err(|e| io_failed("write", remote, e));
        }
        let mut src = std::fs::File::open(local)
            .map_err(|e| StoreError::Failed(format!("can't read {}: {e}", local.display())))?;
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(remote)
            .map_err(|e| io_failed("write", remote, e))?;
        std::io::copy(&mut src, &mut dst).map_err(|e| {
            let _ = std::fs::remove_file(remote);
            io_failed("write", remote, e)
        })?;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str, replace: bool) -> Result<(), StoreError> {
        make_parent(to)?;
        if replace {
            return std::fs::rename(from, to).map_err(|e| io_failed("rename to", to, e));
        }
        // A hard link fails if `to` exists, in one step; then drop the old name.
        std::fs::hard_link(from, to).map_err(|e| io_failed("rename to", to, e))?;
        std::fs::remove_file(from).map_err(|e| io_failed("remove", from, e))
    }

    fn exists(&mut self, remote: &str) -> Result<bool, StoreError> {
        Ok(Path::new(remote).exists())
    }

    fn delete(&mut self, remote: &str) -> Result<(), StoreError> {
        match std::fs::remove_file(remote) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_failed("remove", remote, e)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// An in-memory store that can fail on cue.
    #[derive(Default)]
    struct Fake {
        caps: Caps,
        files: BTreeMap<String, String>,
        /// Temporary failures to give before writes succeed.
        flaky: u32,
        /// Fail the next rename (a dropped connection after the upload).
        break_rename: bool,
        log: Vec<String>,
    }

    impl Store for Fake {
        fn caps(&self) -> Caps {
            self.caps
        }
        fn write(&mut self, local: &Path, remote: &str, exclusive: bool) -> Result<(), StoreError> {
            self.log
                .push(format!("write {remote}{}", if exclusive { " excl" } else { "" }));
            if self.flaky > 0 {
                self.flaky -= 1;
                return Err(StoreError::temporary("connection reset"));
            }
            if exclusive && self.files.contains_key(remote) {
                return Err(StoreError::Exists);
            }
            self.files.insert(remote.into(), local.display().to_string());
            Ok(())
        }
        fn rename(&mut self, from: &str, to: &str, replace: bool) -> Result<(), StoreError> {
            self.log.push(format!(
                "rename {from} {to}{}",
                if replace { " replace" } else { "" }
            ));
            if std::mem::take(&mut self.break_rename) {
                return Err(StoreError::Failed("connection dropped".into()));
            }
            if !replace && self.files.contains_key(to) {
                return Err(StoreError::Exists);
            }
            let v = self
                .files
                .remove(from)
                .ok_or(StoreError::Failed("no such file".into()))?;
            self.files.insert(to.into(), v);
            Ok(())
        }
        fn exists(&mut self, remote: &str) -> Result<bool, StoreError> {
            self.log.push(format!("exists {remote}"));
            Ok(self.files.contains_key(remote))
        }
        fn delete(&mut self, remote: &str) -> Result<(), StoreError> {
            self.files.remove(remote);
            Ok(())
        }
    }

    fn all_caps() -> Caps {
        Caps {
            create_exclusive: true,
            rename_no_replace: true,
            visible_when_complete: false,
        }
    }

    fn run(store: &mut Fake, rules: &Rules) -> Result<Delivered, DeliveryError> {
        deliver_with(store, Path::new("new"), "out/r.xlsx", rules, &mut |_| {})
    }

    fn rules(if_exists: IfExists, atomic: bool) -> Rules {
        Rules {
            if_exists,
            atomic,
            ..Rules::default()
        }
    }

    #[test]
    fn legacy_rules_write_straight_to_the_final_name() {
        let mut s = Fake::default();
        let d = run(&mut s, &Rules::legacy()).unwrap();
        assert_eq!((d.path.as_str(), d.attempts), ("out/r.xlsx", 1));
        assert_eq!(s.log, ["write out/r.xlsx"]);
    }

    #[test]
    fn atomic_uploads_under_a_temporary_name_then_renames() {
        let mut s = Fake {
            caps: all_caps(),
            ..Fake::default()
        };
        s.files.insert("out/r.xlsx".into(), "old".into());
        run(&mut s, &rules(IfExists::Overwrite, true)).unwrap();
        assert_eq!(
            s.log,
            [
                "write out/.r.xlsx.dre-part",
                "rename out/.r.xlsx.dre-part out/r.xlsx replace"
            ]
        );
        assert_eq!(s.files.keys().collect::<Vec<_>>(), ["out/r.xlsx"]);
        assert_eq!(s.files["out/r.xlsx"], "new");
    }

    #[test]
    fn a_dropped_rename_leaves_no_file_at_the_final_name_or_temporary_one() {
        let mut s = Fake {
            caps: all_caps(),
            break_rename: true,
            ..Fake::default()
        };
        assert!(run(&mut s, &rules(IfExists::Overwrite, true)).is_err());
        assert!(s.files.is_empty(), "{:?}", s.files);
    }

    #[test]
    fn temp_dir_holds_the_temporary_file() {
        let mut s = Fake {
            caps: all_caps(),
            ..Fake::default()
        };
        let mut r = rules(IfExists::Overwrite, true);
        r.temp_dir = Some("../staging".into());
        run(&mut s, &r).unwrap();
        assert_eq!(s.log[0], "write out/../staging/.r.xlsx.dre-part");
        r.temp_dir = Some("/tmp/".into());
        s.log.clear();
        run(&mut s, &r).unwrap();
        assert_eq!(s.log[0], "write /tmp/.r.xlsx.dre-part");
    }

    #[test]
    fn error_fails_when_the_file_exists_and_cleans_up() {
        for atomic in [false, true] {
            let mut s = Fake {
                caps: all_caps(),
                ..Fake::default()
            };
            s.files.insert("out/r.xlsx".into(), "old".into());
            let e = run(&mut s, &rules(IfExists::Error, atomic)).unwrap_err();
            assert!(e.exists && e.message.contains("already exists"), "{e:?}");
            assert_eq!(s.files.len(), 1, "atomic={atomic}: {:?}", s.files);
            assert_eq!(s.files["out/r.xlsx"], "old");
            let pe = PluginError::from(e);
            assert_eq!(
                (pe.kind, pe.code.as_deref()),
                (ErrorKind::Delivery, Some("file-exists"))
            );
        }
    }

    #[test]
    fn number_keeps_both_files() {
        for (atomic, caps) in [
            (false, all_caps()),
            (true, all_caps()),
            (false, Caps::default()),
            (true, Caps::default()),
        ] {
            let mut s = Fake {
                caps,
                ..Fake::default()
            };
            s.files.insert("out/r.xlsx".into(), "old".into());
            s.files.insert("out/r_2.xlsx".into(), "old2".into());
            let d = run(&mut s, &rules(IfExists::Number, atomic)).unwrap();
            assert_eq!(d.path, "out/r_3.xlsx", "atomic={atomic} caps={caps:?}");
            assert_eq!(s.files["out/r_3.xlsx"], "new");
            assert_eq!(s.files.len(), 3, "{:?}", s.files);
        }
    }

    #[test]
    fn without_single_step_checks_it_looks_first() {
        let mut s = Fake::default();
        s.files.insert("out/r.xlsx".into(), "old".into());
        assert!(run(&mut s, &rules(IfExists::Error, false)).unwrap_err().exists);
        assert_eq!(s.log, ["exists out/r.xlsx"]);
    }

    #[test]
    fn temporary_errors_are_retried_then_give_up() {
        let mut s = Fake {
            flaky: 2,
            ..Fake::default()
        };
        let mut waits = Vec::new();
        let d = deliver_with(&mut s, Path::new("new"), "r.csv", &Rules::default(), &mut |w| {
            waits.push(w)
        })
        .unwrap();
        assert_eq!(d.attempts, 3);
        assert_eq!(waits.len(), 2);
        assert!(waits[0] < waits[1]);
        let mut s = Fake {
            flaky: 9,
            ..Fake::default()
        };
        let e = deliver_with(&mut s, Path::new("new"), "r.csv", &Rules::default(), &mut |_| {}).unwrap_err();
        assert_eq!(e.attempts, 4);
        assert!(e.message.contains("after 4 tries"), "{e}");
        let no_retry = Rules {
            retries: 0,
            ..Rules::default()
        };
        let mut s = Fake {
            flaky: 1,
            ..Fake::default()
        };
        assert_eq!(run(&mut s, &no_retry).unwrap_err().attempts, 1);
    }

    #[test]
    fn object_stores_need_no_temporary_name() {
        let mut s = Fake {
            caps: Caps {
                create_exclusive: true,
                visible_when_complete: true,
                ..Caps::default()
            },
            ..Fake::default()
        };
        run(&mut s, &rules(IfExists::Error, true)).unwrap();
        assert_eq!(s.log, ["write out/r.xlsx excl"]);
    }

    #[test]
    fn settings_parse_with_aliases_and_errors() {
        let conn = json!({"connect_timeout": "2m", "upload_timeout": 90, "retries": "0"});
        let opts = json!({"if_exists": "number", "atomic": false, "temp_dir": "../in"});
        let (r, warnings) = Rules::from_settings(
            Rules::default(),
            conn.as_object().unwrap(),
            opts.as_object().unwrap(),
            &[("upload_timeout", "timeout")],
        )
        .unwrap();
        assert_eq!(r.if_exists, IfExists::Number);
        assert!(!r.atomic);
        assert_eq!(r.temp_dir.as_deref(), Some("../in"));
        assert_eq!(r.retries, 0);
        assert_eq!(r.connect_timeout, Duration::from_secs(120));
        assert_eq!(r.timeout, Duration::from_secs(90));
        assert_eq!(warnings, ["`upload_timeout` is deprecated; use `timeout`"]);
        let bad = json!({"if_exists": "keep"});
        assert!(
            Rules::from_settings(Rules::default(), &Map::new(), bad.as_object().unwrap(), &[])
                .unwrap_err()
                .contains("if_exists")
        );
    }

    #[test]
    fn durations_take_units_or_seconds() {
        for (v, secs) in [
            (json!("30s"), 30.0),
            (json!("5m"), 300.0),
            (json!("1h"), 3600.0),
            (json!("1500ms"), 1.5),
            (json!(45), 45.0),
            (json!("7"), 7.0),
        ] {
            assert_eq!(parse_duration(&v).unwrap(), Duration::from_secs_f64(secs), "{v}");
        }
        for v in [json!("soon"), json!("5 days"), json!(-1), json!(true)] {
            assert!(parse_duration(&v).is_err(), "{v}");
        }
    }

    #[test]
    fn names_number_and_hide_the_temporary_file() {
        assert_eq!(numbered("out/r.xlsx", 2), "out/r_2.xlsx");
        assert_eq!(numbered("r", 3), "r_3");
        assert_eq!(numbered("a.b/.env", 2), "a.b/.env_2");
        assert_eq!(numbered("x.tar.gz", 2), "x.tar_2.gz");
        assert_eq!(temp_path("r.csv", None), ".r.csv.dre-part");
    }

    #[test]
    fn the_local_store_follows_every_rule() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.csv");
        std::fs::write(&src, "new").unwrap();
        let dst = dir.path().join("out/r.csv");
        let dst = dst.to_str().unwrap();
        let mut s = LocalStore;
        deliver(&mut s, &src, dst, &Rules::legacy()).unwrap();
        assert_eq!(std::fs::read_to_string(dst).unwrap(), "new");
        std::fs::write(&src, "newer").unwrap();
        deliver(&mut s, &src, dst, &rules(IfExists::Overwrite, true)).unwrap();
        assert_eq!(std::fs::read_to_string(dst).unwrap(), "newer");
        let e = deliver(&mut s, &src, dst, &rules(IfExists::Error, true)).unwrap_err();
        assert!(e.exists);
        let d = deliver(&mut s, &src, dst, &rules(IfExists::Number, true)).unwrap();
        assert!(d.path.ends_with("r_2.csv"), "{d:?}");
        let d = deliver(&mut s, &src, dst, &rules(IfExists::Number, false)).unwrap();
        assert!(d.path.ends_with("r_3.csv"), "{d:?}");
        let mut names: Vec<String> = std::fs::read_dir(dir.path().join("out"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["r.csv", "r_2.csv", "r_3.csv"]);
    }
}
