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
