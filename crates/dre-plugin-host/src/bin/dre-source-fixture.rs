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
//! - `stall`: a `stall` format plugin that reads one batch of a `write`, then never reads again
//! - `close_probe`: speaks the protocol by hand, outside the SDK: answers `hello`, `open` and
//!   `describe`, fails every `execute` with ``fixture can't run `<sql>` ``, and on a protocol `close`
//!   after an `open` writes `close` to `$DRE_FIXTURE_CLOSE_FILE`. End of input writes nothing, so
//!   a test can tell a `close` request from a closed pipe (the SDK handles both the same way)
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
//! `bad-token`), `flood N` (N batches of 65,536 rows, logging `fixture: sent <i>` to stderr after
//! each), `bytes` (a stderr line that isn't UTF-8, then `fixture: after bad bytes`), `orphan` (starts
//! a copy of itself in `silent` mode that keeps the plugin's stderr open for a minute).
//! `check` accepts anything except `bad`.

use std::io::Write;
use std::sync::Arc;

use arrow::array::{Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};
use dre_protocol::msg::ConnectionField;
use dre_protocol::msg::LogLevel;
use dre_protocol::plugin::{
    About, Destination, ErrorKind, Format, Plugin, PluginError, ResultSets, ResultSink, Source, WriteRequest,
    serve_format, serve_package, serve_source,
};
use dre_protocol::{CAP_CATALOG, CAP_CHECK, CAP_READ_ONLY, CAP_SESSIONS, CAP_VALIDATE};
use serde_json::{Map, Value};

struct Fixture {
    opened: bool,
}

impl Source for Fixture {
    fn sql_dialect(&self) -> Option<dre_protocol::sql::SqlDialect> {
        Some(fixture_dialect())
    }
    fn catalog(&mut self, _tables: &[String], _schemas: &[String]) -> dre_protocol::plugin::Result<Value> {
        Ok(serde_json::json!({
            "schema_version": "dre/catalog/v1",
            "identifier_quote": "\"",
            "dialect": fixture_dialect().describe(),
            "current_catalog": "fixture",
            "current_schema": "main",
            "tables": [{
                "catalog": "fixture", "schema": "main", "name": "fixture_table",
                "qualified_name": ["fixture", "main", "fixture_table"],
                "description": "Native fixture table", "description_source": "database",
                "columns": [{"name": "id", "type": "BIGINT", "description": "Native fixture column"}],
                "requested": ["fixture_table"]
            }]
        }))
    }

    fn identifier_quote(&self) -> Option<&'static str> {
        Some("\"")
    }

    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            // `same_as_source` matters for the `inbox` destination, which has these fields too.
            ConnectionField::new("path", "where the data lives").same_as_source("fixture"),
            ConnectionField::new("fail", "make `open` fail, with this message").manual(),
            ConnectionField::new("token", "secret")
                .secret()
                .same_as_source("fixture"),
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
            "flood" => {
                let n: usize = arg.parse()?;
                let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
                out.begin(schema.clone())?;
                for i in 0..n {
                    let b = RecordBatch::try_new(
                        schema.clone(),
                        vec![Arc::new(Int64Array::from_iter_values(0..65_536))],
                    )?;
                    if !out.batch(b)? {
                        break;
                    }
                    eprintln!("fixture: sent {i}");
                }
                Ok(())
            }
            "bytes" => {
                let mut err = std::io::stderr();
                err.write_all(b"fixture: \xff\xfe is not UTF-8\n")?;
                err.write_all(b"fixture: after bad bytes\n")?;
                out.no_result(None)
            }
            "orphan" => {
                std::process::Command::new(std::env::current_exe()?)
                    .env("DRE_FIXTURE_MODE", "silent")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn()?;
                out.no_result(None)
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
                dre_protocol::plugin::on_cancel(|| dre_protocol::log::warn!("fixture: cancel hook"));
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
        Ok("close_probe") => close_probe(),
        Ok("stall") => serve_format(About::new("stall", env!("CARGO_PKG_VERSION")), Stall),
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
        CAP_CATALOG,
        CAP_SESSIONS,
        CAP_READ_ONLY,
        CAP_CHECK,
    ]);
    serve_package(vec![
        Plugin::Source(about, Box::new(Fixture { opened: false })),
        Plugin::Destination(About::new("inbox", env!("CARGO_PKG_VERSION")), Box::new(Inbox)),
    ])
}

/// The `close_probe` mode: a source served frame by frame, recording a protocol `close`.
fn close_probe() -> ! {
    use dre_protocol::msg::{Envelope, Request, Response};
    let mut stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut opened = false;
    loop {
        let v = match dre_protocol::frame::read_frame(&mut stdin) {
            Ok(dre_protocol::frame::Frame::Json(v)) => v,
            Ok(dre_protocol::frame::Frame::Arrow(_)) => continue,
            // End of input (or a broken pipe): the host went away without a `close`.
            Err(_) => std::process::exit(0),
        };
        // A frame that isn't a request has no id to answer with, so it gets no reply.
        let Ok(Envelope { id, body }) = serde_json::from_value::<Envelope<Request>>(v) else {
            continue;
        };
        let reply = match body {
            Request::Hello { .. } => Response::Hello {
                protocol_version: dre_protocol::MAX_VERSION,
                kind: dre_protocol::Kind::Source,
                name: "fixture".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                capabilities: [CAP_SESSIONS, CAP_READ_ONLY, CAP_CHECK, CAP_VALIDATE]
                    .map(String::from)
                    .to_vec(),
                provides: Vec::new(),
            },
            Request::Describe {} => Response::Describe {
                format_properties: None,
                connection_fields: Fixture { opened: false }.connection_fields(),
                option_fields: Vec::new(),
                identifier_quote: Some("\"".into()),
                string_literal: None,
                sql_dialect: Some(Box::new(fixture_dialect())),
                message_limit: None,
                single_writer: None,
            },
            Request::Open { .. } => {
                opened = true;
                Response::Ok {}
            }
            Request::Execute { sql, .. } => Response::error(format!("fixture can't run `{sql}`")),
            Request::Close {} => {
                if opened && let Ok(f) = std::env::var("DRE_FIXTURE_CLOSE_FILE") {
                    let _ = std::fs::write(f, "close");
                }
                let _ = dre_protocol::frame::write_json(&mut out, &Envelope::new(id, Response::Ok {}));
                std::process::exit(0);
            }
            _ => Response::error("close_probe: unsupported request"),
        };
        let _ = dre_protocol::frame::write_json(&mut out, &Envelope::new(id, reply));
    }
}

/// The `stall` format: takes one batch, then stops reading, like a plugin stuck on a slow disk.
struct Stall;

impl Format for Stall {
    fn properties(&self) -> Option<dre_protocol::msg::FormatProperties> {
        Some(dre_protocol::msg::FormatProperties {
            extension: "stall".into(),
            layout: dre_protocol::msg::FormatLayout::MultipleTables,
        })
    }

    fn write(
        &mut self,
        _req: &WriteRequest,
        sets: &mut ResultSets<'_>,
    ) -> dre_protocol::plugin::Result<Vec<String>> {
        if let Some(mut set) = sets.next_set()? {
            set.next_batch()?;
        }
        loop {
            std::thread::park();
        }
    }
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

fn fixture_dialect() -> dre_protocol::sql::SqlDialect {
    dre_protocol::sql::SqlDialect {
        parser: dre_protocol::sql::SqlParser::Duckdb,
        identifier_quote: "\"".into(),
        identifier_case: dre_protocol::sql::IdentifierCase::Insensitive,
        literal_style: "doubling".into(),
        hex_literal: None,
        denied_functions: vec!["read_text".into(), "fixture_external".into()],
        file_formats: Vec::new(),
        read_only_notice: None,
    }
}
