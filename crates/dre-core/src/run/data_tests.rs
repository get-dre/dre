//! Local checks of immutable Arrow spools, using a disposable core-owned DuckDB database.

use super::session::{Session, masked_source_error, protected_sql};
use dre_protocol::{CAP_SESSIONS, host::Execution};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow::datatypes::{DataType, TimeUnit};
use arrow::ipc::reader::FileReader;
use minijinja::Value;
use serde_json::json;

use super::{BindingRun, Fail, Statement};
use crate::codes::Code;
use crate::data_tests::{DataTest, Outcome, Status};
use crate::engine::{CancelToken, Level, RunEvent};
use crate::render::{Mode, Renderer, RendererConfig};

#[path = "data_tests_bridge.rs"]
mod bridge;

impl BindingRun<'_> {
    /// Check the final result of this query before advancing to the next query.
    pub(super) fn test_result(&mut self, st: &Statement, session: &Arc<Mutex<Session>>) -> Result<(), Fail> {
        if !self.stages.tests || !st.last {
            return Ok(());
        }
        let tests = self.b.queries[st.query_index].data_tests.clone();
        if tests.is_empty() {
            return Ok(());
        }
        let mut failed = false;
        for (ordinal, test) in tests.into_iter().enumerate() {
            let mut evidence = Evidence::new(
                self.run_dir
                    .join("test_failures")
                    .join(format!(
                        "{}-{}",
                        st.query_index + 1,
                        crate::for_each::safe_name(&st.query)
                    ))
                    .join(format!(
                        "{}-{}.csv",
                        ordinal + 1,
                        crate::for_each::safe_name(&test.name)
                    )),
                &test,
            );
            self.not_cancelled()?;
            let started = Instant::now();
            let checked = if test.config.execution == "source" {
                self.source_test(st, session, &test, &mut evidence)
            } else {
                self.local_test(&st.query, &test, &mut evidence)
            };
            let (status, failures, error) = match &checked {
                Ok(0) => (Status::Pass, Some(0), None),
                Ok(n) if test.config.severity == "warn" => (Status::Warn, Some(*n), None),
                Ok(n) => {
                    failed = true;
                    (Status::Fail, Some(*n), None)
                }
                Err(e) => (Status::Error, None, Some(self.ctx.secrets().mask(e).into_owned())),
            };
            let retained = evidence.writer.is_some();
            let path = retained.then(|| evidence.path.display().to_string());
            let truncated = failures.map(|n| n > evidence.rows);
            let elapsed = started.elapsed();
            let mut detail = match (&failures, &error) {
                (Some(n), _) => format!("{n} failure rows"),
                (_, Some(e)) => e.clone(),
                _ => String::new(),
            };
            if let Some(path) = &path {
                if failures.is_some() {
                    detail.push_str(&format!(
                        "; {} examples: {path}{}",
                        evidence.rows,
                        if truncated == Some(true) {
                            " (truncated)"
                        } else {
                            ""
                        }
                    ));
                } else {
                    detail.push_str(&format!("; partial evidence: {path}"));
                }
            }
            self.ui.step(
                Level::Info,
                match status {
                    Status::Pass => "PASS",
                    Status::Fail => "FAIL",
                    Status::Warn => "WARN",
                    Status::Error => "ERROR",
                    Status::Skipped => "SKIP",
                },
                &format!(
                    "{}: query `{}` test `{}` #{} ({}, {}): {detail}",
                    self.label(),
                    st.query,
                    test.name,
                    ordinal + 1,
                    test.config.execution,
                    test.config.severity
                ),
                Some(elapsed),
            );
            self.record_test(Outcome {
                query: st.query.clone(),
                test: test.name.clone(),
                query_ordinal: Some(st.query_index + 1),
                ordinal: Some(ordinal + 1),
                skip_reason: None,
                execution: test.config.execution.clone(),
                severity: test.config.severity.clone(),
                status,
                failures,
                error: error.clone(),
                failure_path: path,
                stored_failures: failures.and_then(|_| retained.then_some(evidence.rows)),
                failures_truncated: retained.then_some(truncated).flatten(),
                duration_ms: elapsed.as_millis() as u64,
            });
            if let Some(error) = error {
                return Err(Fail::new(
                    Code::DataTestError,
                    format!("query `{}` test `{}`: {error}", st.query, test.name),
                ));
            }
        }
        if failed {
            return Err(Fail::new(
                Code::DataTestFailed,
                format!(
                    "query `{}` has failed data tests; later queries and report outputs were skipped",
                    st.query
                ),
            ));
        }
        Ok(())
    }

    /// Preserve unstarted declarations even when a broken check stops its own query immediately.
    pub(super) fn skip_after_test_failure(&mut self, reason: &str) {
        let Some(last) = self.tests.last() else { return };
        let Some(query_index) = last.query_ordinal.and_then(|n| n.checked_sub(1)) else {
            return;
        };
        let completed = last.ordinal.unwrap_or(0);
        let mut skipped = Vec::new();
        for (index, query) in self.b.queries.iter().enumerate().skip(query_index) {
            if index > query_index {
                self.skipped_queries.push(query.query.clone());
                self.ui.step(
                    Level::Info,
                    "SKIP",
                    &format!(
                        "{}: query `{}`: an earlier data test stopped this Binding",
                        self.label(),
                        query.query
                    ),
                    None,
                );
            }
            for (ordinal, test) in query.data_tests.iter().enumerate() {
                if index == query_index && ordinal < completed {
                    continue;
                }
                skipped.push(Outcome {
                    query: query.query.clone(),
                    test: test.name.clone(),
                    query_ordinal: Some(index + 1),
                    ordinal: Some(ordinal + 1),
                    skip_reason: Some(reason.to_string()),
                    execution: test.config.execution.clone(),
                    severity: test.config.severity.clone(),
                    status: Status::Skipped,
                    failures: None,
                    error: None,
                    failure_path: None,
                    stored_failures: None,
                    failures_truncated: None,
                    duration_ms: 0,
                });
            }
        }
        for outcome in skipped {
            self.ui.step(
                Level::Info,
                "SKIP",
                &format!(
                    "{}: query `{}` test `{}` #{} ({}, {}): an earlier data test stopped this Binding",
                    self.label(),
                    outcome.query,
                    outcome.test,
                    outcome.ordinal.unwrap_or(0),
                    outcome.execution,
                    outcome.severity
                ),
                None,
            );
            self.record_test(outcome);
        }
    }

    fn record_test(&mut self, outcome: Outcome) {
        self.ui.emit(RunEvent::DataTest {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            binding: self.b.dir_name().to_string(),
            outcome: outcome.clone(),
        });
        self.tests.push(outcome);
    }

    fn source_test(
        &mut self,
        st: &Statement,
        session: &Arc<Mutex<Session>>,
        test: &DataTest,
        evidence: &mut Evidence,
    ) -> Result<u64, String> {
        {
            let mut source = session.lock().unwrap();
            if !source.get()?.has(CAP_SESSIONS) {
                return Err(format!(
                    "source data tests require the source plugin for connection `{}` to support sessions",
                    st.connection
                ));
            }
        }
        let renderer = self.renderer(Some(&st.connection)).map_err(|e| e.message)?;
        let sql = renderer
            .render_with(
                &self.report.file,
                &test.invocation(),
                Value::from(minijinja::value::Serde(json!({
                    "__dre_relation": test.arguments["relation"], "__dre_arguments": test.arguments
                }))),
                false,
            )
            .map_err(|e| e.to_string())?;
        let statements = crate::sqlsplit::split(&sql);
        if statements.len() != 1
            || crate::sqlsplit::classify(&statements[0].text) != crate::sqlsplit::StatementKind::Read
        {
            return Err("a data test must return failure rows with one SELECT/WITH statement".into());
        }
        let sensitive = st.sensitive || self.ctx.secrets().contains_secret(&sql);
        self.ui.sql_log()(
            &format!("{}: test {}", st.file.display(), test.name),
            &protected_sql(self.ctx.secrets(), &sql, sensitive),
        );
        let mut source = session.lock().unwrap();
        let (plugin, _scope) = source.statement(sensitive)?;
        match plugin
            .execute(&sql, None, &mut |_, batch| evidence.write(&batch))
            .map_err(|e| masked_source_error(self.ctx.secrets(), &e, sensitive))?
        {
            Execution::Result { rows, .. } => Ok(rows),
            Execution::NoResult { .. } => {
                Err("a data test must return failure rows, but the source returned no result set".into())
            }
        }
    }

    fn local_test(&self, query: &str, test: &DataTest, evidence: &mut Evidence) -> Result<u64, String> {
        // Keeping this under the existing spool directory gives success, failure and cancel
        // the same cleanup as report results, while DuckDB can spill without retaining rows.
        let workspace = tempfile::tempdir_in(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        let conn = local_connection(workspace.path())?;
        let handle = conn.interrupt_handle();
        let finished = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let watcher = scope.spawn(|| {
                while !finished.load(Ordering::Acquire) {
                    if self.opts.cancel.is_cancelled() {
                        handle.interrupt();
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                }
            });
            let _finish = FinishWatch {
                finished: &finished,
                thread: watcher.thread().clone(),
            };
            let result_spool = |query: &str| {
                self.test_results.get(query).ok_or_else(|| format!(
                "query `{query}` returned no dataset for local testing; use a source test with an explicit relation, or an explicit result-producing SELECT"
            ))
            };
            load_result(&conn, result_spool(query)?, "dre_test_result", &self.opts.cancel)?;
            let mut arguments = test.arguments.clone();
            for (index, (argument, referenced)) in test.query_results.iter().enumerate() {
                let relation = format!("dre_test_reference_{index}");
                load_result(&conn, result_spool(referenced)?, &relation, &self.opts.cancel)?;
                arguments.insert(argument.clone(), json!(format!("\"{relation}\"")));
            }
            let renderer = Renderer::new(RendererConfig {
                root: self.project.root(),
                project_name: self.project.name(),
                templates: self.project.templates(),
                context: self.run_context(),
                vars: self.vars.clone(),
                cli_vars: self.opts.vars.clone(),
                runner: None,
                connections: None,
                mode: Mode::Run,
                connection: None,
                source_type: "duckdb".into(),
                sources: None,
                process: self.ctx.clone(),
            })
            .map_err(|e| e.to_string())?;
            let sql = renderer
                .render_with(
                    &self.report.file,
                    &test.invocation(),
                    Value::from(minijinja::value::Serde(
                        json!({"__dre_relation": "\"dre_test_result\"", "__dre_arguments": arguments}),
                    )),
                    false,
                )
                .map_err(|e| e.to_string())?;
            let statements = crate::sqlsplit::split(&sql);
            if statements.len() != 1
                || crate::sqlsplit::classify(&statements[0].text) != crate::sqlsplit::StatementKind::Read
            {
                return Err("a data test must return failure rows with one SELECT/WITH statement".into());
            }
            if test.config.store_failures {
                // Materialize the check once. Count all rows, then serialize only capped examples.
                conn.execute_batch(&format!(
                    "create temp table dre_test_failures as {}",
                    statements[0].text
                ))
                .map_err(|e| e.to_string())?;
                let count: u64 = conn
                    .query_row("select count(*) from dre_test_failures", [], |row| row.get(0))
                    .map_err(|e| e.to_string())?;
                let mut stmt = conn
                    .prepare(&format!(
                        "select * from dre_test_failures limit {}",
                        test.config.failure_limit
                    ))
                    .map_err(|e| e.to_string())?;
                // Use fallible steps: DuckDB's Arrow iterator panics on fetch/conversion errors.
                drop(stmt.stream_arrow([]).map_err(|e| e.to_string())?);
                while let Some(array) = stmt.step().map_err(|e| e.to_string())? {
                    let batch = duckdb::arrow::array::RecordBatch::from(array);
                    evidence.write(&bridge::from_duckdb(&batch)?)?;
                }
                return Ok(count);
            }
            let mut stmt = conn.prepare(&statements[0].text).map_err(|e| e.to_string())?;
            let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
            let mut count = 0;
            while rows.next().map_err(|e| e.to_string())?.is_some() {
                count += 1;
            }
            Ok(count)
        })
    }
}

/// One capped diagnostic sink shared by both engines; no output/delivery registration.
struct Evidence {
    path: PathBuf,
    limit: u64,
    rows: u64,
    writer: Option<arrow::csv::Writer<File>>,
}

impl Evidence {
    fn new(path: PathBuf, test: &DataTest) -> Self {
        Self {
            path,
            limit: if test.config.store_failures {
                test.config.failure_limit
            } else {
                0
            },
            rows: 0,
            writer: None,
        }
    }

    fn write(&mut self, batch: &arrow::array::RecordBatch) -> Result<(), String> {
        let take = (self.limit - self.rows).min(batch.num_rows() as u64) as usize;
        if take == 0 {
            return Ok(());
        }
        if self.writer.is_none() {
            let parent = self.path.parent().ok_or("failure evidence path has no parent")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let file = File::create_new(&self.path)
                .map_err(|e| format!("cannot create {}: {e}", self.path.display()))?;
            self.writer = Some(arrow::csv::WriterBuilder::new().with_header(true).build(file));
        }
        let writer = self.writer.as_mut().expect("writer was opened above");
        writer
            .write(&batch.slice(0, take))
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))?;
        self.rows += take as u64;
        Ok(())
    }
}

// Configure DuckDB memory and spill independently. This is not a process RSS limit:
// Arrow batches and allocations outside DuckDB's buffer manager also occupy memory.
fn local_connection(workspace: &Path) -> Result<duckdb::Connection, String> {
    let spill = workspace.join("spill");
    let spill = spill.to_str().ok_or("local test spill path is not UTF-8")?;
    let config = duckdb::Config::default()
        .max_memory("512MB")
        .and_then(|c| c.threads(1))
        .and_then(|c| c.with("temp_directory", spill))
        .and_then(|c| c.with("max_temp_directory_size", "2GB"))
        .map_err(|e| e.to_string())?;
    duckdb::Connection::open_with_flags(workspace.join("tests.duckdb"), config).map_err(|e| e.to_string())
}

fn load_result(
    conn: &duckdb::Connection,
    spool: &Path,
    relation: &str,
    cancel: &CancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("local data test cancelled".into());
    }
    let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let columns: Result<Vec<String>, String> = reader
        .schema()
        .fields()
        .iter()
        .map(|field| {
            if field
                .metadata()
                .keys()
                .any(|key| key.starts_with("ARROW:extension:"))
            {
                return Err(format!(
                    "local data tests cannot load column `{}` with Arrow extension metadata",
                    field.name()
                ));
            }
            let sql_type = match field.data_type() {
                DataType::Boolean => "BOOLEAN".into(),
                DataType::Int8 => "TINYINT".into(),
                DataType::Int16 => "SMALLINT".into(),
                DataType::Int32 => "INTEGER".into(),
                DataType::Int64 => "BIGINT".into(),
                DataType::UInt8 => "UTINYINT".into(),
                DataType::UInt16 => "USMALLINT".into(),
                DataType::UInt32 => "UINTEGER".into(),
                DataType::UInt64 => "UBIGINT".into(),
                DataType::Float32 => "FLOAT".into(),
                DataType::Float64 => "DOUBLE".into(),
                DataType::Utf8 | DataType::LargeUtf8 => "VARCHAR".into(),
                DataType::Binary | DataType::LargeBinary => "BLOB".into(),
                DataType::Date32 => "DATE".into(),
                DataType::Timestamp(TimeUnit::Second, None) => "TIMESTAMP_S".into(),
                DataType::Timestamp(TimeUnit::Millisecond, None) => "TIMESTAMP_MS".into(),
                DataType::Timestamp(TimeUnit::Microsecond, None) => "TIMESTAMP".into(),
                DataType::Timestamp(TimeUnit::Nanosecond, None) => "TIMESTAMP_NS".into(),
                DataType::Decimal128(precision, scale) if *precision <= 38 && *scale >= 0 => {
                    format!("DECIMAL({precision}, {scale})")
                }
                other => {
                    return Err(format!(
                        "local data tests cannot load column `{}` of Arrow type {other}",
                        field.name()
                    ));
                }
            };
            Ok(format!("\"{}\" {sql_type}", field.name().replace('"', "\"\"")))
        })
        .collect();
    conn.execute_batch(&format!(
        "create table \"{}\" ({})",
        relation.replace('"', "\"\""),
        columns?.join(", ")
    ))
    .map_err(|e| e.to_string())?;
    let mut appender = conn.appender(relation).map_err(|e| e.to_string())?;
    for batch in reader {
        if cancel.is_cancelled() {
            return Err("local data test cancelled".into());
        }
        let batch = batch.map_err(|e| e.to_string())?;
        appender
            .append_record_batch(bridge::to_duckdb(&batch)?)
            .map_err(|e| e.to_string())?;
    }
    appender.flush().map_err(|e| e.to_string())
}

// Wake and stop the scoped watcher on every return path, including unwinding.
struct FinishWatch<'a> {
    finished: &'a AtomicBool,
    thread: std::thread::Thread,
}
impl Drop for FinishWatch<'_> {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

#[cfg(test)]
#[path = "data_tests_tests.rs"]
mod tests;
