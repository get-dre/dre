//! DRE source plugin for DuckDB.
//!
//! Profile target fields: `path` (database file; default `:memory:`, relative to the project
//! directory), `threads`, `memory_limit`. One connection is held for the whole Binding, so temp
//! tables and settings persist across statements.

mod bridge;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use arrow::datatypes::{DataType, Schema};
use dre_protocol::msg::{ConnectionField, FieldKind};
use dre_protocol::plugin::{About, Loaded, Result, ResultSet, ResultSink, Source, conn_str, serve_source};
use dre_protocol::{CAP_CHECK, CAP_LOAD, CAP_READ_ONLY, CAP_SESSIONS};
use duckdb::{AccessMode, Config, Connection, InterruptHandle};
use serde_json::{Map, Value};

struct DuckDb {
    session: Option<Session>,
    /// The SDK's `cancelled`; tests replace it.
    cancelled: fn() -> bool,
}

impl Default for DuckDb {
    fn default() -> Self {
        Self {
            session: None,
            cancelled: dre_protocol::plugin::cancelled,
        }
    }
}

/// The open connection, and what stops a statement on it when core cancels.
struct Session {
    conn: Connection,
    interrupter: Arc<Interrupter>,
}

/// How often a cancelled statement is interrupted again until it ends.
const REARM: Duration = Duration::from_millis(5);

/// Stops the statement running on one connection when core cancels its request.
///
/// DuckDB clears a pending interrupt whenever it starts a statement, so an interrupt sent just
/// before (between the SDK's "cancelled before it started" check and DuckDB's start) is lost.
/// While the request is cancelled, a watcher keeps interrupting until the statement ends.
struct Interrupter {
    handle: Arc<InterruptHandle>,
    /// Whether core has cancelled the request running now; sticky for that request.
    cancelled: fn() -> bool,
    /// Whether a statement is running: the watcher interrupts only then.
    running: Mutex<bool>,
    wake: Condvar,
}

impl Interrupter {
    fn new(handle: Arc<InterruptHandle>, cancelled: fn() -> bool) -> Self {
        Self {
            handle,
            cancelled,
            running: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    /// Core cancelled the request: the SDK's cancel hook, run on its stdin reader thread after
    /// it marked the request cancelled. Wakes the watcher, which interrupts the statement.
    fn cancel(&self) {
        let _running = self.running.lock().unwrap();
        self.wake.notify_all();
    }

    /// Run one statement on the connection, with a watcher that interrupts it for as long as
    /// the request is cancelled. The watcher has stopped by the time this returns, so no
    /// interrupt reaches a later statement.
    fn run<T>(&self, statement: impl FnOnce() -> T) -> T {
        *self.running.lock().unwrap() = true;
        std::thread::scope(|s| {
            s.spawn(|| self.watch());
            let _end = EndWatch(self);
            statement()
        })
    }

    /// The statement ended: stop the watcher.
    fn end(&self) {
        *self.running.lock().unwrap() = false;
        self.wake.notify_all();
    }

    fn watch(&self) {
        let mut running = self.running.lock().unwrap();
        while *running {
            running = if (self.cancelled)() {
                self.handle.interrupt();
                self.wake.wait_timeout(running, REARM).unwrap().0
            } else {
                self.wake.wait(running).unwrap()
            };
        }
    }
}

/// Ends the watch when the statement returns or panics.
struct EndWatch<'a>(&'a Interrupter);

impl Drop for EndWatch<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Leading keywords of statements that produce a result set even when it looks like a row count.
const QUERY_KEYWORDS: &[&str] = &[
    "select",
    "with",
    "from",
    "values",
    "table",
    "show",
    "describe",
    "summarize",
    "explain",
    "pragma",
    "call",
    "(",
];

/// Lookup names come from core already checked; refuse anything that isn't a plain identifier.
fn checked_name(name: &str) -> Result<&str> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name)
    } else {
        Err(format!("`{name}` isn't a valid table name").into())
    }
}

fn leading_keyword(sql: &str) -> String {
    let mut s = sql.trim_start();
    loop {
        if let Some(r) = s.strip_prefix("--") {
            s = r.find('\n').map_or("", |i| &r[i..]).trim_start();
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |i| &r[i + 2..]).trim_start();
        } else {
            break;
        }
    }
    if s.starts_with('(') {
        return "(".into();
    }
    s.split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// What DuckDB returns for statements without a result set: DDL, DML, CTAS and COPY answer with
/// one `Count` column; SET, DROP, BEGIN, ATTACH and the like with one `Success` column.
#[derive(PartialEq)]
enum Status {
    Count,
    Success,
}

fn status_only(schema: &Schema, sql: &str) -> Option<Status> {
    if schema.fields().len() != 1 || QUERY_KEYWORDS.contains(&leading_keyword(sql).as_str()) {
        return None;
    }
    match (schema.field(0).name().as_str(), schema.field(0).data_type()) {
        ("Count", DataType::Int64) => Some(Status::Count),
        ("Success", DataType::Boolean) => Some(Status::Success),
        _ => None,
    }
}

impl DuckDb {
    fn conn(&self) -> Result<&Connection> {
        Ok(&self.session()?.conn)
    }
    fn session(&self) -> Result<&Session> {
        self.session.as_ref().ok_or_else(|| "no open session".into())
    }
    fn open_with(&mut self, c: &Map<String, Value>, read_only: bool, sandbox: bool) -> Result<()> {
        let path = conn_str(c, "path").unwrap_or(":memory:");
        let mut config = Config::default();
        if read_only && path != ":memory:" {
            config = config.access_mode(AccessMode::ReadOnly)?;
        }
        if sandbox {
            config = config
                .enable_external_access(false)?
                .with("autoinstall_known_extensions", "false")?
                .with("autoload_known_extensions", "false")?;
        }
        if let Some(t) = c
            .get("threads")
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        {
            config = config.threads(t)?;
        }
        if let Some(m) = conn_str(c, "memory_limit") {
            config = config.max_memory(m)?;
        }
        let conn = Connection::open_with_flags(path, config)
            .map_err(|e| format!("can't open DuckDB database `{path}`: {e}"))?;
        let interrupter = Arc::new(Interrupter::new(conn.interrupt_handle(), self.cancelled));
        let hook = Arc::clone(&interrupter);
        dre_protocol::plugin::on_cancel(move || hook.cancel());
        if sandbox {
            conn.execute_batch("SET lock_configuration = true")?;
        }
        self.session = Some(Session { conn, interrupter });
        Ok(())
    }
}

impl Source for DuckDb {
    fn identifier_quote(&self) -> Option<&'static str> {
        Some("\"")
    }

    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new(
                "path",
                "DuckDB database file (relative to the project), or :memory:",
            )
            .default(":memory:"),
            ConnectionField::new("threads", "number of threads DuckDB may use").kind(FieldKind::Integer),
            ConnectionField::new("memory_limit", "DuckDB memory limit, e.g. 4GB"),
        ]
    }

    fn open(&mut self, c: &Map<String, Value>, read_only: bool) -> Result<()> {
        self.open_with(c, read_only, false)
    }

    /// `dre query`: read-only, no files or network outside the database, settings locked so
    /// the query can't turn that back on.
    fn open_sandboxed(&mut self, c: &Map<String, Value>) -> Result<()> {
        self.open_with(c, true, true)
    }

    /// Bulk load through DuckDB's appender into a temp table.
    fn load(&mut self, name: &str, data: &mut ResultSet<'_>) -> Result<Loaded> {
        let table = format!("dre_lookup_{}", checked_name(name)?);
        let columns = data
            .schema
            .fields()
            .iter()
            .map(|f| {
                let t = match f.data_type() {
                    DataType::Utf8 => "VARCHAR",
                    DataType::Int64 => "BIGINT",
                    DataType::Float64 => "DOUBLE",
                    DataType::Boolean => "BOOLEAN",
                    DataType::Date32 => "DATE",
                    other => return Err(format!("can't load a column of type {other}")),
                };
                Ok(format!("\"{}\" {t}", f.name()))
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let conn = self.conn()?;
        conn.execute_batch(&format!(
            "create or replace temp table {table} ({})",
            columns.join(", ")
        ))?;
        let mut appender = conn.appender(&table)?;
        let mut rows = 0;
        while let Some(batch) = data.next_batch()? {
            rows += batch.num_rows() as u64;
            appender.append_record_batch(bridge::to_duckdb(&batch)?)?;
        }
        appender.flush()?;
        Ok(Loaded {
            relation: table,
            rows,
            warning: None,
        })
    }

    fn execute(&mut self, sql: &str, _row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()> {
        let Session { conn, interrupter } = self.session()?;
        interrupter.run(|| execute(conn, sql, out))
    }

    fn check(&mut self, sql: &str) -> Result<()> {
        let Session { conn, interrupter } = self.session()?;
        interrupter.run(|| {
            let mut stmt = conn.prepare(&format!("EXPLAIN {sql}"))?;
            stmt.execute([])?;
            Ok(())
        })
    }
}

/// Run `sql` and send its result to `out`.
fn execute(conn: &Connection, sql: &str, out: &mut dyn ResultSink) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let stream = stmt.stream_arrow([])?;
    let schema = bridge::schema_from_duckdb(&stream.get_schema())?;
    if schema.fields().is_empty() {
        return out.no_result(None);
    }
    let mut stream = stream;
    match status_only(&schema, sql) {
        Some(Status::Count) => {
            let n = next_batch(&mut stream)?
                .and_then(|b| {
                    b.column(0)
                        .as_any()
                        .downcast_ref::<duckdb::arrow::array::Int64Array>()
                        .map(|a| a.value(0))
                })
                .map(|n| n as u64);
            return out.no_result(n);
        }
        Some(Status::Success) => {
            while next_batch(&mut stream)?.is_some() {}
            return out.no_result(None);
        }
        None => {}
    }
    out.begin(Arc::new(schema))?;
    while let Some(b) = next_batch(&mut stream)? {
        if !out.batch(bridge::from_duckdb(&b)?)? {
            break;
        }
    }
    Ok(())
}

/// duckdb-rs panics if fetching fails mid-stream; turn that into an error.
fn next_batch(stream: &mut duckdb::ArrowStream<'_>) -> Result<Option<duckdb::arrow::array::RecordBatch>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stream.next())).map_err(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
        format!(
            "DuckDB failed while fetching results: {}",
            msg.unwrap_or_default()
        )
        .into()
    })
}

fn main() {
    let about = About::new("duckdb", env!("CARGO_PKG_VERSION")).capabilities(&[
        CAP_SESSIONS,
        CAP_READ_ONLY,
        CAP_CHECK,
        CAP_LOAD,
    ]);
    serve_source(about, DuckDb::default())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct Discard;

    impl ResultSink for Discard {
        fn no_result(&mut self, _: Option<u64>) -> Result<()> {
            Ok(())
        }
        fn begin(&mut self, _: arrow::datatypes::SchemaRef) -> Result<()> {
            Ok(())
        }
        fn batch(&mut self, _: arrow::array::RecordBatch) -> Result<bool> {
            Ok(true)
        }
    }

    const SLOW: &str = "select sum(a.range * b.range) from range(1000000) a, range(1000000) b";

    /// Run `statement` on another thread; its result, or `None` if it still runs after 20 s.
    fn finishes<T: Send + 'static>(statement: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(statement()));
        finished.recv_timeout(Duration::from_secs(20)).ok()
    }

    fn assert_interrupted(result: Option<Result<()>>) {
        let err = result.expect("the cancelled statement ran on").unwrap_err();
        assert!(err.to_string().contains("Interrupted"), "{err}");
    }

    #[test]
    fn a_cancel_that_lands_before_duckdb_starts_the_statement_still_stops_it() {
        // The request is running and the SDK's "cancelled before it started" check has passed.
        // Then the cancel lands and the hook runs, before DuckDB starts the statement: DuckDB
        // clears any interrupt when it starts one.
        static CANCELLED: AtomicBool = AtomicBool::new(false);
        let mut db = DuckDb {
            cancelled: || CANCELLED.load(Ordering::SeqCst),
            ..DuckDb::default()
        };
        db.open(&Map::new(), false).unwrap();
        assert_interrupted(finishes(move || {
            let Session { conn, interrupter } = db.session().unwrap();
            interrupter.run(|| {
                CANCELLED.store(true, Ordering::SeqCst);
                interrupter.cancel();
                execute(conn, SLOW, &mut Discard)
            })
        }));
    }

    #[test]
    fn a_request_cancelled_before_the_statement_runs_is_stopped() {
        let mut db = DuckDb {
            cancelled: || true,
            ..DuckDb::default()
        };
        db.open(&Map::new(), false).unwrap();
        assert_interrupted(finishes(move || db.execute(SLOW, None, &mut Discard)));
    }

    #[test]
    fn finds_the_leading_keyword_past_comments() {
        assert_eq!(leading_keyword("  -- c\n /* x */ Insert into t"), "insert");
        assert_eq!(leading_keyword("(select 1)"), "(");
    }
}
