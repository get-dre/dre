//! What every DRE template reads: `var()`, `raise_error()`, the date functions, the number
//! filters, `run.*` and `target.name`. Core's renderers and the `document` plugin build on it, so
//! a document template reads exactly what a message template reads.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use minijinja::value::{Object, Value};
use minijinja::{Environment, Error, ErrorKind};
use serde_json::{Map as JsonMap, Value as Json};

use crate::Env;
use crate::dates::{Calendar, Date, DateTime};
use crate::secrets::Secrets;

/// Ambient `run.*` context.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub report: String,
    pub set: Option<String>,
    /// The run's target (environment): `run.target` and `target.name`.
    pub target: String,
    /// The schedule's name under `dre run --schedule`, else `None`.
    pub schedule: Option<String>,
    pub date: NaiveDate,
    /// When the run started, or the instant it was scheduled for: `run.now`.
    pub now: chrono::DateTime<Utc>,
    /// `DRE_RUN_AT`: `run.scheduled_at`.
    pub scheduled_at: Option<chrono::DateTime<Utc>>,
    /// The run's timezone and week settings.
    pub calendar: Calendar,
    /// The Binding's `locale:`, for the number filters.
    pub locale: crate::numbers::Locale,
}

/// `var()` (`--var` first, then the Binding's vars, then the default) and `env_var()`.
/// A `--var` value as YAML 1.2's core rules read it: `true`/`false`, `null`/`~`, integers
/// (no leading zeros), floats, and flow lists and maps (`[a, b]`, `{k: v}`) are typed; anything
/// else stays a string (`yes`, `NO`, `2026-01-31`, `010`, `1.0.0`). Quoting forces a string
/// (`'"false"'`), and an empty value is the empty string.
pub fn cli_var_value(s: &str) -> Json {
    let t = s.trim();
    let int = regex_lite(t, |c, i| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')));
    match t {
        "" => return Json::String(String::new()),
        "true" | "True" | "TRUE" => return Json::Bool(true),
        "false" | "False" | "FALSE" => return Json::Bool(false),
        "null" | "Null" | "NULL" | "~" => return Json::Null,
        _ => {}
    }
    if int {
        let digits = t.trim_start_matches(['-', '+']);
        if !digits.is_empty()
            && !(digits.len() > 1 && digits.starts_with('0'))
            && let Ok(n) = t.parse::<i64>()
        {
            return Json::from(n);
        }
        return Json::String(s.to_string());
    }
    let float = t.trim_start_matches(['-', '+']);
    let is_float = !float.is_empty()
        && float
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'))
        && float
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
        && float.chars().filter(|c| *c == '.').count() <= 1
        && !float.starts_with("00")
        && !(float.starts_with('0') && float.len() > 1 && float.as_bytes()[1].is_ascii_digit());
    if is_float
        && (float.contains('.') || float.contains(['e', 'E']))
        && let Ok(f) = t.parse::<f64>()
        && let Some(n) = serde_json::Number::from_f64(f)
    {
        return Json::Number(n);
    }
    let quoted = (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2);
    let flow = (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('{') && t.ends_with('}'));
    if (quoted || flow)
        && let Ok(v) = serde_saphyr::from_str::<Json>(t)
    {
        return v;
    }
    Json::String(s.to_string())
}

/// Whether every character passes `ok(c, index)`.
fn regex_lite(s: &str, ok: impl Fn(char, usize) -> bool) -> bool {
    !s.is_empty() && s.chars().enumerate().all(|(i, c)| ok(c, i))
}

pub fn add_vars(
    env: &mut Environment<'static>,
    vars: JsonMap<String, Json>,
    cli: BTreeMap<String, String>,
    process: Env,
) {
    env.add_function(
        "var",
        move |name: String, default: Option<Value>| -> Result<Value, Error> {
            if let Some(v) = cli.get(&name) {
                return Ok(Value::from(minijinja::value::Serde(cli_var_value(v))));
            }
            if let Some(v) = vars.get(&name) {
                return Ok(Value::from(minijinja::value::Serde(v)));
            }
            default.ok_or_else(|| {
                Error::new(
                    ErrorKind::UndefinedError,
                    format!("`var('{name}')` has no value and no default"),
                )
            })
        },
    );
    add_env_var(env, process);
}

/// Register the shared environment lookup, preserving the Jinja type of a default.
pub fn add_env_var(env: &mut Environment<'static>, process: Env) {
    env.add_function("env_var", move |name: String, default: Option<Value>| -> Result<Value, Error> {
        match process.var(&name) {
            Some(v) => Ok(Value::from(v)),
            None => default.ok_or_else(|| {
                Error::new(
                    ErrorKind::UndefinedError,
                    format!("`env_var('{name}')`: environment variable `{name}` is not set and no default is given"),
                )
            }),
        }
    });
}

/// What every DRE template reads, for an environment built outside core's renderers (the
/// `document` plugin's html and pdf templates): `var()`, `raise_error()`, the date functions,
/// the number filters, `run.*`, `target.name` and Python-style methods. `env_var()` isn't
/// available: a document is data that's delivered, and environment variables hold secrets.
pub fn add_common(
    env: &mut Environment<'static>,
    context: RunContext,
    vars: JsonMap<String, Json>,
    cli_vars: BTreeMap<String, String>,
    secrets: &Secrets,
) {
    // NULL prints as nothing, as in core's own templates.
    env.set_formatter(|out, state, value| {
        if value.is_none() {
            return Ok(());
        }
        minijinja::escape_formatter(out, state, value)
    });
    crate::mutable::register(env);
    add_vars(env, vars, cli_vars, Env::default());
    env.add_function(
        "env_var",
        |name: String, _default: Option<Value>| -> Result<Value, Error> {
            Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("`env_var('{name}')` can't be used in a document template; pass the value as a var"),
            ))
        },
    );
    env.add_function("raise_error", |message: String| -> Result<Value, Error> {
        Err(Error::new(ErrorKind::InvalidOperation, message))
    });
    crate::dates::register(env, context.calendar, context.date);
    crate::numbers::register(env, context.locale, secrets);
    env.add_global("target", Value::from_object(Target(context.target.clone())));
    env.add_global("run", Value::from_object(Run(context)));
}

#[derive(Debug)]
pub struct Run(pub RunContext);

impl Object for Run {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let c = &self.0;
        Some(match key.as_str()? {
            "report" => Value::from(c.report.clone()),
            "set" => c.set.clone().map(Value::from).unwrap_or(Value::from(())),
            "target" => Value::from(c.target.clone()),
            "profile" => removed("run.profile", "`connection.name` (the query's connection)"),
            "source_type" => removed("run.source_type", "`connection.type` (the query's connection)"),
            "schedule" => c.schedule.clone().map(Value::from).unwrap_or(Value::from(())),
            "date" => Date::value(c.date, c.calendar),
            "now" => DateTime::now(c.now, c.calendar),
            "scheduled_at" => c
                .scheduled_at
                .map(|t| DateTime::now(t, c.calendar))
                .unwrap_or(Value::from(())),
            "timezone" => Value::from(c.calendar.tz.name()),
            _ => return None,
        })
    }

    fn call_method(
        self: &Arc<Self>,
        _: &mut minijinja::State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "date_format" => {
                let fmt = args.first().and_then(|a| a.as_str()).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        "`run.date_format()` takes a strftime format string",
                    )
                })?;
                let mut out = String::new();
                use std::fmt::Write;
                write!(out, "{}", self.0.date.format(fmt)).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("invalid date format `{fmt}`"),
                    )
                })?;
                Ok(Value::from(out))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }
}

/// An error value naming a removed template name and what to write instead.
pub fn removed(name: &str, instead: &str) -> Value {
    Value::from(Error::new(
        ErrorKind::InvalidOperation,
        format!("`{name}` was removed in DRE 0.2: use {instead}"),
    ))
}

/// `connection.*` in DRE 0.2.
#[derive(Debug)]
pub struct Target(pub String);

impl Object for Target {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        Some(match k {
            "name" => Value::from(self.0.clone()),
            "type" => removed("target.type", "`connection.type` (the query's connection)"),
            "profile" => removed("target.profile", "`connection.name` (the query's connection)"),
            _ => removed(
                &format!("target.{k}"),
                &format!(
                    "`connection.{k}` (the query's connection) or `profile('<name>').{k}`; `target` is now only the environment, `target.name`"
                ),
            ),
        })
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
