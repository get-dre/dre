//! The delivery cases (`conformance/delivery.json`), run against [`crate::delivery`] with an
//! in-memory store. The Go SDK runs the same cases against its own delivery code, so both apply
//! `if_exists`, temporary names, `temp_dir`, retries and numbered names the same way.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::Check;
use crate::delivery::{Caps, Rules, Store, StoreError, deliver_with};
use crate::plugin::PluginError;

/// The delivery cases, as published.
pub const DELIVERY_CASES: &str = include_str!("../../conformance/delivery.json");

#[derive(Debug, Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    store: StoreCaps,
    #[serde(default)]
    existing: BTreeMap<String, String>,
    #[serde(default)]
    connection: Map<String, Value>,
    #[serde(default)]
    options: Map<String, Value>,
    #[serde(default)]
    faults: Faults,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    expect: Option<Expect>,
    /// Several deliveries into one store, each with the path it must land at.
    #[serde(default)]
    deliveries: Vec<Landing>,
}

#[derive(Debug, Default, Deserialize)]
struct StoreCaps {
    #[serde(default)]
    create_exclusive: bool,
    #[serde(default)]
    rename_no_replace: bool,
    #[serde(default)]
    visible_when_complete: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Faults {
    /// Temporary failures the first writes give.
    #[serde(default)]
    temporary: u32,
    /// The `Retry-After`, in seconds, those failures carry.
    #[serde(default)]
    retry_after: Option<f64>,
    /// The first rename fails for good.
    #[serde(default)]
    break_rename: bool,
}

#[derive(Debug, Deserialize)]
struct Landing {
    remote: String,
    path: String,
}

#[derive(Debug, Default, Deserialize)]
struct Expect {
    path: Option<String>,
    attempts: Option<u32>,
    ops: Option<Vec<String>>,
    files: Option<BTreeMap<String, String>>,
    waits: Option<Vec<f64>>,
    waits_between: Option<Vec<(f64, f64)>>,
    error: Option<ExpectError>,
    settings_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectError {
    kind: Option<String>,
    code: Option<String>,
    contains: Option<String>,
}

/// An in-memory store that logs every call and fails on cue. The local file's content is
/// always `new`.
struct Memory {
    caps: Caps,
    files: BTreeMap<String, String>,
    temporary: u32,
    retry_after: Option<Duration>,
    break_rename: bool,
    ops: Vec<String>,
}

impl Store for Memory {
    fn caps(&self) -> Caps {
        self.caps
    }

    fn write(&mut self, _local: &Path, remote: &str, exclusive: bool) -> Result<(), StoreError> {
        self.ops.push(format!(
            "write {remote}{}",
            if exclusive { " exclusive" } else { "" }
        ));
        if self.temporary > 0 {
            self.temporary -= 1;
            return Err(StoreError::Temporary {
                message: "connection reset".into(),
                retry_after: self.retry_after,
            });
        }
        if exclusive && self.files.contains_key(remote) {
            return Err(StoreError::Exists);
        }
        self.files.insert(remote.into(), "new".into());
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str, replace: bool) -> Result<(), StoreError> {
        self.ops.push(format!(
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
            .ok_or_else(|| StoreError::Failed(format!("no file at {from}")))?;
        self.files.insert(to.into(), v);
        Ok(())
    }

    fn exists(&mut self, remote: &str) -> Result<bool, StoreError> {
        self.ops.push(format!("exists {remote}"));
        Ok(self.files.contains_key(remote))
    }

    fn delete(&mut self, remote: &str) -> Result<(), StoreError> {
        self.ops.push(format!("delete {remote}"));
        self.files.remove(remote);
        Ok(())
    }
}

/// Run every delivery case against the Rust SDK's delivery code.
///
/// # Panics
///
/// Panics if the published cases don't parse (a test keeps them parsing).
pub fn run() -> Vec<Check> {
    let cases: Cases = serde_json::from_str(DELIVERY_CASES).expect("conformance/delivery.json parses");
    cases
        .cases
        .iter()
        .map(|c| {
            let r = run_case(c);
            Check {
                plugin: "dre-protocol delivery".into(),
                name: c.name.clone(),
                passed: r.is_ok(),
                detail: r.err().unwrap_or_default(),
                skipped: None,
            }
        })
        .collect()
}

fn run_case(c: &Case) -> Result<(), String> {
    let base = match c.base.as_deref() {
        Some("legacy") => Rules::legacy(),
        _ => Rules::default(),
    };
    let rules = match Rules::from_settings(base, &c.connection, &c.options, &[]) {
        Ok((r, _)) => r,
        Err(e) => {
            return match c.expect.as_ref().and_then(|x| x.settings_error.as_deref()) {
                Some(want) if e.contains(want) => Ok(()),
                _ => Err(format!("the settings were refused: {e}")),
            };
        }
    };
    let mut store = Memory {
        caps: Caps {
            create_exclusive: c.store.create_exclusive,
            rename_no_replace: c.store.rename_no_replace,
            visible_when_complete: c.store.visible_when_complete,
        },
        files: c.existing.clone(),
        temporary: c.faults.temporary,
        retry_after: c.faults.retry_after.map(Duration::from_secs_f64),
        break_rename: c.faults.break_rename,
        ops: Vec::new(),
    };
    for l in &c.deliveries {
        let d = deliver_with(&mut store, Path::new("new"), &l.remote, &rules, &mut |_| {})
            .map_err(|e| format!("{}: {e}", l.remote))?;
        if d.path != l.path {
            return Err(format!("{} landed at {}, expected {}", l.remote, d.path, l.path));
        }
    }
    let (Some(remote), Some(x)) = (&c.remote, &c.expect) else {
        return Ok(());
    };
    if let Some(want) = &x.settings_error {
        return Err(format!("expected the settings to be refused (`{want}`)"));
    }
    let mut waits = Vec::new();
    let r = deliver_with(&mut store, Path::new("new"), remote, &rules, &mut |w| {
        waits.push(w.as_secs_f64())
    });
    let attempts = match (&r, &x.error) {
        (Ok(d), None) => {
            if let Some(p) = &x.path
                && &d.path != p
            {
                return Err(format!("landed at {}, expected {p}", d.path));
            }
            d.attempts
        }
        (Err(e), Some(want)) => {
            let attempts = e.attempts;
            let pe = PluginError::from(e.clone());
            if let Some(k) = &want.kind
                && pe.kind.as_str() != k
            {
                return Err(format!("error kind {}, expected {k}", pe.kind.as_str()));
            }
            if want.code.as_deref() != pe.code.as_deref() && want.code.is_some() {
                return Err(format!("error code {:?}, expected {:?}", pe.code, want.code));
            }
            if let Some(s) = &want.contains
                && !pe.message.contains(s.as_str())
            {
                return Err(format!("error `{}` doesn't contain `{s}`", pe.message));
            }
            attempts
        }
        (Ok(d), Some(_)) => return Err(format!("expected an error, it landed at {}", d.path)),
        (Err(e), None) => return Err(format!("failed: {e}")),
    };
    if let Some(n) = x.attempts
        && n != attempts
    {
        return Err(format!("took {attempts} tries, expected {n}"));
    }
    if let Some(ops) = &x.ops
        && &store.ops != ops
    {
        return Err(format!("store calls {:?}, expected {ops:?}", store.ops));
    }
    if let Some(files) = &x.files
        && &store.files != files
    {
        return Err(format!("files {:?}, expected {files:?}", store.files));
    }
    if let Some(w) = &x.waits
        && &waits != w
    {
        return Err(format!("waited {waits:?}s, expected {w:?}s"));
    }
    if let Some(ranges) = &x.waits_between {
        let ok =
            waits.len() == ranges.len() && waits.iter().zip(ranges).all(|(w, (lo, hi))| lo <= w && w <= hi);
        if !ok {
            return Err(format!("waited {waits:?}s, expected within {ranges:?}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_rust_sdk_follows_every_delivery_case() {
        let failed: Vec<String> = super::run()
            .into_iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
