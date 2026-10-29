//! The plugin host: everything the run path does with a plugin, behind one interface. Find a
//! plugin by kind and name ([`PluginHost::locate`]), start it ([`PluginHost::start`]), then ask
//! the running [`Plugin`] to describe itself, check options, run queries into Arrow batches,
//! format batches into files and deliver files or a message. [`PluginHost::cancel`] and
//! [`PluginHost::kill`] stop what this host started, and nothing else.
//!
//! Two adapters: [`SubprocessHost`] runs each plugin as a child process speaking the protocol
//! (production), and [`memory::MemoryHost`] runs scripted plugins in memory (tests).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use dre_protocol::host::{Canceller, Description, Execution, LogSink, PluginInfo, PluginProcess, StderrSync};
use dre_protocol::msg::{DeliveryFile, Message, ResultSetMeta};
use dre_protocol::{PluginId, parse_executable_name};
use serde_json::{Map, Value};

use crate::context::Env;
use crate::plugins::{LocateError, Located};
use crate::project::PluginRequirement;

pub mod memory;

/// What a plugin request returns: its answer, or what went wrong (the plugin's own error, a
/// crash, a protocol break).
pub type HostResult<T> = dre_protocol::host::Result<T>;

/// Finds and starts plugins, and stops the ones it started.
pub trait PluginHost: Send + Sync {
    /// The plugin `id` among a project's declared packages (`root`'s `dre.lock` pins them).
    fn locate(
        &self,
        root: &Path,
        packages: &[PluginRequirement],
        id: &PluginId,
    ) -> Result<Located, LocateError>;

    /// Start `plugin` with `cwd` as its working directory (the project's), its log lines going to
    /// `log`, and complete the handshake.
    fn start(&self, plugin: &Located, log: LogSink, cwd: Option<&Path>) -> HostResult<Box<dyn Plugin>>;

    /// Ask every plugin this host started to stop its running request. Returns how many had one.
    fn cancel(&self) -> usize;

    /// Stop every plugin this host started, at once.
    fn kill(&self);
}

/// How a source session opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    ReadWrite,
    /// Read-only, where the plugin supports it (`read_only` capability).
    ReadOnly,
    /// Read-only, and no files, programs or network beyond the database (`dre query`).
    Sandboxed,
}

/// The start of a format `write`: one file and the result sets that go into it.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteRequest {
    pub path: String,
    pub format: String,
    pub options: Map<String, Value>,
    pub result_sets: Vec<ResultSetMeta>,
    pub template: Option<Value>,
}

/// What a format wrote, and what it warns about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Written {
    pub files: Vec<String>,
    pub warnings: Vec<String>,
}

/// One delivery: files, or a message with `files` attached.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub files: Vec<DeliveryFile>,
    pub message: Option<Message>,
    pub connection: Map<String, Value>,
    pub options: Map<String, Value>,
}

/// Where a delivery went, and how many tries it took (more than 1 when the plugin retried).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub location: String,
    pub attempts: u32,
}

/// A started plugin. A format writes with [`write`](Plugin::write), then per result set its
/// batches and [`end_result_set`](Plugin::end_result_set), then [`finish`](Plugin::finish).
pub trait Plugin: Send {
    /// What the plugin said in its handshake.
    fn info(&self) -> &PluginInfo;

    fn has(&self, capability: &str) -> bool {
        self.info().capabilities.iter().any(|c| c == capability)
    }

    /// The plugin's `describe` reply.
    fn describe(&mut self) -> HostResult<Description>;

    /// Check a config block of options, with a format's `template`: the errors, then the
    /// warnings.
    fn validate(
        &mut self,
        options: Map<String, Value>,
        template: Option<Value>,
    ) -> HostResult<(Vec<String>, Vec<String>)>;

    /// Check a profile entry's settings without connecting; `unresolved` keys are unset
    /// `env_var()`s. The errors, then the warnings.
    fn validate_connection(
        &mut self,
        connection: Map<String, Value>,
        unresolved: Vec<String>,
    ) -> HostResult<(Vec<String>, Vec<String>)>;

    /// Open a source session.
    fn open(&mut self, connection: Map<String, Value>, open: Open) -> HostResult<()>;

    /// Check a statement without running it (`check` capability).
    fn check(&mut self, sql: &str) -> HostResult<()>;

    /// Run one statement, handing every batch to `on_batch` as it arrives.
    fn execute(
        &mut self,
        sql: &str,
        row_limit: Option<u64>,
        on_batch: &mut dyn FnMut(&SchemaRef, RecordBatch) -> Result<(), String>,
    ) -> HostResult<Execution>;

    /// Load `batch` into a temporary table on the session (`load` capability): the relation to
    /// use in SQL, and the plugin's warning.
    fn load(&mut self, name: &str, batch: RecordBatch) -> HostResult<(String, Option<String>)>;

    /// Start writing one file.
    fn write(&mut self, request: WriteRequest) -> HostResult<()>;

    /// Send one batch of the result set being written (the first carries its schema).
    fn write_batch(&mut self, batch: &RecordBatch) -> HostResult<()>;

    /// End the result set being written.
    fn end_result_set(&mut self) -> HostResult<()>;

    /// Finish the file: what was written.
    fn finish(&mut self) -> HostResult<Written>;

    /// Deliver files, or a message (`message` capability) with the files attached.
    fn deliver(&mut self, delivery: Delivery) -> HostResult<Delivered>;

    /// A handle that waits until the plugin's log lines so far are logged, when they arrive on
    /// another thread.
    fn stderr_sync(&self) -> Option<StderrSync> {
        None
    }

    /// Ask the plugin to exit, and wait for it.
    fn close(self: Box<Self>);
}

/// Write one result set: its batches (at least one, carrying the schema), then its end.
pub fn write_result_set(
    plugin: &mut dyn Plugin,
    schema: &SchemaRef,
    batches: impl IntoIterator<Item = RecordBatch>,
) -> HostResult<()> {
    let mut any = false;
    for b in batches {
        any = true;
        plugin.write_batch(&b)?;
    }
    if !any {
        plugin.write_batch(&RecordBatch::new_empty(schema.clone()))?;
    }
    plugin.end_result_set()
}

/// What a source or destination's `describe` said, as a run uses it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Described {
    pub secrets: Vec<String>,
    pub identifier_quote: Option<String>,
    pub string_literal: Option<String>,
    pub capabilities: Vec<String>,
    /// A message destination's length limit.
    pub message_limit: Option<u64>,
}

/// Runs each plugin as a child process speaking the protocol. Each host keeps its own list of
/// the processes it started, so cancelling one run never reaches another's.
pub struct SubprocessHost {
    /// Where plugins are installed (`DRE_PLUGINS_DIR`, `~/.dre`).
    env: Env,
    running: Arc<Running>,
    prober: Prober,
}

/// The processes a [`SubprocessHost`] has running, by a number of its own.
#[derive(Default)]
struct Running {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Canceller)>>,
}

impl Running {
    fn all(&self) -> Vec<Canceller> {
        self.list.lock().unwrap().iter().map(|(_, c)| c.clone()).collect()
    }
}

impl SubprocessHost {
    /// A host finding plugins as `env` says.
    pub fn new(env: &Env) -> SubprocessHost {
        SubprocessHost {
            env: env.clone(),
            running: Arc::default(),
            prober: Prober::default(),
        }
    }

    /// How many plugin processes this host has running.
    pub fn running(&self) -> usize {
        self.running.list.lock().unwrap().len()
    }
}

impl PluginHost for SubprocessHost {
    fn locate(
        &self,
        root: &Path,
        packages: &[PluginRequirement],
        id: &PluginId,
    ) -> Result<Located, LocateError> {
        crate::plugins::locate_in(root, packages, id, &self.env, &self.prober)
    }

    fn start(&self, plugin: &Located, log: LogSink, cwd: Option<&Path>) -> HostResult<Box<dyn Plugin>> {
        let process = PluginProcess::start_for(&plugin.path, Some(&plugin.plugin), log, cwd)?;
        let id = self.running.next.fetch_add(1, Ordering::SeqCst);
        self.running.list.lock().unwrap().push((id, process.canceller()));
        Ok(Box::new(Subprocess {
            process: Some(process),
            id,
            running: self.running.clone(),
        }))
    }

    fn cancel(&self) -> usize {
        self.running.all().iter().filter(|c| c.cancel()).count()
    }

    fn kill(&self) {
        for c in self.running.all() {
            c.kill();
        }
    }
}

/// A plugin process a [`SubprocessHost`] started; it leaves the host's list when it's closed or
/// dropped.
struct Subprocess {
    /// `None` only while closing.
    process: Option<PluginProcess>,
    id: u64,
    running: Arc<Running>,
}

impl Subprocess {
    fn p(&mut self) -> &mut PluginProcess {
        self.process.as_mut().expect("the process is open until close")
    }
}

impl Drop for Subprocess {
    fn drop(&mut self) {
        if let Ok(mut list) = self.running.list.lock() {
            list.retain(|(id, _)| *id != self.id);
        }
    }
}

impl Plugin for Subprocess {
    fn info(&self) -> &PluginInfo {
        self.process
            .as_ref()
            .expect("the process is open until close")
            .info()
    }

    fn describe(&mut self) -> HostResult<Description> {
        self.p().description()
    }

    fn validate(
        &mut self,
        options: Map<String, Value>,
        template: Option<Value>,
    ) -> HostResult<(Vec<String>, Vec<String>)> {
        self.p().validate_with(options, template)
    }

    fn validate_connection(
        &mut self,
        connection: Map<String, Value>,
        unresolved: Vec<String>,
    ) -> HostResult<(Vec<String>, Vec<String>)> {
        self.p().validate_connection(connection, unresolved)
    }

    fn open(&mut self, connection: Map<String, Value>, open: Open) -> HostResult<()> {
        match open {
            Open::ReadWrite => self.p().open(connection, false),
            Open::ReadOnly => self.p().open(connection, true),
            Open::Sandboxed => self.p().open_sandboxed(connection),
        }
    }

    fn check(&mut self, sql: &str) -> HostResult<()> {
        self.p().check(sql)
    }

    fn execute(
        &mut self,
        sql: &str,
        row_limit: Option<u64>,
        on_batch: &mut dyn FnMut(&SchemaRef, RecordBatch) -> Result<(), String>,
    ) -> HostResult<Execution> {
        self.p().execute(sql, row_limit, on_batch)
    }

    fn load(&mut self, name: &str, batch: RecordBatch) -> HostResult<(String, Option<String>)> {
        let schema = batch.schema();
        self.p().load(name, &schema, [batch])
    }

    fn write(&mut self, r: WriteRequest) -> HostResult<()> {
        self.p()
            .write_begin(&r.path, &r.format, r.options, r.result_sets, r.template)
    }

    fn write_batch(&mut self, batch: &RecordBatch) -> HostResult<()> {
        self.p().send_batch(batch)
    }

    fn end_result_set(&mut self) -> HostResult<()> {
        self.p().send(&dre_protocol::msg::Request::ResultSetEnd {})
    }

    fn finish(&mut self) -> HostResult<Written> {
        let (files, warnings) = self.p().write_finish_with_warnings()?;
        Ok(Written { files, warnings })
    }

    fn deliver(&mut self, d: Delivery) -> HostResult<Delivered> {
        let p = self.p();
        let location = match &d.message {
            Some(m) => p.deliver_message(m, &d.files, d.connection, d.options)?,
            None => p.deliver_files(&d.files, d.connection, d.options)?,
        };
        Ok(Delivered {
            location,
            attempts: p.last_attempts(),
        })
    }

    fn stderr_sync(&self) -> Option<StderrSync> {
        self.process.as_ref().map(PluginProcess::stderr_sync)
    }

    fn close(mut self: Box<Self>) {
        if let Some(p) = self.process.take() {
            let _ = p.close();
        }
    }
}

/// What an executable said it provides, and the file's modification time when asked.
type Asked = (Option<std::time::SystemTime>, Vec<PluginId>);

/// Asks executables which plugins they serve, and remembers the answers (per file and
/// modification time) for as long as it lives.
#[derive(Default)]
pub struct Prober {
    asked: Mutex<BTreeMap<PathBuf, Asked>>,
}

impl Prober {
    /// The plugins an executable serves, from its name when that's `dre-<kind>-<name>`, else
    /// from its handshake.
    pub fn probe(&self, path: &Path) -> Result<Vec<PluginId>, String> {
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some((kind, name)) = parse_executable_name(&file) {
            return Ok(vec![PluginId::new(kind, name)]);
        }
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(ids) = self
            .asked
            .lock()
            .unwrap()
            .get(path)
            .filter(|(m, _)| *m == modified)
            .map(|(_, ids)| ids.clone())
        {
            return Ok(ids);
        }
        let quiet: LogSink = Arc::new(|_, _| {});
        let p = PluginProcess::start(path, quiet)
            .map_err(|e| format!("can't ask {} what it provides: {e}", path.display()))?;
        let ids = p.info().provides.clone();
        let _ = p.close();
        self.asked
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), (modified, ids.clone()));
        Ok(ids)
    }
}
