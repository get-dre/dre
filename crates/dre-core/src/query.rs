//! A single read-only query on a configured connection, for external integrations.

use dre_protocol::host::{Execution, LogSink};
use serde_json::{Value, json};
use sqlparser::ast::{Query, SetExpr, Statement, Visit, Visitor, visit_statements};
use sqlparser::dialect::{DatabricksDialect, DuckDbDialect, GenericDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use std::sync::Arc;

use crate::profiles::{Entry, Role};
use crate::project::{PluginKind, Project};

pub fn execute(project: &Project, profile: &str, sql: &str, max_rows: u64) -> Result<Value, String> {
    if max_rows == 0 || max_rows > 100_000 {
        return Err("max_rows must be between 1 and 100000".into());
    }
    let target = match project.profiles.entry(Role::Connection, profile) {
        Entry::Use(target) => target,
        _ => return Err(format!("connection `{profile}` has no entry for this target")),
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
    struct ReadOnly;
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
    }
    safe &= statements.visit(&mut ReadOnly).is_continue();
    if !safe {
        return Err("expected a single read-only query; nothing was run".into());
    }
    let plugin =
        crate::run::find_plugin(project, PluginKind::Source, &target.kind).map_err(|e| e.to_string())?;
    let log: LogSink =
        Arc::new(|name, message| eprintln!("{name}: {}", crate::secrets::mask_source_error(message, false)));
    let mut process = plugin
        .start(log, Some(&project.root))
        .map_err(|e| e.to_string())?;
    if !process.has(dre_protocol::CAP_READ_ONLY) {
        return Err(format!(
            "the {} plugin cannot guarantee a read-only connection",
            target.kind
        ));
    }
    process
        .open(crate::run::render_connection(target)?, true)
        .map_err(|e| crate::secrets::mask_source_error(&e.to_string(), false).into_owned())?;
    let mut rows = Vec::new();
    let mut columns = Vec::new();
    let result = process
        .execute(sql, Some(max_rows + 1), |schema, batch| {
            if columns.is_empty() {
                columns = schema
                    .fields()
                    .iter()
                    .map(|f| json!({"name": f.name(), "type": f.data_type().to_string()}))
                    .collect();
            }
            let left = (max_rows + 1).saturating_sub(rows.len() as u64) as usize;
            let batch = batch.slice(0, left.min(batch.num_rows()));
            rows.extend(
                crate::values::batch_rows(&batch)
                    .into_iter()
                    .map(|row| serde_json::to_value(row).unwrap()),
            );
            Ok(())
        })
        .map_err(|e| crate::secrets::mask_source_error(&e.to_string(), false).into_owned())?;
    if let Execution::Result { schema, .. } = result {
        if columns.is_empty() {
            columns = schema
                .fields()
                .iter()
                .map(|f| json!({"name": f.name(), "type": f.data_type().to_string()}))
                .collect();
        }
    } else {
        return Err("the query returned no result set".into());
    }
    let truncated = rows.len() as u64 > max_rows;
    rows.truncate(max_rows as usize);
    let _ = process.close();
    Ok(
        json!({"schema_version": "dre/rows/v1", "columns": columns, "row_count": rows.len(), "rows": rows,
        "truncated": truncated, "connection": profile, "target": project.target_name}),
    )
}
