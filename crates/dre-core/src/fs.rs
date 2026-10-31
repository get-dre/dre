//! Writing DRE's durable state: [`write_atomic`] for every file a later run, or another tool,
//! reads back (`run_results.json`, the drift snapshot, the manifest, `dre.lock`, plugin and
//! package metadata). A forced stop mid-write leaves the previous or the next complete file,
//! never broken JSON. Report outputs aren't state and are written as they are.

pub use dre_protocol::util::{write_atomic, write_atomic_mode};

/// Swap a prepared directory into place, restoring the old one if the final rename fails.
pub fn replace_dir(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    let old = if to.exists() {
        let parent = to
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", to.display()))?;
        let old = tempfile::Builder::new()
            .prefix(".old-")
            .tempdir_in(parent)
            .map_err(|e| format!("can't create a staging folder in {}: {e}", parent.display()))?;
        std::fs::rename(to, old.path()).map_err(|e| format!("can't replace {}: {e}", to.display()))?;
        Some(old)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(from, to) {
        if let Some(old) = &old {
            let _ = std::fs::rename(old.path(), to);
        }
        return Err(e.to_string());
    }
    Ok(())
}

/// Whether `path` is a strict descendant of `parent` by path component.
pub fn is_below(path: &std::path::Path, parent: &std::path::Path) -> bool {
    path != parent && path.starts_with(parent)
}

#[cfg(test)]
mod tests {
    /// The modules that write durable state use [`super::write_atomic`], never a plain write.
    #[test]
    fn durable_state_is_only_written_atomically() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for module in [
            "engine.rs",
            "lock.rs",
            "manifest.rs",
            "packages.rs",
            "plugins.rs",
            "target.rs",
        ] {
            let text = std::fs::read_to_string(src.join(module)).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            for bad in ["fs::write(", "File::create("] {
                assert!(
                    !code.contains(bad),
                    "{module} writes with `{bad}`; use dre_core::fs::write_atomic"
                );
            }
        }
    }
}
