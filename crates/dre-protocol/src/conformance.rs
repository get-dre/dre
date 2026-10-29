//! The protocol conformance suite. The cases are data, in `conformance/protocol.json` (a request
//! sequence and the properties its replies must have), so a runner in any language can check a
//! plugin against them; this module is the Rust runner, driving a plugin binary over stdin and
//! stdout. `conformance/delivery.json` holds the delivery rules both SDKs share, checked by
//! [`delivery`]. Every first-party plugin runs the suite in its tests. A package executable
//! (`dre-plugin-<package>`) is checked once per plugin it provides.
//!
//! Every message a plugin sends is also checked against the message schema ([`crate::schema`]).
//! A case that doesn't apply to a plugin (another kind, a capability it doesn't advertise, a
//! session the caller didn't describe) is listed as skipped, with the reason.

pub mod delivery;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::frame::{self, Frame};
use crate::host::{LogSink, PluginProcess};
use crate::msg::{ERROR_KINDS, Envelope, Response};
use crate::{
    CAPABILITIES, Kind, MAX_VERSION, MIN_VERSION, PluginId, parse_executable_name,
    parse_package_executable_name,
};

/// The protocol cases, as published.
pub const PROTOCOL_CASES: &str = include_str!("../conformance/protocol.json");

/// How long a plugin may take over one reply.
const TIMEOUT: Duration = Duration::from_secs(20);

/// One case's outcome for one plugin.
#[derive(Debug)]
pub struct Check {
    /// The plugin checked (`<kind>/<name>`), or the package for checks of the package itself.
    pub plugin: String,
    /// The case's name, from `conformance/protocol.json`.
    pub name: String,
    /// True for a case that passed or didn't apply.
    pub passed: bool,
    /// Why the case failed; empty otherwise.
    pub detail: String,
    /// Why the case didn't apply to this plugin; `None` when it ran.
    pub skipped: Option<String>,
}

impl Check {
    /// A case that ran, passing or failing with the reason.
    fn ran(plugin: &str, name: &str, r: Result<(), String>) -> Check {
        Check {
            plugin: plugin.to_string(),
            name: name.to_string(),
            passed: r.is_ok(),
            detail: r.err().unwrap_or_default(),
            skipped: None,
        }
    }

    /// A case that doesn't apply to the plugin.
    fn skipped(plugin: &str, name: &str, why: String) -> Check {
        Check {
            plugin: plugin.to_string(),
            name: name.to_string(),
            passed: true,
            detail: String::new(),
            skipped: Some(why),
        }
    }
}

/// What the session cases run: a connection that opens, and statements by role. The roles the
/// cases use are `rows` (a result set of at least 3 rows), `empty` (a result set of no rows),
/// `no_result` (a statement without a result set), `error` (a statement that fails) and `slow`
/// (a statement that runs for at least a few seconds, until cancelled).
#[derive(Debug, Clone, Default)]
pub struct Session {
    connection: Map<String, Value>,
    statements: BTreeMap<String, String>,
}

impl Session {
    /// A session opened with `connection` (a profile target's fields), with no statements yet.
    #[must_use]
    pub fn new(connection: Map<String, Value>) -> Session {
        Session {
            connection,
            statements: BTreeMap::new(),
        }
    }

    /// The statement for `role` (`rows`, `empty`, `no_result`, `error`, `slow`).
    #[must_use]
    pub fn statement(mut self, role: &str, sql: &str) -> Session {
        self.statements.insert(role.into(), sql.into());
        self
    }
}

/// How to run the suite.
#[derive(Debug, Clone, Default)]
pub struct Options {
    env: Vec<(String, String)>,
    session: Option<Session>,
}

impl Options {
    /// Extra environment variables for every plugin process (for plugins whose capabilities
    /// depend on their environment).
    #[must_use]
    pub fn env(mut self, env: &[(&str, &str)]) -> Options {
        self.env
            .extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self
    }

    /// Run the session cases (sources) with this session.
    #[must_use]
    pub fn session(mut self, session: Session) -> Options {
        self.session = Some(session);
        self
    }
}

#[derive(Debug, Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    /// `package`: run once per package executable, not per plugin.
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    kinds: Vec<Kind>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    session: Vec<String>,
    steps: Vec<Map<String, Value>>,
}

/// The parsed protocol cases.
///
/// # Panics
///
/// Panics if the published cases don't parse (a test keeps them parsing).
fn cases() -> Vec<Case> {
    serde_json::from_str::<Cases>(PROTOCOL_CASES)
        .expect("conformance/protocol.json parses")
        .cases
}

/// Run every case against the plugin executable at `path`.
#[must_use]
pub fn run(path: &Path) -> Vec<Check> {
    run_with(path, &Options::default())
}

/// Like [`run`], with extra environment variables for every plugin process.
#[must_use]
pub fn run_with_env(path: &Path, env: &[(&str, &str)]) -> Vec<Check> {
    run_with(path, &Options::default().env(env))
}

/// Run every case against the plugin executable at `path`, as `options` say.
#[must_use]
pub fn run_with(path: &Path, options: &Options) -> Vec<Check> {
    let file = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().to_string());
    let cases = cases();
    if let Some((kind, name)) = parse_executable_name(&file) {
        let id = PluginId::new(kind, name);
        return run_plugin(path, &id, false, &cases, options);
    }
    let fail = |detail: String| {
        vec![Check::ran(
            &file,
            "the executable is named dre-plugin-<package> or dre-<kind>-<name>",
            Err(detail),
        )]
    };
    let Some(package) = parse_package_executable_name(&file) else {
        return fail(format!("`{file}` is neither"));
    };
    let provides = match greet(path, None, options) {
        // A package serving one plugin may leave `provides` out: it serves `kind`/`name`.
        Ok(hello) => match hello
            .get("provides")
            .cloned()
            .map(serde_json::from_value::<Vec<PluginId>>)
        {
            Some(Ok(p)) if !p.is_empty() => p,
            Some(Err(e)) => return fail(format!("`provides` isn't a list of plugins: {e}")),
            _ => match serde_json::from_value::<PluginId>(json!(format!(
                "{}/{}",
                hello["kind"].as_str().unwrap_or_default(),
                hello["name"].as_str().unwrap_or_default()
            ))) {
                Ok(id) => vec![id],
                Err(e) => return fail(format!("the hello reply has no valid kind and name: {e}")),
            },
        },
        Err(e) => return fail(format!("the handshake failed: {e}")),
    };
    let mut out = Vec::new();
    for case in cases.iter().filter(|c| c.scope.as_deref() == Some("package")) {
        let vars = Vars::new(None, options);
        out.push(finish(&package, case, run_case(path, None, case, &vars, options)));
    }
    for id in &provides {
        out.extend(run_plugin(path, id, true, &cases, options));
    }
    out
}

/// Every case for one plugin. `ask`: name it in the handshake (a package's plugins).
fn run_plugin(path: &Path, id: &PluginId, ask: bool, cases: &[Case], options: &Options) -> Vec<Check> {
    let asked = ask.then_some(id);
    let capabilities: Vec<String> = match greet(path, asked, options) {
        Ok(h) => match h.get("capabilities").cloned().map(serde_json::from_value) {
            Some(Ok(c)) => c,
            None => Vec::new(),
            Some(Err(e)) => {
                let why = format!("`capabilities` isn't a list of names: {e}");
                return vec![Check::ran(
                    &id.to_string(),
                    "the plugin starts and answers hello",
                    Err(why),
                )];
            }
        },
        // Without a handshake, which cases apply is unknown: fail, rather than skip them.
        Err(e) => {
            return vec![Check::ran(
                &id.to_string(),
                "the plugin starts and answers hello",
                Err(e),
            )];
        }
    };
    let mut out = Vec::new();
    for case in cases.iter().filter(|c| c.scope.is_none()) {
        if let Some(reason) = not_applicable(case, id.kind, &capabilities, options) {
            out.push(Check::skipped(&id.to_string(), &case.name, reason));
            continue;
        }
        let vars = Vars::new(Some(id), options);
        out.push(finish(
            &id.to_string(),
            case,
            run_case(path, asked, case, &vars, options),
        ));
    }
    out
}

fn finish(plugin: &str, case: &Case, r: Result<(), String>) -> Check {
    Check::ran(plugin, &case.name, r)
}

/// Why `case` doesn't apply, or `None` when it does.
fn not_applicable(case: &Case, kind: Kind, capabilities: &[String], options: &Options) -> Option<String> {
    if !case.kinds.is_empty() && !case.kinds.contains(&kind) {
        return Some(format!("for {:?} plugins", case.kinds));
    }
    if let Some(c) = case.requires.iter().find(|c| !capabilities.contains(c)) {
        return Some(format!("the plugin doesn't advertise `{c}`"));
    }
    if case.session.is_empty() {
        return None;
    }
    let Some(session) = &options.session else {
        return Some("needs a session (conformance::Options::session)".into());
    };
    case.session
        .iter()
        .find(|s| !session.statements.contains_key(*s))
        .map(|s| format!("the session has no `{s}` statement"))
}

/// Start the plugin and return its hello reply (for its capabilities and `provides`).
fn greet(path: &Path, asked: Option<&PluginId>, options: &Options) -> Result<Map<String, Value>, String> {
    let mut d = Driver::spawn(path, options)?;
    d.write_json(&hello(asked, &Map::new()))?;
    let reply = d.reply()?;
    if reply["type"] != "hello" {
        return Err(format!("expected a hello reply, got {reply}"));
    }
    Ok(reply.as_object().cloned().unwrap_or_default())
}

/// The `hello` request: the SDK's version range unless `with` overrides it.
fn hello(asked: Option<&PluginId>, with: &Map<String, Value>) -> Value {
    let mut h = json!({
        "type": "hello",
        "min_version": MIN_VERSION,
        "max_version": MAX_VERSION,
        "core_version": "conformance",
    });
    if let Some(id) = asked {
        h["plugin"] = json!(id.to_string());
    }
    for (k, v) in with {
        h[k] = v.clone();
    }
    h
}

/// The values a case's `${...}` placeholders stand for.
struct Vars {
    values: BTreeMap<String, Value>,
    tmp: PathBuf,
}

impl Vars {
    fn new(id: Option<&PluginId>, options: &Options) -> Vars {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("dre-conformance-{}-{n}", std::process::id()));
        let mut values = BTreeMap::new();
        if let Some(id) = id {
            values.insert("kind".into(), json!(id.kind.as_str()));
            values.insert("name".into(), json!(id.name));
            values.insert("plugin".into(), json!(id.to_string()));
        }
        values.insert("tmp".into(), json!(tmp.to_string_lossy()));
        values.insert("min_version".into(), json!(MIN_VERSION));
        values.insert("max_version".into(), json!(MAX_VERSION));
        values.insert("far_version".into(), json!(MAX_VERSION + 1000));
        values.insert("capabilities".into(), json!(CAPABILITIES));
        values.insert("error_kinds".into(), json!(ERROR_KINDS));
        let session = options.session.clone().unwrap_or_default();
        values.insert("connection".into(), Value::Object(session.connection));
        for (role, sql) in session.statements {
            values.insert(format!("sql.{role}"), json!(sql));
        }
        Vars { values, tmp }
    }
}

/// `v` with every `${name}` replaced: a string that is only a placeholder becomes its value,
/// whatever its type; elsewhere the value is spliced in as text.
fn substitute(v: &Value, vars: &Vars) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) => {
            if let Some(name) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}'))
                && !name.contains('}')
            {
                return vars
                    .values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("unknown placeholder `{s}`"));
            }
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(start) = rest.find("${") {
                let end = rest[start..]
                    .find('}')
                    .ok_or_else(|| format!("unclosed placeholder in `{s}`"))?;
                let name = &rest[start + 2..start + end];
                let value = vars
                    .values
                    .get(name)
                    .ok_or_else(|| format!("unknown placeholder `${{{name}}}`"))?;
                out.push_str(&rest[..start]);
                match value {
                    Value::String(t) => out.push_str(t),
                    other => out.push_str(&other.to_string()),
                }
                rest = &rest[start + end + 1..];
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, vars)).collect::<Result<_, _>>()?),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), substitute(x, vars)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// Whether `value` (`None`: missing) has the properties `m` describes. See
/// `docs/protocol.md` for the matcher language.
pub(crate) fn matches(value: Option<&Value>, m: &Value) -> Result<(), String> {
    let show = || value.map_or_else(|| "nothing".to_string(), |v| v.to_string());
    match m {
        Value::Object(ops) if ops.keys().any(|k| k.starts_with('$')) => {
            for (op, arg) in ops {
                operator(value, op, arg).map_err(|e| format!("{e} (got {})", show()))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            let Some(Value::Object(o)) = value else {
                return Err(format!("expected an object, got {}", show()));
            };
            for (k, sub) in fields {
                matches(o.get(k), sub).map_err(|e| format!("`{k}`: {e}"))?;
            }
            Ok(())
        }
        Value::Array(items) => {
            let Some(Value::Array(a)) = value else {
                return Err(format!("expected an array, got {}", show()));
            };
            if a.len() != items.len() {
                return Err(format!("expected {} items, got {}", items.len(), show()));
            }
            for (i, (x, sub)) in a.iter().zip(items).enumerate() {
                matches(Some(x), sub).map_err(|e| format!("[{i}]: {e}"))?;
            }
            Ok(())
        }
        scalar => match value {
            Some(v) if same(v, scalar) => Ok(()),
            _ => Err(format!("expected {scalar}, got {}", show())),
        },
    }
}

/// JSON equality, with numbers compared by value (`1` and `1.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn operator(value: Option<&Value>, op: &str, arg: &Value) -> Result<(), String> {
    let present = value.filter(|v| !v.is_null());
    let number = |what: &str| -> Result<(f64, f64), String> {
        let v = present.and_then(Value::as_f64).ok_or("expected a number")?;
        let a = arg.as_f64().ok_or_else(|| format!("`{what}` takes a number"))?;
        Ok((v, a))
    };
    match op {
        "$absent" => match (present, arg.as_bool()) {
            (None, Some(true)) | (Some(_), Some(false)) => Ok(()),
            _ => Err(format!(
                "expected {}",
                if arg == &json!(true) {
                    "no value"
                } else {
                    "a value"
                }
            )),
        },
        "$any" => value.map(|_| ()).ok_or_else(|| "expected a value".into()),
        "$one_of" => {
            let alts = arg.as_array().ok_or("`$one_of` takes a list")?;
            let mut why = Vec::new();
            for alt in alts {
                match matches(value, alt) {
                    Ok(()) => return Ok(()),
                    Err(e) => why.push(e),
                }
            }
            Err(format!("matches none of: {}", why.join("; ")))
        }
        "$type" => {
            let want = arg.as_str().ok_or("`$type` takes a name")?;
            let ok = match (want, present) {
                ("string", Some(v)) => v.is_string(),
                ("number", Some(v)) => v.is_number(),
                ("integer", Some(v)) => v.is_u64() || v.is_i64(),
                ("boolean", Some(v)) => v.is_boolean(),
                ("array", Some(v)) => v.is_array(),
                ("object", Some(v)) => v.is_object(),
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(format!("expected a {want}"))
            }
        }
        "$nonempty" => match present {
            Some(Value::String(s)) if !s.is_empty() => Ok(()),
            Some(Value::Array(a)) if !a.is_empty() => Ok(()),
            _ => Err("expected a non-empty string or list".into()),
        },
        "$len" => {
            let n = match present {
                Some(Value::String(s)) => s.chars().count(),
                Some(Value::Array(a)) => a.len(),
                _ => return Err("expected a string or list".into()),
            };
            match arg.as_u64() {
                Some(want) if want as usize == n => Ok(()),
                _ => Err(format!("expected length {arg}, got {n}")),
            }
        }
        "$contains" => match present {
            Some(Value::String(s)) => match arg.as_str() {
                Some(sub) if s.contains(sub) => Ok(()),
                _ => Err(format!("expected text containing {arg}")),
            },
            Some(Value::Array(a)) => {
                if a.iter().any(|x| matches(Some(x), arg).is_ok()) {
                    Ok(())
                } else {
                    Err(format!("expected an item matching {arg}"))
                }
            }
            _ => Err("expected a string or list".into()),
        },
        "$every" => match present {
            Some(Value::Array(a)) => {
                for (i, x) in a.iter().enumerate() {
                    matches(Some(x), arg).map_err(|e| format!("[{i}]: {e}"))?;
                }
                Ok(())
            }
            _ => Err("expected a list".into()),
        },
        "$in" => {
            let set = arg.as_array().ok_or("`$in` takes a list")?;
            match present {
                Some(v) if set.iter().any(|x| same(x, v)) => Ok(()),
                _ => Err(format!("expected one of {arg}")),
            }
        }
        "$min" => {
            let (v, a) = number("$min")?;
            if v >= a {
                Ok(())
            } else {
                Err(format!("expected at least {a}"))
            }
        }
        "$lt" => {
            let (v, a) = number("$lt")?;
            if v < a {
                Ok(())
            } else {
                Err(format!("expected less than {a}"))
            }
        }
        "$file_exists" => match present.and_then(Value::as_str) {
            Some(p) if Path::new(p).is_file() == (arg == &json!(true)) => Ok(()),
            _ => Err(format!(
                "expected a path that {} a file",
                if arg == &json!(true) { "is" } else { "isn't" }
            )),
        },
        other => Err(format!("unknown matcher `{other}`")),
    }
}

/// One plugin process, driven frame by frame.
struct Driver {
    p: PluginProcess,
    /// The last reply read, for a `result_set` step's columns.
    last: Value,
}

impl Driver {
    fn spawn(path: &Path, options: &Options) -> Result<Driver, String> {
        let env: Vec<(&str, &str)> = options
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let quiet: LogSink = Arc::new(|_, _| {});
        let p = PluginProcess::spawn_env(path, quiet, &env).map_err(|e| e.to_string())?;
        Ok(Driver { p, last: Value::Null })
    }

    fn write_json(&mut self, v: &Value) -> Result<(), String> {
        let mut bytes = Vec::new();
        frame::write_json(&mut bytes, v).map_err(|e| e.to_string())?;
        self.p.write_raw(&bytes).map_err(|e| e.to_string())
    }

    /// The next message that isn't a `log` or `progress`, checked against the schema.
    fn reply(&mut self) -> Result<Value, String> {
        loop {
            match self.p.recv_frame(TIMEOUT).map_err(|e| e.to_string())? {
                Frame::Arrow(_) => return Err("Arrow data where a reply was expected".into()),
                Frame::Json(v) => {
                    conforms(&v)?;
                    if !is_chatter(&v) {
                        self.last = v.clone();
                        return Ok(v);
                    }
                }
            }
        }
    }
}

/// A `log` or `progress` message: a plugin may send one at any time, before any reply.
fn is_chatter(v: &Value) -> bool {
    v["type"] == "log" || v["type"] == "progress"
}

/// Whether a message a plugin sent matches the message schema.
fn conforms(v: &Value) -> Result<(), String> {
    serde_json::from_value::<Envelope<Response>>(v.clone())
        .map(|_| ())
        .map_err(|e| format!("{v} doesn't match the message schema: {e}"))
}

fn run_case(
    path: &Path,
    asked: Option<&PluginId>,
    case: &Case,
    vars: &Vars,
    options: &Options,
) -> Result<(), String> {
    std::fs::create_dir_all(&vars.tmp).map_err(|e| e.to_string())?;
    let r = run_steps(path, asked, case, vars, options);
    let _ = std::fs::remove_dir_all(&vars.tmp);
    r
}

fn run_steps(
    path: &Path,
    asked: Option<&PluginId>,
    case: &Case,
    vars: &Vars,
    options: &Options,
) -> Result<(), String> {
    let mut d = Driver::spawn(path, options)?;
    if !case.steps.first().is_some_and(|s| s.contains_key("hello")) {
        d.write_json(&hello(asked, &Map::new()))?;
        let r = d.reply().map_err(|e| format!("handshake: {e}"))?;
        if r["type"] != "hello" {
            return Err(format!("handshake: expected a hello reply, got {r}"));
        }
    }
    for (i, step) in case.steps.iter().enumerate() {
        let Value::Object(step) = substitute(&Value::Object(step.clone()), vars)? else {
            unreachable!("substitute keeps an object an object");
        };
        run_step(&mut d, asked, &step).map_err(|e| format!("step {}: {e}", i + 1))?;
    }
    Ok(())
}

fn run_step(d: &mut Driver, asked: Option<&PluginId>, step: &Map<String, Value>) -> Result<(), String> {
    if let Some(h) = step.get("hello") {
        d.write_json(&hello(asked, h.as_object().unwrap_or(&Map::new())))?;
    } else if let Some(v) = step.get("send") {
        d.write_json(v)?;
    } else if let Some(hex) = step.get("send_raw") {
        let bytes = decode_hex(hex.as_str().unwrap_or_default())?;
        // A plugin may end on a malformed frame before reading all of it.
        let _ = d.p.write_raw(&bytes);
    } else if let Some(spec) = step.get("send_arrow") {
        let ipc = arrow_frame(spec)?;
        let mut bytes = Vec::new();
        frame::write_arrow(&mut bytes, &ipc).map_err(|e| e.to_string())?;
        d.p.write_raw(&bytes).map_err(|e| e.to_string())?;
    } else if step.contains_key("close_input") {
        d.p.close_input();
    } else if let Some(spec) = step.get("result_set") {
        return result_set(d, spec);
    } else if let Some(m) = step.get("exit") {
        let status =
            d.p.wait_exit(TIMEOUT)
                .ok_or("still running after it should have exited")?;
        let code = status.code().map_or(Value::Null, Value::from);
        return matches(Some(&code), m).map_err(|e| format!("exit code: {e}"));
    } else {
        return Err(format!("unknown step {}", Value::Object(step.clone())));
    }
    let Some(m) = step.get("expect") else {
        return Ok(());
    };
    let or_exit = step.get("or_exit") == Some(&json!(true));
    match d.reply() {
        Ok(v) => matches(Some(&v), m).map_err(|e| format!("reply {v}: {e}")),
        Err(_) if or_exit && d.p.wait_exit(Duration::from_secs(5)).is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Read one result set: Arrow frames (at least one, with the `result` reply's columns), then a
/// `result_end` whose `rows` is the rows sent.
fn result_set(d: &mut Driver, spec: &Value) -> Result<(), String> {
    let columns = d.last["columns"].clone();
    let mut frames = 0;
    let mut rows: u64 = 0;
    loop {
        match d.p.recv_frame(TIMEOUT).map_err(|e| e.to_string())? {
            Frame::Arrow(ipc) => {
                let (schema, batches) = frame::decode_batches(&ipc).map_err(|e| e.to_string())?;
                let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
                if json!(names) != columns {
                    return Err(format!(
                        "the Arrow schema has columns {names:?}, the result reply {columns}"
                    ));
                }
                frames += 1;
                rows += batches.iter().map(|b| b.num_rows() as u64).sum::<u64>();
            }
            Frame::Json(v) => {
                conforms(&v)?;
                if is_chatter(&v) {
                    continue;
                }
                if frames == 0 {
                    return Err(format!("no Arrow frame (carrying the schema) before {v}"));
                }
                matches(Some(&v), &json!({"type": "result_end", "id": spec["id"]}))?;
                if v["rows"].as_u64() != Some(rows) {
                    return Err(format!("`result_end` says {} rows, {rows} were sent", v["rows"]));
                }
                return matches(Some(&json!(rows)), &spec["rows"]).map_err(|e| format!("rows: {e}"));
            }
        }
    }
}

/// An Arrow IPC stream of `rows` null rows with the given columns.
fn arrow_frame(spec: &Value) -> Result<Vec<u8>, String> {
    use arrow::datatypes::{DataType, Field, Schema};
    let mut fields = Vec::new();
    for c in spec["columns"].as_array().ok_or("`send_arrow` needs `columns`")? {
        let t = match c["type"].as_str() {
            Some("int64") => DataType::Int64,
            Some("float64") => DataType::Float64,
            Some("utf8") => DataType::Utf8,
            Some("boolean") => DataType::Boolean,
            other => return Err(format!("unknown column type {other:?}")),
        };
        fields.push(Field::new(c["name"].as_str().unwrap_or_default(), t, true));
    }
    let schema = Arc::new(Schema::new(fields));
    let rows = spec["rows"].as_u64().unwrap_or(0) as usize;
    let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
    for (f, c) in schema
        .fields()
        .iter()
        .zip(spec["columns"].as_array().into_iter().flatten())
    {
        columns.push(match c["values"].as_array() {
            None => arrow::array::new_null_array(f.data_type(), rows),
            Some(v) if v.len() != rows => {
                return Err(format!("`{}` has {} values for {rows} rows", f.name(), v.len()));
            }
            Some(v) => match f.data_type() {
                DataType::Int64 => Arc::new(arrow::array::Int64Array::from_iter(v.iter().map(Value::as_i64))),
                DataType::Float64 => {
                    Arc::new(arrow::array::Float64Array::from_iter(v.iter().map(Value::as_f64)))
                }
                DataType::Boolean => Arc::new(arrow::array::BooleanArray::from_iter(
                    v.iter().map(Value::as_bool),
                )),
                _ => Arc::new(arrow::array::StringArray::from_iter(v.iter().map(Value::as_str))),
            },
        });
    }
    let batch = arrow::array::RecordBatch::try_new(schema, columns).map_err(|e| e.to_string())?;
    frame::encode_batch(&batch).map_err(|e| e.to_string())
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap_or("?"), 16).map_err(|e| e.to_string()))
        .collect()
}

/// Panic with a readable report unless every check passes. For use in plugin tests.
///
/// # Panics
///
/// Panics when a check fails: that is how it fails the test.
pub fn assert_conforms(path: &Path) {
    assert_conforms_with(path, &Options::default());
}

/// [`assert_conforms`], run as `options` say.
///
/// # Panics
///
/// Panics when a check fails: that is how it fails the test.
pub fn assert_conforms_with(path: &Path, options: &Options) {
    let checks = run_with(path, options);
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("  ✗ {}: {}: {}", c.plugin, c.name, c.detail))
        .collect();
    assert!(
        failed.is_empty(),
        "{} fails protocol conformance:\n{}",
        path.display(),
        failed.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_cases_parse_and_name_known_capabilities() {
        let cases = cases();
        assert!(cases.len() > 20);
        for c in &cases {
            assert!(
                matches!(c.scope.as_deref(), None | Some("package")),
                "{}: scope",
                c.name
            );
            for r in &c.requires {
                assert!(CAPABILITIES.contains(&r.as_str()), "{}: {r}", c.name);
            }
        }
    }

    #[test]
    fn matchers_check_properties_not_whole_messages() {
        let reply = json!({"type": "error", "id": 4, "message": "no", "kind": "query", "extra": 1});
        let ok = |m: Value| matches(Some(&reply), &m);
        assert!(ok(json!({"type": "error", "id": 4})).is_ok());
        assert!(ok(json!({"code": {"$absent": true}})).is_ok());
        assert!(ok(json!({"kind": {"$in": ["query", "auth"]}})).is_ok());
        assert!(ok(json!({"kind": {"$in": ["auth"]}})).is_err());
        assert!(ok(json!({"message": {"$len": 2}, "id": {"$min": 4, "$lt": 5}})).is_ok());
        assert!(ok(json!({"$one_of": [{"type": "ok"}, {"type": "error"}]})).is_ok());
        assert!(ok(json!({"type": "ok"})).is_err());
        let list = json!(["a/b", "c"]);
        assert!(matches(Some(&list), &json!({"$contains": {"$contains": "/"}})).is_ok());
        assert!(matches(Some(&list), &json!({"$every": {"$contains": "/"}})).is_err());
    }

    #[test]
    fn placeholders_take_their_values_or_splice_as_text() {
        let session = Session::new(Map::new()).statement("rows", "rows 3");
        let vars = Vars::new(
            Some(&PluginId::new(Kind::Format, "csv")),
            &Options::default().session(session),
        );
        let v = substitute(
            &json!({"v": "${max_version}", "p": "${tmp}/x.${name}", "sql": "${sql.rows}"}),
            &vars,
        )
        .unwrap();
        assert_eq!(v["v"], json!(MAX_VERSION));
        assert!(v["p"].as_str().unwrap().ends_with("/x.csv"));
        assert_eq!(v["sql"], "rows 3");
        assert!(substitute(&json!("${nope}"), &vars).is_err());
    }
}
