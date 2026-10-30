//! A Binding's connections: one source session per connection, opened on first use and held
//! until the Binding ends, and what templates read through them (`run_query()`, `columns()`,
//! lookups, `connection.*` and `profile()`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::datatypes::SchemaRef;
use dre_protocol::host::{Execution, HostError, LogSink};
use dre_protocol::{CAP_LOAD, CAP_READ_ONLY};
use serde_json::{Map as JsonMap, Value as Json};

use crate::context::Context;
use crate::lookups::Table;
use crate::plugin_host::{Described, Open, Plugin, PluginHost};
use crate::profiles::{Entry, LOCAL_TYPE, ProfileTarget, Profiles, Role, render_connection};
use crate::project::{PluginKind, Project};
use crate::render::{Column, Connection, Connections, QueryRows, QueryRunner};
use crate::secrets::Secrets;
use crate::sqlsplit::{self, StatementKind};

pub(super) fn masked_source_error(secrets: &Secrets, error: &HostError, sensitive: bool) -> String {
    let error = error.to_string();
    secrets.mask_source_error(&error, sensitive).into_owned()
}

pub(super) fn masked_sql_summary(secrets: &Secrets, sql: &str, max: usize, sensitive: bool) -> String {
    if sensitive {
        crate::secrets::MASK.to_string()
    } else {
        dre_protocol::util::summarize(&secrets.mask(sql), max)
    }
}

pub(super) fn protected_sql<'a>(
    secrets: &Secrets,
    sql: &'a str,
    sensitive: bool,
) -> std::borrow::Cow<'a, str> {
    if sensitive {
        std::borrow::Cow::Borrowed(crate::secrets::MASK)
    } else {
        secrets.mask(sql)
    }
}

/// The source session, started on first use (rendering may need it for `run_query()`).
pub(super) struct Session {
    host: Arc<dyn PluginHost>,
    secrets: Secrets,
    path: Result<crate::plugins::Located, String>,
    connection: Result<JsonMap<String, Json>, String>,
    cwd: PathBuf,
    log: LogSink,
    /// Whether plugin diagnostics for the active statement must be hidden rather than exactly
    /// masked. Connection diagnostics always use exact-value masking.
    opaque_log: Arc<AtomicBool>,
    /// Unmanaged reports run read-only unless they create temp objects.
    unmanaged: bool,
    read_only: bool,
    /// A lookup was loaded into a temp table, so the session must stay open as it is.
    pub(super) loaded: bool,
    proc_: Option<Box<dyn Plugin>>,
}

impl Session {
    fn new(
        ctx: &Context,
        path: Result<crate::plugins::Located, String>,
        connection: Result<JsonMap<String, Json>, String>,
        cwd: PathBuf,
        log: LogSink,
        unmanaged: bool,
    ) -> Session {
        let opaque_log = Arc::new(AtomicBool::new(false));
        let protect = Arc::clone(&opaque_log);
        let downstream = log;
        let secrets = ctx.secrets().clone();
        let log: LogSink = Arc::new(move |plugin, line| {
            let line = secrets.mask_source_error(line, protect.load(Ordering::SeqCst));
            downstream(plugin, &line);
        });
        Session {
            host: ctx.host().clone(),
            secrets: ctx.secrets().clone(),
            path,
            connection,
            cwd,
            log,
            opaque_log,
            unmanaged,
            read_only: unmanaged,
            loaded: false,
            proc_: None,
        }
    }

    pub(super) fn want_read_only(&mut self, ro: bool) {
        if self.read_only != ro {
            // Reopen if a session was already started in the other mode.
            self.close();
            self.read_only = ro;
        }
    }

    pub(super) fn get(&mut self) -> Result<&mut dyn Plugin, String> {
        if self.proc_.is_none() {
            let plugin = self.path.clone()?;
            let connection = self.connection.clone()?;
            let mut p = self
                .host
                .start(&plugin, self.log.clone(), Some(&self.cwd))
                .map_err(|e| masked_source_error(&self.secrets, &e, false))?;
            let open = if self.unmanaged && self.read_only && p.has(CAP_READ_ONLY) {
                Open::ReadOnly
            } else {
                Open::ReadWrite
            };
            p.open(connection, open).map_err(|e| {
                let protected = masked_source_error(&self.secrets, &e, false);
                format!("can't open the source connection: {protected}")
            })?;
            self.proc_ = Some(p);
        }
        Ok(self.proc_.as_deref_mut().expect("the process was started above"))
    }

    pub(super) fn statement(&mut self, sensitive: bool) -> Result<(&mut dyn Plugin, OpaqueLogScope), String> {
        let opaque_log = Arc::clone(&self.opaque_log);
        let process = self.get()?;
        let stderr = if sensitive { process.stderr_sync() } else { None };
        let scope = OpaqueLogScope::new(opaque_log, sensitive, stderr);
        Ok((process, scope))
    }

    pub(super) fn close(&mut self) {
        if let Some(p) = self.proc_.take() {
            p.close();
        }
    }
}

/// Hides a sensitive statement's plugin diagnostics while it runs. The plugin's stderr is logged
/// from another thread, so the scope waits for the lines the plugin wrote before showing
/// diagnostics again; if the plugin keeps writing, they stay hidden until the next statement.
#[must_use = "dropping the scope shows diagnostics again at once"]
pub(super) struct OpaqueLogScope(Arc<AtomicBool>, Option<dre_protocol::host::StderrSync>);

impl OpaqueLogScope {
    fn new(active: Arc<AtomicBool>, sensitive: bool, stderr: Option<dre_protocol::host::StderrSync>) -> Self {
        active.store(sensitive, Ordering::SeqCst);
        Self(active, stderr)
    }
}

impl Drop for OpaqueLogScope {
    fn drop(&mut self) {
        if let Some(stderr) = &self.1
            && !stderr.wait(Duration::from_secs(1))
        {
            return;
        }
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The Binding's connections: each one's session, opened when a query first needs it and held
/// until the report ends.
pub(super) struct Pool {
    pub(super) connections: Arc<ProfileConnections>,
    cwd: PathBuf,
    log: LogSink,
    unmanaged: bool,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Session>>>>,
}

impl Pool {
    pub(super) fn new(
        connections: Arc<ProfileConnections>,
        cwd: PathBuf,
        log: LogSink,
        unmanaged: bool,
    ) -> Pool {
        Pool {
            connections,
            cwd,
            log,
            unmanaged,
            sessions: Mutex::default(),
        }
    }

    /// A connection's settings for this run.
    fn target(&self, name: &str) -> Result<ProfileTarget, String> {
        let profiles = &self.connections.profiles;
        match profiles.entry(Role::Connection, name) {
            Entry::Use(o) => Ok(o.clone()),
            Entry::Unknown => Err(format!(
                "connection `{name}` isn't under `{}:` in {}",
                profiles.section_key(Role::Connection),
                profiles.path.display()
            )),
            // A connection never has `deliver: false`.
            Entry::Missing | Entry::Nowhere => Err(profiles.missing_entry(Role::Connection, name)),
        }
    }

    /// A connection's plugin type.
    pub(super) fn kind(&self, name: &str) -> Result<String, String> {
        Ok(self.target(name)?.kind)
    }

    pub(super) fn session(&self, name: &str) -> Result<Arc<Mutex<Session>>, String> {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            return Ok(s.clone());
        }
        let output = self.target(name)?;
        let id = crate::project::PluginId::new(PluginKind::Source, output.kind.clone());
        let ctx = &self.connections.ctx;
        let path = ctx
            .host()
            .locate(&self.connections.root, &self.connections.plugins, &id)
            .map_err(String::from);
        let session = Arc::new(Mutex::new(Session::new(
            ctx,
            path,
            render_connection(&output, ctx.env()),
            self.cwd.clone(),
            self.log.clone(),
            self.unmanaged,
        )));
        self.sessions
            .lock()
            .unwrap()
            .insert(name.to_string(), session.clone());
        Ok(session)
    }

    pub(super) fn close_all(&self) {
        for s in self.sessions.lock().unwrap().values() {
            if let Ok(mut s) = s.lock() {
                s.close();
            }
        }
    }
}

/// `run_query()`, `columns()` and lookups for one renderer: on the named connection's session,
/// else on the renderer's own (`default`).
pub(super) struct PoolRunner {
    pub(super) pool: Arc<Pool>,
    pub(super) secrets: Secrets,
    pub(super) default: Option<String>,
    pub(super) log: LogSink,
    /// Set when a query the templates sent failed in the database: the render failed because
    /// it needed the data, which compiling can't check.
    pub(super) failed: Arc<AtomicBool>,
}

impl PoolRunner {
    fn session(&self, profile: Option<&str>) -> Result<Arc<Mutex<Session>>, String> {
        let name = profile.or(self.default.as_deref()).ok_or(
            "no connection here: pass `profile=`, or give the report a `profile:` (this isn't a query's SQL)",
        )?;
        self.pool.session(name)
    }
}

impl QueryRunner for PoolRunner {
    fn run_query(&self, sql: &str, max_rows: u64, profile: Option<&str>) -> Result<QueryRows, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = self.secrets.contains_secret(sql);
        // An unmanaged report may only read, and run_query() runs before the file's own
        // statements are checked, so it gets the same rule up front.
        if s.unmanaged
            && let Some(bad) = sqlsplit::split(sql)
                .into_iter()
                .find(|st| sqlsplit::classify(&st.text) != StatementKind::Read)
        {
            return Err(format!(
                "run_query() in an unmanaged report may only read, but got `{}`; give the report a YAML to declare it",
                masked_sql_summary(&self.secrets, &bad.text, 60, sensitive)
            ));
        }
        (self.log)("run_query()", &protected_sql(&self.secrets, sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut out = QueryRows::default();
        let mut too_many = false;
        let exec = p
            .execute(sql, Some(max_rows + 1), &mut |schema, batch| {
                if out.columns.is_empty() {
                    out.columns = schema.fields().iter().map(|f| f.name().clone()).collect();
                }
                if out.rows.len() as u64 + batch.num_rows() as u64 > max_rows {
                    too_many = true;
                    return Ok(());
                }
                out.rows.extend(crate::values::batch_rows(&batch));
                Ok(())
            })
            .map_err(|e| {
                self.failed.store(true, Ordering::SeqCst);
                format!(
                    "run_query() failed: {}",
                    masked_source_error(&self.secrets, &e, sensitive)
                )
            })?;
        if too_many {
            return Err(format!(
                "run_query() returned more than {max_rows} rows; it's meant for small lookups — raise the cap with `run_query(sql, max_rows=N)` or `run_query_max_rows` in dre_project.yml"
            ));
        }
        if let Execution::Result { schema, .. } = exec
            && out.columns.is_empty()
        {
            out.columns = schema.fields().iter().map(|f| f.name().clone()).collect();
        }
        Ok(out)
    }

    fn columns(&self, sql: &str, profile: Option<&str>) -> Result<Vec<Column>, String> {
        let columns = self.columns_of(sql, profile);
        if columns.is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
        columns
    }

    fn load(&self, name: &str, table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        self.load_table(name, table)
    }
}

impl PoolRunner {
    fn columns_of(&self, sql: &str, profile: Option<&str>) -> Result<Vec<Column>, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = self.secrets.contains_secret(sql);
        (self.log)("columns()", &protected_sql(&self.secrets, sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut schema: Option<SchemaRef> = None;
        let exec = p
            .execute(sql, Some(1), &mut |sch, _| {
                schema.get_or_insert_with(|| sch.clone());
                Ok(())
            })
            .map_err(|e| masked_source_error(&self.secrets, &e, sensitive))?;
        if let Execution::Result { schema: sch, .. } = exec {
            schema.get_or_insert(sch);
        }
        let schema = schema.ok_or("the query returned no result set")?;
        Ok(schema
            .fields()
            .iter()
            .map(|f| Column {
                name: f.name().clone(),
                data_type: f.data_type().to_string(),
            })
            .collect())
    }

    fn load_table(&self, name: &str, table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        let session = self.session(None)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        // A temp table is allowed even in an unmanaged report; it needs a writable session.
        s.want_read_only(false);
        if !s.get()?.has(CAP_LOAD) {
            return Ok(None);
        }
        let batch = table.to_batch()?;
        (self.log)(
            &format!("lookup `{name}`"),
            &format!(
                "-- {} rows loaded through the plugin's `load` request",
                batch.num_rows()
            ),
        );
        let r = s.get()?.load(name, batch).map_err(|e| e.to_string())?;
        s.loaded = true;
        Ok(Some(r))
    }
}

/// `connection`, `destination` and `profile()` from `profiles.yml`, for the run's target. A
/// field is secret when the plugin's `describe` says so; when the plugin can't be asked, when its
/// name looks like one; and always when its value comes from a `DRE_SECRET_*` variable.
pub(super) struct ProfileConnections {
    /// The run's plugin host, secrets and `describe` cache.
    ctx: Context,
    /// Each profile's entry for the run comes from here.
    profiles: Profiles,
    root: PathBuf,
    plugins: Vec<crate::project::PluginRequirement>,
    log: LogSink,
}

impl ProfileConnections {
    /// `project`'s profiles for the run, with `ctx`'s plugin host; plugin log lines go to `log`.
    pub(super) fn new(ctx: &Context, project: &Project, log: LogSink) -> ProfileConnections {
        ProfileConnections {
            ctx: ctx.clone(),
            profiles: project.profiles().clone(),
            root: project.root().to_path_buf(),
            plugins: project.plugins().to_vec(),
            log,
        }
    }

    /// How many Bindings may use `connection` at once: its entry's `threads:` (default 1), and
    /// 1 whatever it says when the source plugin's `describe` names a single-writer field the
    /// entry sets to a file (DuckDB's `path`), as only one process can write it.
    pub(super) fn threads(&self, connection: &str) -> usize {
        let Some(t) = self.profiles.target(Role::Connection, connection) else {
            return 1;
        };
        let n = match t.fields.get("threads") {
            Some(Json::Number(n)) => n.as_u64().unwrap_or(1),
            Some(Json::String(s)) => s.trim().parse().unwrap_or(1),
            _ => 1,
        } as usize;
        if n <= 1 {
            return 1;
        }
        let single = self
            .described(Role::Connection, &t.kind)
            .and_then(|d| d.single_writer)
            .is_some_and(|w| w.applies(&t.fields));
        if single { 1 } else { n }
    }
}

const SECRET_NAME_WORDS: &[&str] = &["password", "secret", "token", "key", "credential"];

impl ProfileConnections {
    fn view(&self, role: Role, name: &str) -> Result<Connection, String> {
        let target = self.profiles.target_of(role, name);
        let out: ProfileTarget = match self.profiles.entry(role, name) {
            Entry::Use(o) => o.clone(),
            Entry::Nowhere => {
                return Err(format!(
                    "destination `{name}`: `{target}` delivers nowhere (`deliver: false`), so it has no settings"
                ));
            }
            Entry::Missing => return Err(self.profiles.missing_entry(role, name)),
            Entry::Unknown => {
                return Err(format!(
                    "no {} profile `{name}` in {}",
                    role.as_str(),
                    self.profiles.path.display()
                ));
            }
        };
        let mut secrets: Vec<String> = out
            .fields
            .iter()
            .filter(|(_, v)| v.as_str().is_some_and(|s| s.contains(crate::secrets::PREFIX)))
            .map(|(k, _)| k.clone())
            .collect();
        match self.described(role, &out.kind) {
            Some(d) => secrets.extend(d.secrets),
            None => secrets.extend(
                out.fields
                    .keys()
                    .filter(|k| {
                        let k = k.to_lowercase();
                        SECRET_NAME_WORDS.iter().any(|w| k.contains(w))
                    })
                    .cloned(),
            ),
        }
        self.ctx.secrets().register_secret_fields(&out.fields, &secrets);
        let fields = render_connection(&out, self.ctx.env())?;
        Ok(Connection {
            profile: name.to_string(),
            target,
            kind: out.kind.clone(),
            fields,
            secrets,
        })
    }

    /// The plugin's `describe` reply, asked once per plugin type in a run.
    pub(super) fn described(&self, role: Role, kind: &str) -> Option<Described> {
        let key = (self.root.clone(), role, kind.to_string());
        self.ctx.described(key, || self.describe(role, kind))
    }

    /// Ask the plugin for its `describe` reply; `None` when it can't be asked.
    fn describe(&self, role: Role, kind: &str) -> Option<Described> {
        let plugin_kind = match role {
            Role::Connection => PluginKind::Source,
            Role::Destination => PluginKind::Destination,
        };
        {
            if kind == LOCAL_TYPE {
                return Some(Described {
                    secrets: Vec::new(),
                    identifier_quote: None,
                    string_literal: None,
                    capabilities: Vec::new(),
                    message_limit: None,
                    single_writer: None,
                });
            }
            let id = crate::project::PluginId::new(plugin_kind, kind);
            let host = self.ctx.host();
            let plugin = host.locate(&self.root, &self.plugins, &id).ok()?;
            let mut p = host.start(&plugin, self.log.clone(), Some(&self.root)).ok()?;
            let capabilities = p.info().capabilities.clone();
            let d = p.describe().ok();
            p.close();
            let d = d?;
            Some(Described {
                secrets: d
                    .connection_fields
                    .into_iter()
                    .filter(|f| f.secret)
                    .map(|f| f.name)
                    .collect(),
                identifier_quote: d.identifier_quote,
                string_literal: d.string_literal,
                capabilities,
                message_limit: d.message_limit,
                single_writer: d.single_writer,
            })
        }
    }
}

impl Connections for ProfileConnections {
    fn profile(&self, name: &str, role: Option<&str>) -> Result<Connection, String> {
        let role = match role {
            Some("connection") => Role::Connection,
            Some(_) => Role::Destination,
            None => {
                let c = self.profiles.get(Role::Connection, name).is_some();
                let d = self.profiles.get(Role::Destination, name).is_some();
                match (c, d) {
                    (true, true) => {
                        return Err(format!(
                            "`{name}` is both a connection and a destination profile; say which with `profile('{name}', role='connection')` or `role='destination'`"
                        ));
                    }
                    (true, false) => Role::Connection,
                    _ => Role::Destination,
                }
            }
        };
        self.view(role, name)
    }

    fn identifier_quote(&self, kind: &str) -> Result<Option<String>, String> {
        match self.described(Role::Connection, kind) {
            Some(d) => Ok(d.identifier_quote),
            None => Err(format!(
                "the `{kind}` source plugin couldn't be asked for its identifier quote character"
            )),
        }
    }

    fn string_literal(&self, kind: &str) -> Result<Option<String>, String> {
        Ok(self
            .described(Role::Connection, kind)
            .and_then(|d| d.string_literal))
    }
}
