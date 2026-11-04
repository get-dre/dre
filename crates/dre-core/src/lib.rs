//! DRE core: turns a project directory into a resolved, validated project, and runs it.

// A public function that can panic says when in a `# Panics` section (CI denies warnings).
#![warn(clippy::missing_panics_doc)]

pub mod catalog;
pub mod checksum;
pub mod codes;
pub mod coltypes;
pub mod config;
pub mod constraints;
pub mod context;
pub mod data_tests;
pub mod dates;
pub mod delivery;
pub mod diag;
pub mod dialect;
pub mod document;
pub mod engine;
pub mod error;
pub mod fonts;
pub mod for_each;
pub mod formats;
pub mod fs;
pub mod lock;
pub mod lookups;
pub mod manager;
pub mod manifest;
pub mod message;
mod mutable;
pub mod no_data;
pub mod numbers;
pub mod occurrences;
pub mod options;
pub mod packages;
pub mod parse;
pub mod plugin_host;
pub mod plugins;
pub mod preflight;
pub mod profiles;
pub mod project;
pub mod query;
pub mod rate;
pub mod release;
pub mod render;
pub mod rows;
pub mod run;
pub mod run_results;
pub mod runs;
pub mod saved;
pub mod schedule;
mod schema;
pub mod secret_columns;
pub mod secrets;
pub mod selector;
pub mod settings;
pub mod sqlsplit;
pub mod start;
pub mod target;
pub mod template_file;
pub mod values;
pub mod variables;
pub mod yaml;
pub mod zip_pack;

pub use context::{Context, Env};
pub use diag::{Diagnostic, Diagnostics, Severity};

/// The version string `dre --version` prints. DRE has no released version yet.
pub fn version() -> &'static str {
    match env!("CARGO_PKG_VERSION") {
        "0.0.0" => "unreleased",
        v => v,
    }
}

/// A path with `/` separators on every platform, so messages and goldens read the same everywhere.
pub fn slash(p: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(p.to_string_lossy().replace('\\', "/"))
}

/// A path relative to `root` when it is inside it, with `/` separators on every platform.
/// Paths outside `root` keep their original prefix. Relativity follows the native path rules;
/// separator normalization happens afterwards, as in [`slash`].
pub fn slash_relative(root: &std::path::Path, path: &std::path::Path) -> std::path::PathBuf {
    slash(path.strip_prefix(root).unwrap_or(path))
}

#[cfg(test)]
mod path_tests {
    use std::path::Path;

    #[test]
    fn relative_paths_preserve_native_prefix_rules_before_normalizing_separators() {
        for (root, path, expected) in [
            ("project", "project/reports/daily.sql", "reports/daily.sql"),
            ("project", "elsewhere/daily.sql", "elsewhere/daily.sql"),
            (
                "project",
                "./project/reports/daily.sql",
                "./project/reports/daily.sql",
            ),
            ("./project", "./project/reports/daily.sql", "reports/daily.sql"),
            ("project", "project/./reports/daily.sql", "reports/daily.sql"),
            ("project", "project", ""),
            ("project", r"project/reports\daily.sql", "reports/daily.sql"),
        ] {
            assert_eq!(
                super::slash_relative(Path::new(root), Path::new(path)).to_string_lossy(),
                expected,
                "root {root:?}, path {path:?}"
            );
        }
        let root = std::env::temp_dir().join("dre-path-contract");
        assert_eq!(
            super::slash_relative(&root, &root.join("reports").join("daily.sql")),
            Path::new("reports/daily.sql")
        );
        let outside = root.with_file_name("outside").join("daily.sql");
        assert_eq!(super::slash_relative(&root, &outside), super::slash(&outside));
    }

    #[cfg(unix)]
    #[test]
    fn relative_paths_strip_only_native_prefixes_and_replace_invalid_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        assert_eq!(
            super::slash_relative(Path::new("project"), Path::new(r"project\reports\daily.sql")),
            Path::new("project/reports/daily.sql")
        );
        let path = Path::new(OsStr::from_bytes(b"project/report-\xff.sql"));
        assert_eq!(
            super::slash_relative(Path::new("project"), path).to_string_lossy(),
            "report-\u{fffd}.sql"
        );
    }
}

mod test_blocks;
