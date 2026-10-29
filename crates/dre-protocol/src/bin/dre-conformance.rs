//! Run the protocol conformance suite against a plugin executable, in any language:
//!
//! ```text
//! dre-conformance <plugin executable> [--session <session.json>]
//! ```
//!
//! `session.json`, for a source, runs the session cases:
//! `{"connection": {...}, "statements": {"rows": "...", "empty": "...", "no_result": "...",
//! "error": "...", "slow": "..."}}` (any of the statements). It prints every case and exits 1
//! when one fails.

use std::path::PathBuf;
use std::process::ExitCode;

use dre_protocol::conformance::{self, Options, Session};
use serde_json::{Map, Value};

fn session(path: &str) -> Result<Session, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let connection = v["connection"].as_object().cloned().unwrap_or_else(Map::new);
    let mut s = Session::new(connection);
    for (role, sql) in v["statements"].as_object().into_iter().flatten() {
        let sql = sql
            .as_str()
            .ok_or_else(|| format!("{path}: statement `{role}` isn't a string"))?;
        s = s.statement(role, sql);
    }
    Ok(s)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (plugin, options) = match args.as_slice() {
        [plugin] => (plugin, Options::default()),
        [plugin, flag, file] if flag == "--session" => match session(file) {
            Ok(s) => (plugin, Options::default().session(s)),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("usage: dre-conformance <plugin executable> [--session <session.json>]");
            return ExitCode::from(2);
        }
    };
    let checks = conformance::run_with(&PathBuf::from(plugin), &options);
    let mut failed = 0;
    for c in &checks {
        match (&c.skipped, c.passed) {
            (Some(why), _) => println!("  - {}: {} (skipped: {why})", c.plugin, c.name),
            (None, true) => println!("  ✓ {}: {}", c.plugin, c.name),
            (None, false) => {
                failed += 1;
                println!("  ✗ {}: {}: {}", c.plugin, c.name, c.detail);
            }
        }
    }
    println!("{} cases, {failed} failed", checks.len());
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
