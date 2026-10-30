//! A single read-only query on a configured connection, for external integrations.

use dre_protocol::host::{Execution, LogSink};
use serde_json::{Value, json};
use sqlparser::ast::{
    Expr, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor, visit_statements,
};
use sqlparser::dialect::{DatabricksDialect, DuckDbDialect, GenericDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use std::sync::Arc;

use crate::plugin_host::{Open, Plugin};
use crate::profiles::{Entry, Role};
use crate::project::{PluginKind, Project};

pub fn execute(
    ctx: &crate::Context,
    project: &Project,
    profile: &str,
    sql: &str,
    max_rows: u64,
) -> Result<Value, String> {
    if max_rows == 0 || max_rows > 100_000 {
        return Err("max_rows must be between 1 and 100000".into());
    }
    let Entry::Use(target) = project.profiles().entry(Role::Connection, profile) else {
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
    let plugin = crate::plugin_host::find_plugin(ctx, project, PluginKind::Source, &target.kind)
        .map_err(|e| e.to_string())?;
    let secrets = ctx.secrets().clone();
    let log: LogSink =
        Arc::new(move |name, message| eprintln!("{name}: {}", secrets.mask_source_error(message, false)));
    let process = ctx
        .host()
        .start(&plugin, log, Some(project.root()))
        .map_err(|e| e.to_string())?;
    // The session and its result live inside one inner call, so the single `close()` afterwards
    // runs on every error and return path after `start()`. A panic skips it: dropping the process
    // then stops it without a protocol `close`.
    run_session(
        ctx,
        process,
        target,
        sql,
        max_rows,
        profile,
        project.target_name(),
    )
}

/// The steps after the plugin has started: open a session, stream the query, build the result.
/// The plugin is closed once they return, with a result or an error.
fn run_session(
    ctx: &crate::Context,
    mut process: Box<dyn Plugin>,
    target: &crate::profiles::ProfileTarget,
    sql: &str,
    max_rows: u64,
    profile: &str,
    target_name: &str,
) -> Result<Value, String> {
    let outcome = session_result(ctx, process.as_mut(), target, sql, max_rows, profile, target_name);
    process.close();
    outcome
}

/// Every step between the running plugin and the result: the session steps of `run_session`,
/// without the close.
fn session_result(
    ctx: &crate::Context,
    process: &mut dyn Plugin,
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
    let masked = |e: dre_protocol::host::HostError| {
        ctx.secrets()
            .mask_source_error(&e.to_string(), false)
            .into_owned()
    };
    let connection = crate::profiles::render_connection(target, ctx.env())?;
    let open = if session_read_only {
        Open::Sandboxed
    } else {
        Open::ReadWrite
    };
    process.open(connection, open).map_err(masked)?;
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
                .map(|row| serde_json::to_value(row).expect("a JSON value serializes")),
        );
        Ok(())
    };
    let result = process
        .execute(sql, Some(max_rows + 1), &mut collect)
        .map_err(masked)?;
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
    use crate::plugin_host::PluginHost;
    use crate::plugin_host::memory::{Call, MemoryHost, Script};
    use dre_protocol::Kind;

    /// `run_session` on a scripted `fixture` source, against a target with no connection fields:
    /// its result, and whether the plugin was closed.
    fn run(script: Script) -> (Result<Value, String>, bool) {
        let host = MemoryHost::new().plugin(Kind::Source, "fixture", script);
        let ctx = crate::Context::new(crate::Env::default(), Arc::new(host.clone()));
        let id = dre_protocol::PluginId::new(Kind::Source, "fixture");
        let plugin = host.locate(std::path::Path::new("."), &[], &id).unwrap();
        let process = host.start(&plugin, Arc::new(|_, _| {}), None).unwrap();
        let target = crate::profiles::ProfileTarget {
            kind: "fixture".into(),
            fields: serde_json::Map::new(),
        };
        let out = run_session(&ctx, process, &target, "select 1", 10, "fx", "dev");
        let closed = host.calls().iter().any(|c| c == "source/fixture Close");
        (out, closed)
    }

    fn read_only() -> Script {
        Script::new(&[dre_protocol::CAP_READ_ONLY])
    }

    fn one_row() -> arrow::array::RecordBatch {
        let schema = Arc::new(arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
            "n",
            arrow::datatypes::DataType::Int64,
            false,
        )]));
        arrow::array::RecordBatch::try_new(schema, vec![Arc::new(arrow::array::Int64Array::from(vec![1]))])
            .unwrap()
    }

    #[test]
    fn a_failing_execute_closes_the_process_and_keeps_the_error() {
        let (out, closed) = run(read_only().fail(Call::Execute, 1, "the warehouse is down"));
        let err = out.expect_err("the failing execute must surface as an error");
        assert!(err.contains("the warehouse is down"), "unexpected error: {err}");
        assert!(closed, "the process was not closed after a failing execute");
    }

    #[test]
    fn a_query_with_no_result_set_closes_the_process() {
        let (out, closed) = run(read_only());
        assert_eq!(out.unwrap_err(), "the query returned no result set");
        assert!(closed, "the process was not closed after the no-result-set path");
    }

    #[test]
    fn a_plugin_without_read_only_is_rejected_and_still_closed() {
        let (out, closed) = run(Script::new(&[]));
        assert_eq!(
            out.unwrap_err(),
            "the fixture plugin cannot guarantee a read-only connection"
        );
        assert!(closed, "the process was not closed after the read-only rejection");
    }

    #[test]
    fn a_failed_open_is_reported_and_the_process_is_still_closed() {
        let (out, closed) = run(read_only().fail(Call::Open, 1, "bad password"));
        let err = out.expect_err("a failed open must surface as an error");
        assert!(err.contains("bad password"), "unexpected error: {err}");
        assert!(closed, "the process was not closed after a failed open");
    }

    #[test]
    fn a_succeeding_session_builds_the_result_and_closes_the_process() {
        let (out, closed) = run(read_only().rows("select 1", one_row()));
        let out = out.expect("a read-only session must succeed");
        assert_eq!(out["schema_version"], "dre/rows/v1");
        assert_eq!(out["row_count"], 1);
        assert_eq!(out["connection"], "fx");
        assert_eq!(out["target"], "dev");
        assert_eq!(out["read_only_enforcement"], "session");
        assert!(closed, "the process was not closed on the success path");
    }
}
