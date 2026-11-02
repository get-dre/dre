//! Local checks of immutable Arrow spools, using a disposable core-owned DuckDB database.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use arrow::datatypes::{DataType, TimeUnit};
use arrow::ipc::reader::FileReader;
use minijinja::Value;
use serde_json::json;

use super::{BindingRun, Fail, Statement};
use crate::codes::Code;
use crate::data_tests::{DataTest, Outcome, Status};
use crate::engine::Level;
use crate::render::{Mode, Renderer, RendererConfig};

#[path = "data_tests_bridge.rs"]
mod bridge;

impl BindingRun<'_> {
    /// Check the final result of this query before advancing to the next query.
    pub(super) fn test_result(&mut self, st: &Statement) -> Result<(), Fail> {
        if !self.stages.tests || !st.tab {
            return Ok(());
        }
        let tests = self
            .b
            .queries
            .iter()
            .find(|q| q.query == st.query)
            .map(|q| q.data_tests.clone())
            .unwrap_or_default();
        if tests.is_empty() {
            return Ok(());
        }
        let result = self.produced.last().expect("a tab's result was just saved");
        let spool = result.spool.clone();
        let mut failed = false;
        for test in tests {
            self.not_cancelled()?;
            let started = Instant::now();
            let checked = self.local_test(&spool, &test);
            let (status, failures, error) = match &checked {
                Ok(0) => (Status::Pass, Some(0), None),
                Ok(n) if test.config.severity == "warn" => (Status::Warn, Some(*n), None),
                Ok(n) => {
                    failed = true;
                    (Status::Fail, Some(*n), None)
                }
                Err(e) => (Status::Error, None, Some(self.ctx.secrets().mask(e).into_owned())),
            };
            let elapsed = started.elapsed();
            let detail = match (&failures, &error) {
                (Some(n), _) => format!("{n} failure rows"),
                (_, Some(e)) => e.clone(),
                _ => String::new(),
            };
            self.ui.step(
                Level::Info,
                match status {
                    Status::Pass => "PASS",
                    Status::Fail => "FAIL",
                    Status::Warn => "WARN",
                    Status::Error => "ERROR",
                },
                &format!(
                    "{}: query `{}` test `{}` (local, {}): {detail}",
                    self.label(),
                    st.query,
                    test.name,
                    test.config.severity
                ),
                Some(elapsed),
            );
            self.tests.push(Outcome {
                query: st.query.clone(),
                test: test.name.clone(),
                execution: "local".into(),
                severity: test.config.severity.clone(),
                status,
                failures,
                error: error.clone(),
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

    fn local_test(&self, spool: &Path, test: &DataTest) -> Result<u64, String> {
        // Keeping this under the existing spool directory gives success, failure and cancel
        // the same cleanup as report results, while DuckDB can spill without retaining rows.
        let workspace = tempfile::tempdir_in(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        let conn =
            duckdb::Connection::open(workspace.path().join("tests.duckdb")).map_err(|e| e.to_string())?;
        conn.execute_batch("SET memory_limit = '512MB'; SET threads = 1")
            .map_err(|e| e.to_string())?;
        load_result(&conn, spool)?;
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
                    json!({"__dre_relation": "\"dre_test_result\"", "__dre_arguments": test.arguments}),
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

fn load_result(conn: &duckdb::Connection, spool: &Path) -> Result<(), String> {
    let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let columns: Result<Vec<String>, String> = reader
        .schema()
        .fields()
        .iter()
        .map(|field| {
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
    conn.execute_batch(&format!("create table dre_test_result ({})", columns?.join(", ")))
        .map_err(|e| e.to_string())?;
    let mut appender = conn.appender("dre_test_result").map_err(|e| e.to_string())?;
    for batch in reader {
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
mod tests {
    use super::*;
    use arrow::array::{Int64Array, RecordBatch};
    use arrow::datatypes::{Field, Schema};
    use arrow::ipc::writer::FileWriter;
    use std::sync::Arc;

    #[test]
    fn loading_multiple_batches_and_empty_results_preserves_the_original_spool() {
        for empty in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let spool = temp.path().join("result.arrow");
            let schema = Arc::new(Schema::new(vec![Field::new("quoted\"id", DataType::Int64, true)]));
            let mut writer = FileWriter::try_new(File::create(&spool).unwrap(), &schema).unwrap();
            if !empty {
                for values in [vec![Some(1), None], vec![Some(i64::MAX)]] {
                    writer
                        .write(
                            &RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(values))])
                                .unwrap(),
                        )
                        .unwrap();
                }
            }
            writer.finish().unwrap();
            drop(writer);
            let before = std::fs::read(&spool).unwrap();
            let conn = duckdb::Connection::open_in_memory().unwrap();
            load_result(&conn, &spool).unwrap();
            let count: i64 = conn
                .query_row("select count(*) from dre_test_result", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, if empty { 0 } else { 3 });
            if !empty {
                let max: i64 = conn
                    .query_row("select max(\"quoted\"\"id\") from dre_test_result", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(max, i64::MAX);
            }
            assert_eq!(std::fs::read(&spool).unwrap(), before);
        }
    }
}
