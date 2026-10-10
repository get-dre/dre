//! The plugin SDK: implement one of [`Source`], [`Format`] or [`Destination`] and call the
//! matching `serve_*` function from `main`. The SDK owns stdin/stdout, the handshake, framing,
//! request ids, error replies and panics.
//!
//! Formats and destinations declare their options (`options()`) and add any rule a declaration
//! can't express (`validate()`). The SDK checks a config block against both on `validate` and
//! before every `write` and `deliver`, so plugin code only sees options that passed.
//!
//! - **Logging:** use the `log` crate (`log::info!`, `log::warn!`), or [`log`] for a message with
//!   fields; the SDK sends them to core as `log` messages. The plugin's own records keep their
//!   level; its dependencies' are shown only with `-v`. Long requests may report [`progress`].
//! - **Errors:** return a [`PluginError`] to give core the problem's kind and code; any other
//!   error is sent as a plain message.
//! - **Cancellation:** stdin is read on a background thread while a request runs. On `cancel`
//!   (or when core goes away) [`cancelled`] turns true and the hook set with [`on_cancel`] runs,
//!   e.g. to cancel a query on the server.

use std::io::{BufReader, BufWriter, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use serde_json::{Map, Value};

use crate::frame::{self, Frame, FrameError};
use crate::msg::{ConnectionField, Envelope, LogLevel, Request, Response, ResultSetMeta};
use crate::options::{self, OptionField};
use crate::{CAP_VALIDATE, Kind, MAX_VERSION, MIN_VERSION, PluginId};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// What kind of problem a [`PluginError`] is (the kinds of DRE's error-code registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The settings are wrong.
    Config,
    /// The plugin itself misbehaved.
    Plugin,
    /// Refused something unsafe.
    Refused,
    /// A connection couldn't be made; trying again may work.
    Connection,
    /// Credentials were refused.
    Auth,
    /// A query failed on the database.
    Query,
    /// A file or message couldn't be delivered.
    Delivery,
    /// Something unexpected: a bug.
    Internal,
    Cancelled,
    TimedOut,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Config => "config",
            ErrorKind::Plugin => "plugin",
            ErrorKind::Refused => "refused",
            ErrorKind::Connection => "connection",
            ErrorKind::Auth => "auth",
            ErrorKind::Query => "query",
            ErrorKind::Delivery => "delivery",
            ErrorKind::Internal => "internal",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::TimedOut => "timed_out",
        }
    }
}

/// An error with its kind and, optionally, a code. The SDK namespaces the code by the plugin
/// (`host-key-mismatch` is sent as `sftp/host-key-mismatch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    pub kind: ErrorKind,
    pub code: Option<String>,
    pub message: String,
}

impl PluginError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> PluginError {
        PluginError {
            kind,
            code: None,
            message: message.into(),
        }
    }

    pub fn code(mut self, code: impl Into<String>) -> PluginError {
        self.code = Some(code.into());
        self
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

/// The SDK's process-wide state, shared by the request loop, the stdin reader, and logging
/// from any thread.
struct Shared {
    out: Mutex<BufWriter<Stdout>>,
    /// The protocol version settled on (0 until the handshake).
    version: AtomicU32,
    /// The request running now (0: none).
    current: AtomicU64,
    /// The last request core cancelled (0: none).
    cancelled: AtomicU64,
    hook: Mutex<Option<CancelHook>>,
    /// The plugin being served, for namespacing its error codes.
    name: OnceLock<String>,
}

type CancelHook = Arc<dyn Fn() + Send + Sync>;

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        out: Mutex::new(BufWriter::new(std::io::stdout())),
        version: AtomicU32::new(0),
        current: AtomicU64::new(0),
        cancelled: AtomicU64::new(0),
        hook: Mutex::new(None),
        name: OnceLock::new(),
    })
}

/// Whether core has cancelled the request running now. Long loops check it between steps.
pub fn cancelled() -> bool {
    let s = shared();
    let current = s.current.load(Ordering::SeqCst);
    current != 0 && s.cancelled.load(Ordering::SeqCst) == current
}

/// Run `hook` when core cancels a request, on the stdin reader's thread, while the request is
/// still running on the main one (e.g. ask the server to cancel the query). Replaces any hook
/// set before. The request then fails, and the SDK replies with kind `cancelled`.
pub fn on_cancel(hook: impl Fn() + Send + Sync + 'static) {
    *shared().hook.lock().unwrap() = Some(Arc::new(hook));
}

/// Cancel request `id`: remembered, so a request cancelled before it starts never runs, and
/// the hook runs if it's running now.
fn cancel(id: u64) {
    let s = shared();
    s.cancelled.store(id, Ordering::SeqCst);
    if id != 0 && s.current.load(Ordering::SeqCst) == id {
        let hook = s.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Send a log message to core, with structured `fields` (never secrets).
pub fn log_fields(level: LogLevel, message: &str, fields: Map<String, Value>) {
    let s = shared();
    if s.version.load(Ordering::SeqCst) == 0 {
        // Protocol 0 has no `log` message: stderr, with the prefixes core 0.3 shows.
        let prefix = match level {
            LogLevel::Error | LogLevel::Warn => "warning: ",
            LogLevel::Info => "info: ",
            LogLevel::Debug | LogLevel::Trace => "",
        };
        eprintln!("{prefix}{message}");
        return;
    }
    send(&Response::Log {
        level,
        message: message.to_string(),
        fields,
    });
}

/// Send a log message to core.
pub fn log(level: LogLevel, message: &str) {
    log_fields(level, message, Map::new())
}

/// Tell core how far a long request has got; it shows the latest.
pub fn progress(message: Option<&str>, done: Option<u64>, total: Option<u64>) {
    if shared().version.load(Ordering::SeqCst) == 0 {
        if let Some(m) = message {
            log(LogLevel::Info, m);
        }
        return;
    }
    send(&Response::Progress {
        message: message.map(str::to_string),
        done,
        total,
    });
}

/// Bridges the `log` crate to `log` messages.
struct FrameLogger;

impl ::log::Log for FrameLogger {
    fn enabled(&self, m: &::log::Metadata) -> bool {
        m.level() <= ::log::Level::Debug
    }

    fn log(&self, r: &::log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let level = match r.level() {
            ::log::Level::Error => LogLevel::Error,
            ::log::Level::Warn => LogLevel::Warn,
            ::log::Level::Info => LogLevel::Info,
            ::log::Level::Debug => LogLevel::Debug,
            ::log::Level::Trace => LogLevel::Trace,
        };
        // A dependency's records (a driver, an HTTP client) are for `-v` only.
        let level = if r.target().starts_with("dre") {
            level
        } else {
            level.max(LogLevel::Debug)
        };
        log(level, &r.args().to_string());
    }

    fn flush(&self) {}
}

/// Write one reply or streamed message, with the running request's id from protocol 1.
fn send(r: &Response) {
    let s = shared();
    let id = match s.version.load(Ordering::SeqCst) {
        0 => None,
        _ => Some(s.current.load(Ordering::SeqCst)).filter(|id| *id != 0),
    };
    let mut out = s.out.lock().unwrap();
    if frame::write_json(&mut *out, &Envelope::new(id, r)).is_err() {
        // Core has gone; nothing left to talk to.
        std::process::exit(1);
    }
}

/// The `error` reply for a failed request: kind `cancelled` if core cancelled it, else the
/// kind and code of a [`PluginError`], else the message alone.
fn error_reply(e: &Error) -> Response {
    if cancelled() {
        return Response::Error {
            message: e.to_string(),
            kind: Some("cancelled".into()),
            code: None,
        };
    }
    match e.downcast_ref::<PluginError>() {
        Some(pe) => Response::Error {
            message: pe.message.clone(),
            kind: Some(pe.kind.as_str().into()),
            code: pe.code.as_ref().map(|c| match shared().name.get() {
                Some(name) if !c.contains('/') => format!("{name}/{c}"),
                _ => c.clone(),
            }),
        },
        None => Response::error(e.to_string()),
    }
}

/// Identity reported in the handshake.
#[derive(Debug, Clone)]
pub struct About {
    pub name: &'static str,
    pub version: &'static str,
    pub capabilities: &'static [&'static str],
}

impl About {
    /// `version` "0.0.0" (the crate has no version yet) is reported as "unreleased".
    pub fn new(name: &'static str, version: &'static str) -> About {
        let version = if version == "0.0.0" { "unreleased" } else { version };
        About {
            name,
            version,
            capabilities: &[],
        }
    }

    pub fn capabilities(mut self, caps: &'static [&'static str]) -> About {
        self.capabilities = caps;
        self
    }
}

/// Where a source pushes the outcome of `execute`: either `no_result`, or `begin` followed by
/// any number of `batch` calls.
pub trait ResultSink {
    fn no_result(&mut self, rows_affected: Option<u64>) -> Result<()>;
    fn begin(&mut self, schema: SchemaRef) -> Result<()>;
    /// Returns `false` once the row limit is reached: stop producing batches.
    fn batch(&mut self, batch: RecordBatch) -> Result<bool>;
}

pub trait Source {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        Vec::new()
    }
    /// The character the database quotes identifiers with, reported in `describe`: `"` for
    /// most SQL databases, a backtick for Databricks and MySQL. Core doubles it inside a name.
    fn identifier_quote(&self) -> Option<&'static str> {
        None
    }
    /// Rules the declared [`Source::connection_fields`] can't express (two keys that can't go
    /// together, a value's form), each a sentence naming the field, never its value. Static:
    /// no network. Run by `validate_connection` and before `open`.
    /// A value set through an unset `env_var()` arrives as `null`.
    fn validate_connection(&self, _connection: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    fn open(&mut self, connection: &Map<String, Value>, read_only: bool) -> Result<()>;
    /// Run one statement. `row_limit` is a hint; the SDK enforces it either way.
    fn execute(&mut self, sql: &str, row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()>;
    /// Verify without executing. Only called when the plugin advertises `check`.
    fn check(&mut self, _sql: &str) -> Result<()> {
        Err("this source can't check statements".into())
    }
    /// Load `data` into a temporary table on the session, named after `name`. Only called when
    /// the plugin advertises `load`.
    fn load(&mut self, _name: &str, _data: &mut ResultSet<'_>) -> Result<Loaded> {
        Err("this source can't load rows".into())
    }
    /// End the session cleanly (called on `close` and at end of input, before exiting).
    fn close(&mut self) {}
}

/// The outcome of `Source::load`.
pub struct Loaded {
    /// How SQL refers to the loaded rows, e.g. the temp table's name.
    pub relation: String,
    pub rows: u64,
    /// Shown to the user, e.g. when the database has no bulk path.
    pub warning: Option<String>,
}

/// One incoming result set, streamed from core.
pub struct ResultSet<'a> {
    pub meta: ResultSetMeta,
    pub schema: SchemaRef,
    first: Option<Vec<RecordBatch>>,
    input: &'a mut Input,
    done: bool,
}

impl ResultSet<'_> {
    /// The next batch, or `None` at the end of this result set.
    pub fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        loop {
            if let Some(first) = self.first.as_mut() {
                if !first.is_empty() {
                    return Ok(Some(first.remove(0)));
                }
                self.first = None;
            }
            if self.done {
                return Ok(None);
            }
            match self.input.read()? {
                Frame::Arrow(ipc) => {
                    let (_, batches) = frame::decode_batches(&ipc)?;
                    self.first = Some(batches);
                }
                Frame::Json(v) => match serde_json::from_value::<Request>(v)? {
                    Request::ResultSetEnd {} => {
                        self.done = true;
                        return Ok(None);
                    }
                    other => return Err(format!("expected result data, got {other:?}").into()),
                },
            }
        }
    }

    fn drain(&mut self) -> Result<()> {
        while self.next_batch()?.is_some() {}
        Ok(())
    }
}

/// A format `write` request.
pub struct WriteRequest {
    pub path: String,
    pub format: String,
    pub options: Map<String, Value>,
    pub result_sets: Vec<ResultSetMeta>,
    pub template: Option<Value>,
}

/// Hands a format plugin its result sets one at a time, in order.
pub struct ResultSets<'a> {
    metas: std::vec::IntoIter<ResultSetMeta>,
    input: &'a mut Input,
    warnings: Vec<String>,
}

impl ResultSets<'_> {
    /// Tell the person something about the output, shown as a warning by core.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// The next result set, or `None` after the last. Each must be read (or is drained) before
    /// the next is requested.
    pub fn next_set(&mut self) -> Result<Option<ResultSet<'_>>> {
        let Some(meta) = self.metas.next() else {
            return Ok(None);
        };
        let ipc = match self.input.read()? {
            Frame::Arrow(ipc) => ipc,
            Frame::Json(v) => {
                return Err(format!(
                    "expected an Arrow frame starting result set `{}`, got {v}",
                    meta.name
                )
                .into());
            }
        };
        let (schema, batches) = frame::decode_batches(&ipc)?;
        Ok(Some(ResultSet {
            meta,
            schema,
            first: Some(batches),
            input: &mut *self.input,
            done: false,
        }))
    }
}

pub trait Format {
    /// The options this format takes: every key of a report's `output:` block except `format`,
    /// `destination`, `template` and `extension`, which core owns.
    fn options(&self) -> Vec<OptionField> {
        Vec::new()
    }
    /// Problems the declared [`Format::options`] can't catch, each a sentence naming the key.
    /// Only called on options whose declared types already passed.
    fn validate(&self, _options: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    /// Write every result set to `req.path` (and siblings, if the format needs several files);
    /// return the files written.
    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>>;
}

/// A destination `deliver` request.
pub struct Delivery {
    /// One file, or every file of an output when the plugin advertises `multi_file`. With a
    /// `message`: the files to attach to it (often none).
    pub files: Vec<DeliveryFile>,
    pub connection: Map<String, Value>,
    /// The destination entry's plugin options, rendered by core (empty when none).
    pub options: Map<String, Value>,
    /// A message to post; only sent to a plugin advertising `message`.
    pub message: Option<crate::msg::Message>,
}

/// One file to deliver: the local copy in `target/run/` and its rendered remote path, if any.
pub struct DeliveryFile {
    pub local: PathBuf,
    pub remote: Option<String>,
}

pub trait Destination {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        Vec::new()
    }
    /// The options a report's destination entry takes: every key but `profile` and `path`.
    /// String values may still hold Jinja when checked (see [`options::is_template`]); core
    /// renders it before `deliver`.
    fn options(&self) -> Vec<OptionField> {
        Vec::new()
    }
    /// Problems the declared [`Destination::options`] can't catch, each a sentence naming the key.
    /// Only called on options whose declared types already passed.
    fn validate(&self, _options: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    /// Rules the declared [`Destination::connection_fields`] can't express, each a sentence
    /// naming the field, never its value. Static: no network. Run by `validate_connection` and
    /// before every delivery.
    fn validate_connection(&self, _connection: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }
    /// Deliver `local` to `remote` (rendered by core); return where it landed. Enough for a
    /// destination that takes no options; others implement [`Destination::deliver_files`].
    fn deliver(
        &mut self,
        _local: &Path,
        _remote: Option<&str>,
        _connection: &Map<String, Value>,
    ) -> Result<String> {
        Err("this destination doesn't implement `deliver`".into())
    }
    /// The most characters a message may have in the service (after translation), reported in
    /// `describe` by a plugin advertising `message`.
    fn message_limit(&self) -> Option<u64> {
        None
    }
    /// Post `d.message` (with `d.files` attached). Called only for a plugin advertising
    /// `message`, which implements it.
    fn deliver_message(&mut self, _d: &Delivery, _m: &crate::msg::Message) -> Result<String> {
        Err("this destination doesn't take messages".into())
    }
    /// The whole request, options included (already checked). The default hands a single file
    /// to [`Destination::deliver`]; several files arrive only with `multi_file` advertised.
    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        match d.files.as_slice() {
            [f] => self.deliver(&f.local, f.remote.as_deref(), &d.connection),
            _ => Err("this destination takes one file per delivery".into()),
        }
    }
}

/// The frames core sends, read on a background thread (see [`read_stdin`]).
pub struct Input {
    rx: Receiver<std::result::Result<Frame, FrameError>>,
}

impl Input {
    fn next(&mut self) -> std::result::Result<Frame, FrameError> {
        self.rx.recv().unwrap_or(Err(FrameError::Eof))
    }

    fn read(&mut self) -> Result<Frame> {
        self.next().map_err(|e| -> Error {
            match e {
                FrameError::Eof => "core closed the connection".into(),
                e => e.to_string().into(),
            }
        })
    }
}

/// Read stdin into `tx`, acting on `cancel` at once rather than queueing it behind the request
/// it cancels. End of input cancels the running request too: core has gone.
fn read_stdin(tx: Sender<std::result::Result<Frame, FrameError>>) {
    let mut r = BufReader::new(std::io::stdin());
    loop {
        let f = frame::read_frame(&mut r);
        match &f {
            Ok(Frame::Json(v)) if v.get("type").and_then(Value::as_str) == Some("cancel") => {
                cancel(v.get("id").and_then(Value::as_u64).unwrap_or(0));
                continue;
            }
            Err(_) => cancel(shared().current.load(Ordering::SeqCst)),
            Ok(_) => {}
        }
        let stop = f.is_err();
        if tx.send(f).is_err() || stop {
            break;
        }
    }
}

/// Writes replies and result data (see [`send`]).
struct Output;

impl Output {
    fn send(&mut self, r: &Response) {
        send(r)
    }

    fn batch(&mut self, b: &RecordBatch) -> Result<()> {
        let ipc = frame::encode_batch(b)?;
        let mut out = shared().out.lock().unwrap();
        frame::write_arrow(&mut *out, &ipc).map_err(|_| -> Error { "core closed the connection".into() })
    }
}

enum Handler<'a> {
    Source(&'a mut dyn Source),
    Format(&'a mut dyn Format),
    Destination(&'a mut dyn Destination),
}

impl Handler<'_> {
    fn kind(&self) -> Kind {
        match self {
            Handler::Source(_) => Kind::Source,
            Handler::Format(_) => Kind::Format,
            Handler::Destination(_) => Kind::Destination,
        }
    }

    fn option_fields(&self) -> Vec<OptionField> {
        match self {
            Handler::Source(_) => Vec::new(),
            Handler::Format(f) => f.options(),
            Handler::Destination(d) => d.options(),
        }
    }

    /// Every problem with a config block: the declared checks, then the plugin's own once the
    /// declared ones pass.
    fn check(&self, name: &str, o: &Map<String, Value>) -> Vec<String> {
        let errs = options::check(self.kind(), name, &self.option_fields(), o);
        if !errs.is_empty() {
            return errs;
        }
        match self {
            Handler::Source(_) => Vec::new(),
            Handler::Format(f) => f.validate(o),
            Handler::Destination(d) => d.validate(o),
        }
    }

    /// The config block's problems as one error, for `write` and `deliver`.
    fn checked(&self, name: &str, o: &Map<String, Value>) -> Result<()> {
        let errs = self.check(name, o);
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; ").into())
        }
    }
}

pub fn serve_source(about: About, s: impl Source + 'static) -> ! {
    serve_package(vec![Plugin::Source(about, Box::new(s))])
}

pub fn serve_format(about: About, f: impl Format + 'static) -> ! {
    serve_package(vec![Plugin::Format(about, Box::new(f))])
}

pub fn serve_destination(about: About, d: impl Destination + 'static) -> ! {
    serve_package(vec![Plugin::Destination(about, Box::new(d))])
}

/// One of the plugins a package's executable serves (see [`serve_package`]).
pub enum Plugin {
    Source(About, Box<dyn Source>),
    Format(About, Box<dyn Format>),
    Destination(About, Box<dyn Destination>),
}

impl Plugin {
    fn id(&self) -> PluginId {
        let (kind, about) = match self {
            Plugin::Source(a, _) => (Kind::Source, a),
            Plugin::Format(a, _) => (Kind::Format, a),
            Plugin::Destination(a, _) => (Kind::Destination, a),
        };
        PluginId::new(kind, about.name)
    }

    fn handler(&mut self) -> (&About, Handler<'_>) {
        match self {
            Plugin::Source(a, s) => (a, Handler::Source(s.as_mut())),
            Plugin::Format(a, f) => (a, Handler::Format(f.as_mut())),
            Plugin::Destination(a, d) => (a, Handler::Destination(d.as_mut())),
        }
    }
}

/// Serve every plugin of a package from one executable. Core's `hello` names the plugin it
/// wants; without one, the first is served.
pub fn serve_package(mut plugins: Vec<Plugin>) -> ! {
    assert!(!plugins.is_empty(), "a package serves at least one plugin");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || read_stdin(tx));
    let mut input = Input { rx };
    let mut out = Output;
    if ::log::set_logger(&FrameLogger).is_ok() {
        ::log::set_max_level(::log::LevelFilter::Debug);
    }
    let provides: Vec<PluginId> = plugins.iter().map(Plugin::id).collect();
    let chosen = loop {
        let req = match next_request(&mut input, &mut out) {
            Some(r) => r,
            None => continue,
        };
        let Request::Hello {
            min_version,
            max_version,
            plugin,
            ..
        } = req
        else {
            out.send(&Response::error("the first request must be `hello`"));
            continue;
        };
        let Some(chosen) = negotiate((min_version, max_version), (MIN_VERSION, MAX_VERSION)) else {
            out.send(&Response::VersionMismatch {
                min_version: MIN_VERSION,
                max_version: MAX_VERSION,
            });
            std::process::exit(1);
        };
        let i = match &plugin {
            None => 0,
            Some(want) => match provides.iter().position(|p| p == want) {
                Some(i) => i,
                None => {
                    let list: Vec<String> = provides.iter().map(|p| p.to_string()).collect();
                    out.send(&Response::error(format!(
                        "this executable provides {}, not {want}",
                        list.join(", ")
                    )));
                    std::process::exit(1);
                }
            },
        };
        let (about, h) = plugins[i].handler();
        let _ = shared().name.set(about.name.to_string());
        out.send(&Response::Hello {
            protocol_version: chosen,
            kind: h.kind(),
            name: about.name.to_string(),
            version: about.version.to_string(),
            capabilities: about
                .capabilities
                .iter()
                .chain(std::iter::once(&CAP_VALIDATE))
                .map(|c| c.to_string())
                .collect(),
            provides: if provides.len() > 1 {
                provides.clone()
            } else {
                Vec::new()
            },
        });
        shared().version.store(chosen, Ordering::SeqCst);
        break i;
    };
    let (about, h) = plugins[chosen].handler();
    serve(about.name, h, input, out)
}

/// The next request, or `None` after answering one that can't be read. Exits at end of input.
fn next_request(input: &mut Input, out: &mut Output) -> Option<Request> {
    let frame = match input.next() {
        Ok(f) => f,
        Err(FrameError::Eof) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e}");
            out.send(&Response::error(e.to_string()));
            std::process::exit(2);
        }
    };
    match frame {
        Frame::Json(v) => match serde_json::from_value::<Request>(v.clone()) {
            Ok(r) => Some(r),
            Err(_) => {
                let t = v.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                out.send(&Response::error(format!("unsupported request `{t}`")));
                None
            }
        },
        Frame::Arrow(_) => {
            out.send(&Response::error("unexpected Arrow frame"));
            None
        }
    }
}

/// Serve requests after the handshake.
fn serve(name: &str, mut h: Handler<'_>, mut input: Input, mut out: Output) -> ! {
    loop {
        let frame = match input.next() {
            Ok(f) => f,
            Err(FrameError::Eof) => {
                if let Handler::Source(src) = &mut h {
                    src.close();
                }
                std::process::exit(0)
            }
            Err(e) => {
                eprintln!("{e}");
                out.send(&Response::error(e.to_string()));
                std::process::exit(2);
            }
        };
        let s = shared();
        let id = match &frame {
            Frame::Json(v) => v.get("id").and_then(Value::as_u64).unwrap_or(0),
            Frame::Arrow(_) => 0,
        };
        s.current.store(id, Ordering::SeqCst);
        let req = match frame {
            Frame::Json(v) => match serde_json::from_value::<Request>(v.clone()) {
                Ok(r) => r,
                Err(_) => {
                    let t = v.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                    out.send(&Response::error(format!("unsupported request `{t}`")));
                    s.current.store(0, Ordering::SeqCst);
                    continue;
                }
            },
            Frame::Arrow(_) => {
                out.send(&Response::error("unexpected Arrow frame"));
                continue;
            }
        };
        if let Request::Hello { .. } = req {
            out.send(&Response::error("`hello` was already answered"));
            s.current.store(0, Ordering::SeqCst);
            continue;
        }
        if let Request::Close {} = req {
            if let Handler::Source(src) = &mut h {
                src.close();
            }
            out.send(&Response::Ok {});
            std::process::exit(0);
        }
        // Cancelled before it started: it never runs.
        let result = if cancelled() {
            Ok(Err("cancelled before it started".into()))
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle(&mut h, name, req, &mut input, &mut out)
            }))
        };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => out.send(&error_reply(&e)),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                out.send(&Response::Error {
                    message: format!("plugin panicked: {msg}"),
                    kind: Some("internal".into()),
                    code: None,
                });
                std::process::exit(101);
            }
        }
        s.current.store(0, Ordering::SeqCst);
    }
}

/// The declared fields' generic checks, then the plugin's own rules (when the generic ones
/// passed), with secret values redacted.
fn check_connection(
    h: &Handler<'_>,
    name: &str,
    connection: &Map<String, Value>,
    unresolved: &[String],
) -> Result<crate::connection::Checked> {
    let (kind, fields) = match h {
        Handler::Source(s) => (Kind::Source, s.connection_fields()),
        Handler::Destination(d) => (Kind::Destination, d.connection_fields()),
        Handler::Format(_) => return Err("a format has no connection settings".into()),
    };
    let mut c = crate::connection::check(kind, name, &fields, connection, unresolved);
    if c.errors.is_empty() {
        // An unresolved value arrives as `null`: set, but not known (a rule about two keys
        // going together checks `contains_key`).
        c.errors = match h {
            Handler::Source(s) => s.validate_connection(connection),
            Handler::Destination(d) => d.validate_connection(connection),
            Handler::Format(_) => Vec::new(),
        };
    }
    c.errors = crate::connection::redact(c.errors, &fields, connection);
    c.warnings = crate::connection::redact(c.warnings, &fields, connection);
    Ok(c)
}

fn handle(h: &mut Handler<'_>, name: &str, req: Request, input: &mut Input, out: &mut Output) -> Result<()> {
    if let Request::Describe {} = req {
        let connection_fields = match h {
            Handler::Source(s) => s.connection_fields(),
            Handler::Destination(d) => d.connection_fields(),
            Handler::Format(_) => Vec::new(),
        };
        let identifier_quote = match h {
            Handler::Source(s) => s.identifier_quote().map(str::to_string),
            _ => None,
        };
        let message_limit = match h {
            Handler::Destination(d) => d.message_limit(),
            _ => None,
        };
        out.send(&Response::Describe {
            connection_fields,
            option_fields: h.option_fields(),
            identifier_quote,
            message_limit,
        });
        return Ok(());
    }
    if let Request::Validate { options } = req {
        out.send(&Response::Validated {
            errors: h.check(name, &options),
            warnings: Vec::new(),
        });
        return Ok(());
    }
    if let Request::ValidateConnection {
        connection,
        unresolved,
    } = req
    {
        let c = check_connection(h, name, &connection, &unresolved)?;
        out.send(&Response::Validated {
            errors: c.errors,
            warnings: c.warnings,
        });
        return Ok(());
    }
    // The same checks before connecting: errors stop it, warnings are logged.
    if let Request::Open { connection, .. } | Request::Deliver { connection, .. } = &req {
        let c = check_connection(h, name, connection, &[])?;
        for w in &c.warnings {
            crate::log::warn!("{w}");
        }
        if !c.errors.is_empty() {
            return Err(PluginError::new(ErrorKind::Config, c.errors.join("; ")).into());
        }
    }
    let checked = match &req {
        Request::Write { options, .. } | Request::Deliver { options, .. } => h.checked(name, options),
        _ => Ok(()),
    };
    match (h, req) {
        (
            Handler::Source(s),
            Request::Open {
                connection,
                read_only,
            },
        ) => {
            s.open(&connection, read_only)?;
            out.send(&Response::Ok {});
        }
        (Handler::Source(s), Request::Load { name }) => {
            let ipc = match input.read()? {
                Frame::Arrow(ipc) => ipc,
                Frame::Json(v) => return Err(format!("expected the rows to load, got {v}").into()),
            };
            let (schema, batches) = frame::decode_batches(&ipc)?;
            let mut data = ResultSet {
                meta: ResultSetMeta {
                    name: name.clone(),
                    query: name.clone(),
                    result_index: 1,
                    anchor: None,
                    header: None,
                    columns: Default::default(),
                    autofit: None,
                },
                schema,
                first: Some(batches),
                input,
                done: false,
            };
            let r = s.load(&name, &mut data);
            data.drain()?;
            let loaded = r?;
            out.send(&Response::Loaded {
                relation: loaded.relation,
                rows: loaded.rows,
                warning: loaded.warning,
            });
        }
        (Handler::Source(s), Request::Check { sql }) => {
            s.check(&sql)?;
            out.send(&Response::Ok {});
        }
        (Handler::Source(s), Request::Execute { sql, row_limit }) => {
            let mut sink = FrameSink {
                out,
                row_limit,
                rows: 0,
                state: SinkState::Idle,
                schema: None,
                sent_any: false,
            };
            let r = s.execute(&sql, row_limit, &mut sink);
            match (r, sink.state) {
                (Err(e), _) => return Err(e),
                (Ok(()), SinkState::Idle) => {
                    return Err("the source produced neither a result nor `no_result`".into());
                }
                (Ok(()), SinkState::NoResult) => {}
                (Ok(()), SinkState::Result) => {
                    if !sink.sent_any {
                        let schema = sink.schema.clone().unwrap();
                        sink.out.batch(&RecordBatch::new_empty(schema))?;
                    }
                    let rows = sink.rows;
                    sink.out.send(&Response::ResultEnd { rows });
                }
            }
        }
        (
            Handler::Format(f),
            Request::Write {
                path,
                format,
                options,
                result_sets,
                template,
            },
        ) => {
            let req = WriteRequest {
                path,
                format,
                options,
                result_sets: result_sets.clone(),
                template,
            };
            let mut sets = ResultSets {
                metas: result_sets.into_iter(),
                input,
                warnings: Vec::new(),
            };
            let written = checked.and_then(|()| f.write(&req, &mut sets));
            // A failed write is reported at once, so core can stop streaming; this is the
            // request's one reply.
            if let Err(e) = &written {
                out.send(&error_reply(e));
            }
            // Consume whatever the format didn't read (after an error, possibly the rest of a
            // result set), so the stream stays in sync.
            loop {
                match sets.input.read()? {
                    Frame::Json(v) if v.get("type").and_then(Value::as_str) == Some("finish") => break,
                    Frame::Json(v) if v.get("type").and_then(Value::as_str) == Some("result_set_end") => {}
                    Frame::Arrow(_) => {}
                    other => return Err(format!("expected `finish`, got {other:?}").into()),
                }
            }
            if let Ok(files) = written {
                out.send(&Response::Written {
                    files,
                    warnings: sets.warnings,
                });
            }
        }
        (
            Handler::Destination(d),
            Request::Deliver {
                local_path,
                remote_path,
                files,
                connection,
                options,
                message,
            },
        ) => {
            checked?;
            let files = match (local_path, files.is_empty(), &message) {
                (Some(local), true, None) => vec![DeliveryFile {
                    local: PathBuf::from(local),
                    remote: remote_path,
                }],
                (None, false, _) | (None, true, Some(_)) => files
                    .into_iter()
                    .map(|f| DeliveryFile {
                        local: PathBuf::from(f.local_path),
                        remote: f.remote_path,
                    })
                    .collect(),
                _ => return Err("`deliver` needs exactly one of `local_path` or `files`".into()),
            };
            let delivery = Delivery {
                files,
                connection,
                options,
                message,
            };
            crate::delivery::take_attempts();
            let location = match &delivery.message {
                Some(m) => d.deliver_message(&delivery, m)?,
                None => d.deliver_files(&delivery)?,
            };
            let attempts = Some(crate::delivery::take_attempts()).filter(|n| *n > 1);
            out.send(&Response::Delivered { location, attempts });
        }
        (h, req) => {
            let t = serde_json::to_value(&req)
                .ok()
                .and_then(|v| v.get("type").cloned())
                .unwrap_or_default();
            return Err(format!("a {} plugin doesn't handle {t} requests", h.kind()).into());
        }
    }
    Ok(())
}

#[derive(PartialEq)]
enum SinkState {
    Idle,
    NoResult,
    Result,
}

struct FrameSink<'a> {
    out: &'a mut Output,
    row_limit: Option<u64>,
    rows: u64,
    state: SinkState,
    schema: Option<SchemaRef>,
    sent_any: bool,
}

impl ResultSink for FrameSink<'_> {
    fn no_result(&mut self, rows_affected: Option<u64>) -> Result<()> {
        if self.state != SinkState::Idle {
            return Err("`no_result` after the result started".into());
        }
        self.state = SinkState::NoResult;
        self.out.send(&Response::NoResult { rows_affected });
        Ok(())
    }

    fn begin(&mut self, schema: SchemaRef) -> Result<()> {
        if self.state != SinkState::Idle {
            return Err("a statement can only produce one result".into());
        }
        self.state = SinkState::Result;
        self.out.send(&Response::Result {
            columns: schema.fields().iter().map(|f| f.name().clone()).collect(),
        });
        self.schema = Some(schema);
        Ok(())
    }

    fn batch(&mut self, b: RecordBatch) -> Result<bool> {
        if self.state != SinkState::Result {
            return Err("`batch` before `begin`".into());
        }
        if self.row_limit.is_some_and(|l| self.rows >= l) {
            return Ok(false);
        }
        let b = match self.row_limit {
            Some(l) if self.rows + b.num_rows() as u64 > l => b.slice(0, (l - self.rows) as usize),
            _ => b,
        };
        if b.num_rows() > 0 || !self.sent_any {
            self.rows += b.num_rows() as u64;
            self.out.batch(&b)?;
            self.sent_any = true;
        }
        Ok(self.row_limit.is_none_or(|l| self.rows < l))
    }
}

/// The highest version in both ranges.
fn negotiate(core: (u32, u32), ours: (u32, u32)) -> Option<u32> {
    let (lo, hi) = (core.0.max(ours.0), core.1.min(ours.1));
    (lo <= hi).then_some(hi)
}

/// Read a string field from a connection map.
pub fn conn_str<'a>(c: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    c.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Read a required string field from a connection map.
pub fn conn_required<'a>(c: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    conn_str(c, key).ok_or_else(|| format!("the profile output needs a `{key}` field").into())
}

/// Read a flag that may be written as a boolean or a string.
pub fn conn_bool(c: &Map<String, Value>, key: &str) -> Option<bool> {
    match c.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => Some(matches!(s.as_str(), "true" | "yes" | "1")),
        _ => None,
    }
}
