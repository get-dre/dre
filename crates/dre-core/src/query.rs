//! A single read-only query on a configured connection, for external integrations.

use dre_protocol::host::{Execution, LogSink};
use serde_json::{Value, json};
use sqlparser::ast::{
    Expr, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor, visit_statements,
};
use sqlparser::dialect::{DatabricksDialect, DuckDbDialect, GenericDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use std::sync::Arc;

use crate::profiles::{Entry, Role};
use crate::project::{PluginKind, Project};

pub fn execute(project: &Project, profile: &str, sql: &str, max_rows: u64) -> Result<Value, String> {
    if max_rows == 0 || max_rows > 100_000 {
        return Err("max_rows must be between 1 and 100000".into());
    }
    let Entry::Use(target) = project.profiles.entry(Role::Connection, profile) else {
        return Err(format!("connection `{profile}` has no entry for this target"));
    };
    let dialect: Box<dyn sqlparser::dialect::Dialect> = match target.kind.as_str() {
        "duckdb" => Box::new(DuckDbDialect {}),
        "postgres" => Box::new(PostgreSqlDialect {}),
        "databricks" => Box::new(DatabricksDialect {}),
        _ => Box::new(GenericDialect {}),
    };
    let statements = Parser::parse_sql(dialect.as_ref(), sql)
        .map_err(|e| format!("expected a single read-only query: {e}"))?;
    let mut safe = statements.len() == 1;
    let _ = visit_statements(&statements, |statement| {
        if !matches!(statement, Statement::Query(_)) {
            safe = false;
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    /// Refuses anything but reads, and functions that reach files, programs or the network.
    struct ReadOnly {
        dialect: Option<&'static crate::dialect::Dialect>,
        refused: Option<String>,
    }
    impl ReadOnly {
        fn function(&mut self, name: &ObjectName) -> std::ops::ControlFlow<()> {
            let name = name.to_string();
            if self.dialect.is_some_and(|d| d.denies(&name)) {
                self.refused = Some(name);
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    impl Visitor for ReadOnly {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> std::ops::ControlFlow<()> {
            fn reads(body: &SetExpr) -> bool {
                match body {
                    SetExpr::Select(select) => select.into.is_none(),
                    SetExpr::SetOperation { left, right, .. } => reads(left) && reads(right),
                    SetExpr::Query(query) => reads(&query.body),
                    SetExpr::Values(_) | SetExpr::Table(_) => true,
                    _ => false,
                }
            }
            if !reads(&query.body) || !query.locks.is_empty() || !query.pipe_operators.is_empty() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> std::ops::ControlFlow<()> {
            match factor {
                // `FROM 'file.csv'`: a path, not a table.
                TableFactor::Table { name, .. }
                    if name
                        .0
                        .iter()
                        .any(|p| p.as_ident().is_some_and(|i| i.quote_style == Some('\''))) =>
                {
                    self.refused = Some(name.to_string());
                    std::ops::ControlFlow::Break(())
                }
                // A table function, not a table that happens to share a function's name.
                TableFactor::Table {
                    name, args: Some(_), ..
                }
                | TableFactor::Function { name, .. } => self.function(name),
                _ => std::ops::ControlFlow::Continue(()),
            }
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            match expr {
                Expr::Function(f) => self.function(&f.name),
                _ => std::ops::ControlFlow::Continue(()),
            }
        }
    }
    let mut guard = ReadOnly {
        dialect: crate::dialect::of(&target.kind),
        refused: None,
    };
    safe &= statements.visit(&mut guard).is_continue();
    if let Some(name) = guard.refused {
        return Err(format!(
            "`{name}` reads files, runs programs or reaches the network; `dre query` refuses it and nothing was run"
        ));
    }
    if !safe {
        return Err("expected a single read-only query; nothing was run".into());
    }
    let plugin =
        crate::run::find_plugin(project, PluginKind::Source, &target.kind).map_err(|e| e.to_string())?;
    let log: LogSink =
        Arc::new(|name, message| eprintln!("{name}: {}", crate::secrets::mask_source_error(message, false)));
    let process = plugin
        .start(log, Some(&project.root))
        .map_err(|e| e.to_string())?;
    // The session and its result live inside one inner call, so the single `close()` afterwards
    // runs on every error and return path after `start()`. A panic skips it: dropping the process
    // then kills it without a protocol `close`.
    run_session(process, target, sql, max_rows, profile, &project.target_name)
}

/// A plugin source running as a child process: what [`run_session`] needs of it, so a test can
/// inject a fake and observe that the process is always closed.
trait SourceProcess {
    fn has(&self, capability: &str) -> bool;
    fn open(
        &mut self,
        connection: serde_json::Map<String, Value>,
        read_only: bool,
    ) -> dre_protocol::host::Result<()>;
    fn open_sandboxed(
        &mut self,
        connection: serde_json::Map<String, Value>,
    ) -> dre_protocol::host::Result<()>;
    fn execute(
        &mut self,
        sql: &str,
        row_limit: Option<u64>,
        on_batch: &mut dyn FnMut(
            &arrow::datatypes::SchemaRef,
            arrow::array::RecordBatch,
        ) -> std::result::Result<(), String>,
    ) -> dre_protocol::host::Result<Execution>;
    fn close(self) -> dre_protocol::host::Result<()>;
}

impl SourceProcess for dre_protocol::host::PluginProcess {
    fn has(&self, capability: &str) -> bool {
        dre_protocol::host::PluginProcess::has(self, capability)
    }
    fn open(
        &mut self,
        connection: serde_json::Map<String, Value>,
        read_only: bool,
    ) -> dre_protocol::host::Result<()> {
        dre_protocol::host::PluginProcess::open(self, connection, read_only)
    }
    fn open_sandboxed(
        &mut self,
        connection: serde_json::Map<String, Value>,
    ) -> dre_protocol::host::Result<()> {
        dre_protocol::host::PluginProcess::open_sandboxed(self, connection)
    }
    fn execute(
        &mut self,
        sql: &str,
        row_limit: Option<u64>,
        on_batch: &mut dyn FnMut(
            &arrow::datatypes::SchemaRef,
            arrow::array::RecordBatch,
        ) -> std::result::Result<(), String>,
    ) -> dre_protocol::host::Result<Execution> {
        dre_protocol::host::PluginProcess::execute(self, sql, row_limit, on_batch)
    }
    fn close(self) -> dre_protocol::host::Result<()> {
        dre_protocol::host::PluginProcess::close(self)
    }
}

/// The steps after the plugin process has started: open a session, stream the query, build the
/// result. The process is closed once they return, with a result or an error.
fn run_session<P: SourceProcess>(
    mut process: P,
    target: &crate::profiles::ProfileTarget,
    sql: &str,
    max_rows: u64,
    profile: &str,
    target_name: &str,
) -> Result<Value, String> {
    let outcome = session_result(&mut process, target, sql, max_rows, profile, target_name);
    let _ = process.close();
    outcome
}

/// Every step between the running process and the result: the session steps of `run_session`,
/// without the close.
fn session_result<P: SourceProcess>(
    process: &mut P,
    target: &crate::profiles::ProfileTarget,
    sql: &str,
    max_rows: u64,
    profile: &str,
    target_name: &str,
) -> Result<Value, String> {
    let session_read_only = process.has(dre_protocol::CAP_READ_ONLY);
    // Databricks has no read-only session capability. Its single-SELECT guard is
    // enforced here; database permissions remain the user's enforcement boundary.
    if !session_read_only && target.kind != "databricks" {
        return Err(format!(
            "the {} plugin cannot guarantee a read-only connection",
            target.kind
        ));
    }
    let connection = crate::run::render_connection(target)?;
    if session_read_only {
        process.open_sandboxed(connection)
    } else {
        process.open(connection, false)
    }
    .map_err(|e| crate::secrets::mask_source_error(&e.to_string(), false).into_owned())?;
    let mut rows = Vec::new();
    let mut columns = Vec::new();
    let mut collect = |schema: &arrow::datatypes::SchemaRef, batch: arrow::array::RecordBatch| {
        if columns.is_empty() {
            columns = crate::rows::column_types(schema);
        }
        let left = (max_rows + 1).saturating_sub(rows.len() as u64) as usize;
        let batch = batch.slice(0, left.min(batch.num_rows()));
        rows.extend(
            crate::values::batch_rows(&batch)
                .into_iter()
                .map(|row| serde_json::to_value(row).unwrap()),
        );
        Ok(())
    };
    let result = process
        .execute(sql, Some(max_rows + 1), &mut collect)
        .map_err(|e| crate::secrets::mask_source_error(&e.to_string(), false).into_owned())?;
    if let Execution::Result { schema, .. } = result {
        if columns.is_empty() {
            columns = crate::rows::column_types(&schema);
        }
    } else {
        return Err("the query returned no result set".into());
    }
    let truncated = rows.len() as u64 > max_rows;
    rows.truncate(max_rows as usize);
    Ok(
        json!({"schema_version": "dre/rows/v1", "columns": columns, "row_count": rows.len(), "rows": rows,
        "truncated": truncated, "read_only_enforcement": if session_read_only {"session"} else {"SQL validation; use a read-only Databricks database role"}, "connection": profile, "target": target_name}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// What a fake source process does in a session.
    #[derive(Clone, Copy)]
    enum Scenario {
        /// Doesn't advertise a read-only session, so the capability guard rejects it.
        NotReadOnly,
        FailOpen,
        FailExecute,
        /// `execute` returns no result set.
        NoResult,
        Ok,
    }

    struct FakeSource {
        scenario: Scenario,
        /// Set when `close()` runs, so a test can prove the process was closed.
        closed: Rc<Cell<bool>>,
    }

    impl SourceProcess for FakeSource {
        fn has(&self, _: &str) -> bool {
            !matches!(self.scenario, Scenario::NotReadOnly)
        }
        fn open(
            &mut self,
            _connection: serde_json::Map<String, Value>,
            _read_only: bool,
        ) -> dre_protocol::host::Result<()> {
            if let Scenario::FailOpen = self.scenario {
                return Err(dre_protocol::host::HostError::Unexpected {
                    plugin: "fake".into(),
                    expected: "an open reply",
                    got: "an error".into(),
                });
            }
            Ok(())
        }
        fn open_sandboxed(
            &mut self,
            connection: serde_json::Map<String, Value>,
        ) -> dre_protocol::host::Result<()> {
            self.open(connection, true)
        }
        fn execute(
            &mut self,
            _sql: &str,
            _row_limit: Option<u64>,
            _on_batch: &mut dyn FnMut(
                &arrow::datatypes::SchemaRef,
                arrow::array::RecordBatch,
            ) -> std::result::Result<(), String>,
        ) -> dre_protocol::host::Result<Execution> {
            match self.scenario {
                Scenario::FailExecute => Err(dre_protocol::host::HostError::Unexpected {
                    plugin: "fake".into(),
                    expected: "an execute reply",
                    got: "an error".into(),
                }),
                Scenario::NoResult => Ok(Execution::NoResult { rows_affected: None }),
                _ => {
                    let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![
                        arrow::datatypes::Field::new("n", arrow::datatypes::DataType::Int64, false),
                    ]));
                    Ok(Execution::Result { schema, rows: 0 })
                }
            }
        }
        fn close(self) -> dre_protocol::host::Result<()> {
            self.closed.set(true);
            Ok(())
        }
    }

    /// `run_session` on a fake source in `scenario`, against a `fixture` target with no
    /// connection fields: its result, and whether the process was closed.
    fn run(scenario: Scenario) -> (Result<Value, String>, bool) {
        let closed = Rc::new(Cell::new(false));
        let fake = FakeSource {
            scenario,
            closed: closed.clone(),
        };
        let target = crate::profiles::ProfileTarget {
            kind: "fixture".into(),
            fields: serde_json::Map::new(),
        };
        let out = run_session(fake, &target, "select 1", 10, "fx", "dev");
        (out, closed.get())
    }

    #[test]
    fn a_failing_execute_closes_the_process_and_keeps_the_error() {
        let (out, closed) = run(Scenario::FailExecute);
        let err = out.expect_err("the failing execute must surface as an error");
        assert!(err.contains("an execute reply"), "unexpected error: {err}");
        assert!(closed, "the process was not closed after a failing execute");
    }

    #[test]
    fn a_query_with_no_result_set_closes_the_process() {
        let (out, closed) = run(Scenario::NoResult);
        assert_eq!(out.unwrap_err(), "the query returned no result set");
        assert!(closed, "the process was not closed after the no-result-set path");
    }

    #[test]
    fn a_plugin_without_read_only_is_rejected_and_still_closed() {
        let (out, closed) = run(Scenario::NotReadOnly);
        assert_eq!(
            out.unwrap_err(),
            "the fixture plugin cannot guarantee a read-only connection"
        );
        assert!(closed, "the process was not closed after the read-only rejection");
    }

    #[test]
    fn a_failed_open_is_reported_and_the_process_is_still_closed() {
        let (out, closed) = run(Scenario::FailOpen);
        let err = out.expect_err("a failed open must surface as an error");
        assert!(err.contains("an open reply"), "unexpected error: {err}");
        assert!(closed, "the process was not closed after a failed open");
    }

    #[test]
    fn a_succeeding_session_builds_the_result_and_closes_the_process() {
        let (out, closed) = run(Scenario::Ok);
        let out = out.expect("a read-only session must succeed");
        assert_eq!(out["schema_version"], "dre/rows/v1");
        assert_eq!(out["row_count"], 0);
        assert_eq!(out["connection"], "fx");
        assert_eq!(out["target"], "dev");
        assert_eq!(out["read_only_enforcement"], "session");
        assert!(closed, "the process was not closed on the success path");
    }
}
