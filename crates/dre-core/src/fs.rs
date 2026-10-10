//! Writing DRE's durable state: [`write_atomic`] for every file a later run, or another tool,
//! reads back (`run_results.json`, the drift snapshot, the manifest, `dre.lock`, plugin and
//! package metadata). A forced stop mid-write leaves the previous or the next complete file,
//! never broken JSON. Report outputs aren't state and are written as they are.

pub use dre_protocol::util::{write_atomic, write_atomic_mode};

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
