//! An in-memory [`PluginHost`] for tests: scripted plugins that run in this process. A script
//! says what each query returns, which call fails (the Nth execute, write or delivery, or the
//! one with a given detail, such as a delivery's path) and how long a call takes; the host
//! records every call and can cancel the ones in flight.
//!
//! A format writes a small text file per `write` (its result sets' names and row counts), so the
//! run finds real files to zip and deliver. A destination delivers nowhere: it records the files
//! (or message) and answers `memory://<remote path>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use dre_protocol::host::{Description, Execution, HostError, LogSink, PluginInfo};
use dre_protocol::{Kind, PluginId};
use serde_json::{Map, Value};

use super::{Delivered, Delivery, HostResult, Open, Plugin, PluginHost, WriteRequest, Written};
use crate::plugins::{LocateError, Located};
use crate::project::PluginRequirement;

/// A call a script can fail or slow down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Call {
    Start,
    Open,
    Execute,
    Write,
    Deliver,
}

/// What one scripted plugin does.
#[derive(Clone, Default)]
pub struct Script {
    capabilities: Vec<String>,
    description: Description,
    /// Rows by SQL (trimmed, without a trailing `;`). Any other statement returns no result set.
    rows: BTreeMap<String, Vec<RecordBatch>>,
    /// `(call, n, message)`: the plugin's `n`th such call (from 1, counted across the host)
    /// fails with `message`.
    failures: Vec<(Call, usize, String)>,
    /// `(call, detail, message)`: every such call with this detail (what [`MemoryHost::calls`]
    /// shows after the call's name) fails with `message`, whatever order calls arrive in.
    failures_on: Vec<(Call, String, String)>,
    /// How long each such call takes; a cancel ends the wait early with an error.
    delays: BTreeMap<Call, Duration>,
    logs: BTreeMap<Call, String>,
}

impl Script {
    /// A plugin with these capabilities.
    pub fn new(capabilities: &[&str]) -> Script {
        Script {
            capabilities: capabilities.iter().map(|c| (*c).to_string()).collect(),
            ..Script::default()
        }
    }

    /// `sql` returns `batch`.
    pub fn rows(mut self, sql: &str, batch: RecordBatch) -> Script {
        self.rows.insert(normalize(sql), vec![batch]);
        self
    }

    /// `sql` streams these batches, in order. An empty list returns no result set.
    pub fn batches(mut self, sql: &str, batches: Vec<RecordBatch>) -> Script {
        self.rows.insert(normalize(sql), batches);
        self
    }

    /// The `n`th `call` (from 1) fails with `message`.
    pub fn fail(mut self, call: Call, n: usize, message: &str) -> Script {
        self.failures.push((call, n, message.to_string()));
        self
    }

    /// Every `call` whose detail is `detail` fails with `message`. Unlike [`Script::fail`] it
    /// doesn't depend on call order, so it picks one row when rows deliver concurrently.
    pub fn fail_on(mut self, call: Call, detail: &str, message: &str) -> Script {
        self.failures_on
            .push((call, detail.to_string(), message.to_string()));
        self
    }

    /// Every `call` takes `delay`.
    pub fn delay(mut self, call: Call, delay: Duration) -> Script {
        self.delays.insert(call, delay);
        self
    }

    /// Emit a plugin diagnostic on each `call`, through the sink given at process start.
    pub fn log_on(mut self, call: Call, message: &str) -> Script {
        self.logs.insert(call, message.to_string());
        self
    }

    /// What `describe` answers.
    pub fn description(mut self, description: Description) -> Script {
        self.description = description;
        self
    }
}

fn normalize(sql: &str) -> String {
    sql.trim().trim_end_matches(';').trim().to_string()
}

/// What a host's plugins did, in order: `"<kind>/<name> <call> <detail>"`.
#[derive(Default)]
struct State {
    calls: Mutex<Vec<String>>,
    counts: Mutex<BTreeMap<(PluginId, Call), usize>>,
    /// Set by [`PluginHost::cancel`]: calls waiting out a delay stop.
    cancelled: AtomicBool,
    /// Calls waiting out a delay now.
    waiting: AtomicUsize,
}

/// Scripted plugins, by kind and name.
#[derive(Clone, Default)]
pub struct MemoryHost {
    scripts: BTreeMap<PluginId, Script>,
    state: Arc<State>,
}

impl MemoryHost {
    /// A host serving no plugin yet.
    pub fn new() -> MemoryHost {
        MemoryHost::default()
    }

    /// Serve the `kind` plugin `name` as `script` says.
    pub fn plugin(mut self, kind: Kind, name: &str, script: Script) -> MemoryHost {
        self.scripts.insert(PluginId::new(kind, name), script);
        self
    }

    /// Every call so far, in order.
    ///
    /// # Panics
    ///
    /// Panics if the call list is poisoned (a thread panicked while changing it).
    pub fn calls(&self) -> Vec<String> {
        self.state.calls.lock().unwrap().clone()
    }

    /// Whether [`PluginHost::cancel`] was called.
    pub fn cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }
}

impl PluginHost for MemoryHost {
    fn locate(
        &self,
        _root: &Path,
        _packages: &[PluginRequirement],
        id: &PluginId,
    ) -> Result<Located, LocateError> {
        if !self.scripts.contains_key(id) {
            return Err(LocateError::NotProvided(format!(
                "no plugin package the project declares provides the {} plugin `{}`",
                id.kind, id.name
            )));
        }
        Ok(Located {
            path: PathBuf::from(format!("memory/{}-{}", id.kind, id.name)),
            plugin: id.clone(),
            package: id.name.clone(),
        })
    }

    fn start(&self, plugin: &Located, log: LogSink, _cwd: Option<&Path>) -> HostResult<Box<dyn Plugin>> {
        let id = plugin.plugin.clone();
        let script = self
            .scripts
            .get(&id)
            .cloned()
            .ok_or_else(|| HostError::Malformed {
                plugin: id.name.clone(),
                message: "no script for this plugin".into(),
            })?;
        let mut p = Scripted {
            info: PluginInfo {
                protocol_version: dre_protocol::MAX_VERSION,
                kind: id.kind,
                name: id.name.clone(),
                version: "0.0.0".into(),
                capabilities: script.capabilities.clone(),
                provides: vec![id.clone()],
            },
            id,
            script,
            state: self.state.clone(),
            writing: None,
            log,
        };
        p.call(Call::Start, "")?;
        Ok(Box::new(p))
    }

    fn cancel(&self) -> usize {
        self.state.cancelled.store(true, Ordering::SeqCst);
        self.state.waiting.load(Ordering::SeqCst)
    }

    fn kill(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
    }
}

/// A file being written: its request and each result set's row count so far.
struct Writing {
    request: WriteRequest,
    rows: Vec<usize>,
}

struct Scripted {
    id: PluginId,
    info: PluginInfo,
    script: Script,
    state: Arc<State>,
    writing: Option<Writing>,
    log: LogSink,
}

impl Scripted {
    fn error(&self, message: &str) -> HostError {
        HostError::Plugin {
            plugin: self.id.name.clone(),
            message: message.to_string(),
            kind: None,
            code: None,
        }
    }

    /// Record `call`, wait out its delay, and fail it if the script says so.
    fn call(&mut self, call: Call, detail: &str) -> HostResult<()> {
        let line = format!("{}/{} {call:?} {detail}", self.id.kind, self.id.name);
        self.state.calls.lock().unwrap().push(line.trim_end().to_string());
        if let Some(message) = self.script.logs.get(&call) {
            (self.log)(&self.id.name, message);
        }
        let n = {
            let mut counts = self.state.counts.lock().unwrap();
            let n = counts.entry((self.id.clone(), call)).or_default();
            *n += 1;
            *n
        };
        if let Some(delay) = self.script.delays.get(&call) {
            self.state.waiting.fetch_add(1, Ordering::SeqCst);
            let until = Instant::now() + *delay;
            while Instant::now() < until && !self.state.cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.state.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        if self.state.cancelled.load(Ordering::SeqCst) && call != Call::Start {
            return Err(HostError::Plugin {
                plugin: self.id.name.clone(),
                message: "the request was cancelled".into(),
                kind: Some("cancelled".into()),
                code: Some("cancelled".into()),
            });
        }
        let by_count = self
            .script
            .failures
            .iter()
            .find(|(c, at, _)| *c == call && *at == n)
            .map(|(_, _, message)| message);
        let by_detail = self
            .script
            .failures_on
            .iter()
            .find(|(c, d, _)| *c == call && d == detail)
            .map(|(_, _, message)| message);
        match by_count.or(by_detail) {
            Some(message) => Err(self.error(message)),
            None => Ok(()),
        }
    }

    fn writing(&mut self) -> HostResult<&mut Writing> {
        match self.writing.as_mut() {
            Some(w) => Ok(w),
            None => Err(HostError::Malformed {
                plugin: self.id.name.clone(),
                message: "result data outside a write".into(),
            }),
        }
    }
}

impl Plugin for Scripted {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn describe(&mut self) -> HostResult<Description> {
        Ok(self.script.description.clone())
    }

    fn validate(
        &mut self,
        _options: Map<String, Value>,
        _template: Option<Value>,
    ) -> HostResult<(Vec<String>, Vec<String>)> {
        Ok((Vec::new(), Vec::new()))
    }

    fn validate_connection(
        &mut self,
        _connection: Map<String, Value>,
        _unresolved: Vec<String>,
    ) -> HostResult<(Vec<String>, Vec<String>)> {
        Ok((Vec::new(), Vec::new()))
    }

    fn open(&mut self, _connection: Map<String, Value>, open: Open) -> HostResult<()> {
        self.call(Call::Open, &format!("{open:?}"))
    }

    fn check(&mut self, _sql: &str) -> HostResult<()> {
        Ok(())
    }

    fn execute(
        &mut self,
        sql: &str,
        _row_limit: Option<u64>,
        on_batch: &mut dyn FnMut(&SchemaRef, RecordBatch) -> Result<(), String>,
    ) -> HostResult<Execution> {
        let sql = normalize(sql);
        self.call(Call::Execute, &sql)?;
        let Some(batches) = self.script.rows.get(&sql).cloned() else {
            return Ok(Execution::NoResult { rows_affected: None });
        };
        let Some(first) = batches.first() else {
            return Ok(Execution::NoResult { rows_affected: None });
        };
        let schema = first.schema();
        let rows = batches.iter().map(|batch| batch.num_rows() as u64).sum();
        for batch in batches {
            on_batch(&schema, batch).map_err(|e| self.error(&e))?;
        }
        Ok(Execution::Result { schema, rows })
    }

    fn load(&mut self, name: &str, _batch: RecordBatch) -> HostResult<(String, Option<String>)> {
        Ok((name.to_string(), None))
    }

    fn write(&mut self, request: WriteRequest) -> HostResult<()> {
        self.call(Call::Write, &request.path)?;
        self.writing = Some(Writing {
            request,
            rows: vec![0],
        });
        Ok(())
    }

    fn write_batch(&mut self, batch: &RecordBatch) -> HostResult<()> {
        let w = self.writing()?;
        if let Some(n) = w.rows.last_mut() {
            *n += batch.num_rows();
        }
        Ok(())
    }

    fn end_result_set(&mut self) -> HostResult<()> {
        self.writing()?.rows.push(0);
        Ok(())
    }

    fn finish(&mut self) -> HostResult<Written> {
        let w = self
            .writing
            .take()
            .ok_or_else(|| self.error("finish outside a write"))?;
        let body: String = w
            .request
            .result_sets
            .iter()
            .zip(&w.rows)
            .map(|(m, n)| format!("{}: {n} rows\n", m.name))
            .collect();
        std::fs::write(&w.request.path, body).map_err(|e| self.error(&e.to_string()))?;
        Ok(Written {
            files: vec![w.request.path],
            warnings: Vec::new(),
        })
    }

    fn deliver(&mut self, d: Delivery) -> HostResult<Delivered> {
        let what: Vec<String> = d
            .files
            .iter()
            .map(|f| f.remote_path.clone().unwrap_or_else(|| f.local_path.clone()))
            .collect();
        let detail = match &d.message {
            Some(m) => format!("message `{}` {}", m.title, what.join(" ")),
            None => what.join(" "),
        };
        self.call(Call::Deliver, detail.trim_end())?;
        Ok(Delivered {
            location: format!("memory://{}", what.join(",")),
            attempts: 1,
        })
    }

    fn close(self: Box<Self>) {
        let line = format!("{}/{} Close", self.id.kind, self.id.name);
        self.state.calls.lock().unwrap().push(line);
    }
}
