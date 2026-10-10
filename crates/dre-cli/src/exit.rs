//! `dre`'s exit codes, in one place; docs/exit-codes.md lists them. Orchestrators decide from
//! them whether to retry, alert or stop, so each means one thing on every command.

use std::process::ExitCode;

/// Everything succeeded. Skipped outputs (`when:` false, `deliver: false`) count as success.
pub const OK: u8 = 0;
/// The command ran, but something failed: a report or a delivery (details per Binding in
/// `run_results.json`), or problems `dre validate` found. A retry may help.
pub const FAILED: u8 = 1;
/// The command couldn't start: bad flags or environment, an invalid project or profile, a missing
/// plugin. Fix the cause; retrying won't help.
pub const NOT_STARTED: u8 = 2;

/// [`FAILED`].
pub fn failed() -> ExitCode {
    ExitCode::from(FAILED)
}

/// [`NOT_STARTED`].
pub fn not_started() -> ExitCode {
    ExitCode::from(NOT_STARTED)
}

/// [`OK`].
pub fn ok() -> ExitCode {
    ExitCode::from(OK)
}
