//! Host-side process management and the DRE protocol conformance runner.
//!
//! Internal to the matching DRE release; not a stable Rust embedding API.
#![warn(clippy::missing_panics_doc)]

pub mod conformance;
#[cfg(target_os = "linux")]
mod fuse_fds;
pub mod host;
