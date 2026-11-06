//! Template pieces shared by `dre-core` and the `document` plugin, so a document template reads
//! exactly what a message template reads: the date functions, the number filters, `var()`,
//! `run.*`, secrets masking, query results and where template files are found. The Rust API is
//! internal to the matching DRE release and carries no stability guarantee.

pub mod common;
pub mod dates;
pub mod document;
mod env;
pub mod mutable;
pub mod numbers;
pub mod result;
pub mod secrets;
pub mod template_file;
pub mod values;

pub use env::Env;

/// Folder names core and the `template_file` search skip or reserve in a project.
pub const DEPS_DIR: &str = "dre_deps";
pub const TARGET_DIR: &str = "target";
pub const LOGS_DIR: &str = "logs";

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
