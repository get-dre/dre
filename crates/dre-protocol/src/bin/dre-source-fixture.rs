//! A fixture source plugin for protocol tests. It speaks the protocol through the SDK, except in
//! the misbehaving modes selected by `DRE_FIXTURE_MODE`:
//!
//! - `old_protocol`: answers the handshake claiming only versions 7..=9
//! - `protocol_zero`: answers the handshake claiming only version 0, as a plugin from before
//!   protocol 1 does
//! - `garbage`: writes bytes that aren't a frame
//! - `silent`: never answers
//! - `die`: logs to stderr and exits 5 at start
//! - `no_sessions`: a normal source that doesn't advertise `sessions`
//! - `no_check`: a normal source that doesn't advertise `check`
//!
//! Every `open` logs `fixture: opened read_only=<bool>` to stderr, and writes the process id to
//! `$DRE_FIXTURE_PID_FILE` when it's set.
//!
//! In its normal mode it's a package of two plugins: the `fixture` source (served when core's
//! hello names no plugin) and an `inbox` destination with the source's connection fields, which
//! stands in for a destination on the same platform (`dre init` tests).
//!
//! SQL it understands: `rows N` (N rows of `n`, batches of 3), `none`, `fail`, `crash`,
//! `log <text>`, `log_prefix <text>`, `panic`, `sleep N` (up to N seconds, until cancelled; the
//! cancel hook logs `fixture: cancel hook`), `stubborn N` (N seconds, ignoring cancel), `slog <text>` (a `log` message at info, with a
//! field), `progress` (two progress messages), `coded` (an error with kind `auth` and code
//! `bad-token`). `check` accepts anything except `bad`.

use std::io::Write;
use std::sync::Arc;

use arrow::array::{Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};
use dre_protocol::msg::ConnectionField;
use dre_protocol::msg::LogLevel;
use dre_protocol::plugin::{
    About, Destination, ErrorKind, Plugin, PluginError, ResultSink, Source, serve_package, serve_source,
};
use dre_protocol::{CAP_CHECK, CAP_READ_ONLY, CAP_SESSIONS};
use serde_json::{Map, Value};

struct Fixture {
    opened: bool,
}

impl Source for Fixture {
    fn identifier_quote(&self) -> Option<&'static str> {
        Some("\"")
    }

    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            // `same_as_source` matters for the `inbox` destination, which has these fields too.
            ConnectionField::new("path", "where the data lives")
                .required()
                .same_as_source("fixture"),
            ConnectionField::new("token", "secret").secret(),
            // `dre init` must not ask for this one.
            ConnectionField::new("token_text", "the token as text")
                .secret()
                .manual(),
        ]
    }

    fn open(
        &mut self,
        connection: &Map<String, Value>,
        _read_only: bool,
    ) -> dre_protocol::plugin::Result<()> {
        if let Some(fail) = connection.get("fail") {
            let detail = fail.as_str().unwrap_or("fixture told to fail");
            return Err(format!("can't connect: {detail}").into());
        }
        self.opened = true;
        eprintln!("fixture: opened read_only={_read_only}");
        if let Ok(f) = std::env::var("DRE_FIXTURE_PID_FILE") {
            let _ = std::fs::write(f, std::process::id().to_string());
        }
        Ok(())
    }

    fn execute(
        &mut self,
        sql: &str,
        _row_limit: Option<u64>,
        out: &mut dyn ResultSink,
    ) -> dre_protocol::plugin::Result<()> {
        if !self.opened {
            return Err("execute before open".into());
        }
        let (cmd, arg) = sql.split_once(' ').unwrap_or((sql, ""));
        match cmd {
            "rows" => {
                let n: i64 = arg.parse()?;
                let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
                out.begin(schema.clone())?;
                let mut i = 0;
                while i < n {
                    let end = (i + 3).min(n);
                    let b = RecordBatch::try_new(
                        schema.clone(),
                        vec![Arc::new(Int64Array::from_iter_values(i..end))],
                    )?;
                    if !out.batch(b)? {
                        break;
                    }
                    i = end;
                }
                Ok(())
            }
            "none" => out.no_result(Some(0)),
            "log" => {
                eprintln!("{arg}");
                out.no_result(None)
            }
            "log_prefix" => {
                eprintln!("{}", arg.chars().take(20).collect::<String>());
                out.no_result(None)
            }
            "sleep" => {
                let secs: u64 = arg.parse()?;
                dre_protocol::plugin::on_cancel(|| log::warn!("fixture: cancel hook"));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                while std::time::Instant::now() < deadline {
                    if dre_protocol::plugin::cancelled() {
                        return Err("fixture: stopped".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                out.no_result(None)
            }
            "stubborn" => {
                // Ignores cancel: core has to kill it.
                std::thread::sleep(std::time::Duration::from_secs(arg.parse()?));
                out.no_result(None)
            }
            "slog" => {
                let mut fields = Map::new();
                fields.insert("attempt".into(), Value::from(2));
                dre_protocol::plugin::log_fields(LogLevel::Info, arg, fields);
                out.no_result(None)
            }
            "progress" => {
                dre_protocol::plugin::progress(Some("reading"), Some(1), Some(2));
                dre_protocol::plugin::progress(Some("reading"), Some(2), Some(2));
                out.no_result(None)
            }
            "coded" => Err(PluginError::new(ErrorKind::Auth, "the token was refused")
                .code("bad-token")
                .into()),
            "crash" => std::process::exit(3),
            "panic" => panic!("fixture panic"),
            _ => Err(format!("fixture can't run `{sql}`").into()),
        }
    }

    fn check(&mut self, sql: &str) -> dre_protocol::plugin::Result<()> {
        if sql == "bad" {
            Err("fixture says: bad statement".into())
        } else {
            Ok(())
        }
    }
}

fn main() {
    match std::env::var("DRE_FIXTURE_MODE").as_deref() {
        Ok(mode @ ("old_protocol" | "protocol_zero")) => {
            let (min_version, max_version) = if mode == "old_protocol" { (7, 9) } else { (0, 0) };
            let mut stdin = std::io::stdin();
            let _ = dre_protocol::frame::read_frame(&mut stdin);
            let mut out = std::io::stdout();
            let _ = dre_protocol::frame::write_json(
                &mut out,
                &dre_protocol::msg::Response::VersionMismatch {
                    min_version,
                    max_version,
                },
            );
            std::process::exit(1);
        }
        Ok("garbage") => {
            let _ = std::io::stdout().write_all(&[0, 0, 0, 3, b'Z', 1, 2]);
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_secs(5));
            std::process::exit(0);
        }
        Ok("silent") => {
            std::thread::sleep(std::time::Duration::from_secs(60));
            std::process::exit(0);
        }
        Ok("die") => {
            eprintln!("boom: fixture died on purpose");
            std::process::exit(5);
        }
        _ => {}
    }
    if std::env::var("DRE_FIXTURE_MODE").as_deref() == Ok("no_check") {
        let about =
            About::new("fixture", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_SESSIONS, CAP_READ_ONLY]);
        serve_source(about, Fixture { opened: false })
    }
    if std::env::var("DRE_FIXTURE_MODE").as_deref() == Ok("no_sessions") {
        let about =
            About::new("fixture", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_READ_ONLY, CAP_CHECK]);
        serve_source(about, Fixture { opened: false })
    }
    let about = About::new("fixture", env!("CARGO_PKG_VERSION")).capabilities(&[
        CAP_SESSIONS,
        CAP_READ_ONLY,
        CAP_CHECK,
    ]);
    serve_package(vec![
        Plugin::Source(about, Box::new(Fixture { opened: false })),
        Plugin::Destination(About::new("inbox", env!("CARGO_PKG_VERSION")), Box::new(Inbox)),
    ])
}

/// A destination with the source's connection fields, recording nothing.
struct Inbox;

impl Destination for Inbox {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        Fixture { opened: false }.connection_fields()
    }

    fn deliver(
        &mut self,
        local: &std::path::Path,
        _remote: Option<&str>,
        _connection: &Map<String, Value>,
    ) -> dre_protocol::plugin::Result<String> {
        Ok(format!("inbox:{}", local.display()))
    }
}
