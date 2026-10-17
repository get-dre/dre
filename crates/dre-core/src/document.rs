//! The context of a document template: what core sends the `document` plugin (the html and pdf
//! formats) in a format `write` request's `template`, and the Jinja environment the plugin
//! builds from it. Rendering happens in the plugin; core only runs the queries and sends this.
//!
//! The plugin builds its environment with [`environment`], so `var()`, `run.*`, `target.name`,
//! the date functions and the number filters behave exactly as in core's own templates (one
//! implementation, no copy to drift), and `results.<query>` is the same value messages read.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::RecordBatch;
use chrono::{DateTime, NaiveDate, Utc};
use minijinja::{Environment, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as Json};

use crate::dates::{Calendar, WeekNumbering, WeekStart};
use crate::message::QueryResult;
use crate::numbers::Locale;
use crate::render::RunContext;

/// The formats rendered from a document template, which get a [`Context`] in every `write`.
pub const FORMATS: &[&str] = &["html", "pdf"];

/// A format `write` request's `template` for the html and pdf formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// The template file, resolved by core ([`crate::template_file`]), absolute. `None`: the
    /// output names no template, and the plugin uses its default (a table per query).
    #[serde(default)]
    pub file: Option<String>,
    /// The project root: includes and assets are looked up inside it, never outside.
    pub project_root: String,
    pub run: RunInfo,
    /// The Binding's vars with the schedule's on top: what `var()` reads after `--var`.
    #[serde(default)]
    pub vars: JsonMap<String, Json>,
    /// `--var` values, as typed on the command line: `var()` reads them first.
    #[serde(default)]
    pub cli_vars: BTreeMap<String, String>,
    /// The Binding's `locale:` tag, for the number filters.
    pub locale: String,
    /// A per-row output's driver row: `each.<column>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<JsonMap<String, Json>>,
}

/// What `validate` sends the plugin for an html or pdf output, so `dre validate` checks its
/// template: the file, the project root, and the output's queries (what `results.<name>` may
/// name), in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    #[serde(default)]
    pub file: Option<String>,
    pub project_root: String,
    #[serde(default)]
    pub queries: Vec<String>,
}

/// `run.*` and `target.name`, as plain values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub report: String,
    #[serde(default)]
    pub set: Option<String>,
    pub target: String,
    #[serde(default)]
    pub schedule: Option<String>,
    pub date: NaiveDate,
    pub now: DateTime<Utc>,
    #[serde(default)]
    pub scheduled_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub week_start: String,
    pub week_numbering: String,
}

impl Context {
    /// Build the context of a run.
    pub fn new(
        file: Option<&std::path::Path>,
        project_root: &std::path::Path,
        run: &RunContext,
        vars: JsonMap<String, Json>,
        cli_vars: BTreeMap<String, String>,
    ) -> Context {
        Context {
            file: file.map(|f| f.to_string_lossy().into_owned()),
            project_root: project_root.to_string_lossy().into_owned(),
            run: RunInfo {
                report: run.report.clone(),
                set: run.set.clone(),
                target: run.target.clone(),
                schedule: run.schedule.clone(),
                date: run.date,
                now: run.now,
                scheduled_at: run.scheduled_at,
                timezone: run.calendar.tz.name().to_string(),
                week_start: match run.calendar.week_start {
                    WeekStart::Monday => "monday",
                    WeekStart::Sunday => "sunday",
                }
                .into(),
                week_numbering: match run.calendar.numbering {
                    WeekNumbering::Iso => "iso",
                    WeekNumbering::Us => "us",
                }
                .into(),
            },
            vars,
            cli_vars,
            locale: run.locale.tag.to_string(),
            each: None,
        }
    }

    /// The run's calendar: its timezone and week settings.
    pub fn calendar(&self) -> Result<Calendar, String> {
        Ok(Calendar {
            tz: crate::dates::parse_tz(&self.run.timezone)?,
            week_start: WeekStart::parse(&self.run.week_start)
                .ok_or_else(|| format!("unknown week start `{}`", self.run.week_start))?,
            numbering: WeekNumbering::parse(&self.run.week_numbering)
                .ok_or_else(|| format!("unknown week numbering `{}`", self.run.week_numbering))?,
        })
    }

    pub fn locale(&self) -> Result<Locale, String> {
        Locale::parse(&self.locale)
    }

    /// The `run.*` context core's templates read.
    pub fn run_context(&self) -> Result<RunContext, String> {
        Ok(RunContext {
            report: self.run.report.clone(),
            set: self.run.set.clone(),
            target: self.run.target.clone(),
            schedule: self.run.schedule.clone(),
            date: self.run.date,
            now: self.run.now,
            scheduled_at: self.run.scheduled_at,
            calendar: self.calendar()?,
            locale: self.locale()?,
        })
    }
}

/// A Jinja environment with everything core's templates read: `var()`, `run.*`, `target.name`,
/// the date functions and the number filters. Undefined values are errors. The caller adds its
/// loader, escaping and its own filters.
pub fn environment(ctx: &Context) -> Result<Environment<'static>, String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    crate::render::add_common(
        &mut env,
        ctx.run_context()?,
        ctx.vars.clone(),
        ctx.cli_vars.clone(),
    );
    if let Some(each) = &ctx.each {
        env.add_global("each", Value::from(minijinja::value::Serde(each.clone())));
    }
    Ok(env)
}

/// One query's result from its Arrow batches, as `results.<query>` holds it (every row).
pub fn query_result(columns: Vec<String>, batches: &[RecordBatch], cal: Calendar) -> QueryResult {
    let mut rows = Vec::new();
    for b in batches {
        rows.extend(crate::values::typed_rows(b, cal));
    }
    QueryResult {
        columns: Arc::new(columns),
        row_count: rows.len() as u64,
        rows,
    }
}

/// `results`: each query's result by name, iterating in the order given (the YAML's).
pub fn results_value(results: Vec<(String, Arc<QueryResult>)>) -> Value {
    Value::from_object(Ordered(
        results.into_iter().map(|(name, r)| (name, r.value())).collect(),
    ))
}

/// A map that iterates its keys in insertion order.
#[derive(Debug)]
struct Ordered(Vec<(String, Value)>);

impl minijinja::value::Object for Ordered {
    fn repr(self: &Arc<Self>) -> minijinja::value::ObjectRepr {
        minijinja::value::ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn enumerate(self: &Arc<Self>) -> minijinja::value::Enumerator {
        minijinja::value::Enumerator::Values(self.0.iter().map(|(n, _)| Value::from(n.as_str())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            file: Some("/p/s.html".into()),
            project_root: "/p".into(),
            run: RunInfo {
                report: "statement".into(),
                set: Some("acme".into()),
                target: "prod".into(),
                schedule: None,
                date: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
                now: "2026-10-01T02:00:00Z".parse().unwrap(),
                scheduled_at: None,
                timezone: "Australia/Sydney".into(),
                week_start: "monday".into(),
                week_numbering: "iso".into(),
            },
            vars: JsonMap::from_iter([("client".to_string(), Json::from("Acme"))]),
            cli_vars: BTreeMap::from([("terms".to_string(), "30".to_string())]),
            locale: "de-DE".into(),
            each: None,
        }
    }

    #[test]
    fn round_trips_as_json() {
        let c = ctx();
        let back: Context = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn environment_reads_what_core_templates_read() {
        let env = environment(&ctx()).unwrap();
        let out = env
            .render_str(
                "{{ run.report }}|{{ run.set }}|{{ target.name }}|{{ run.date }}|{{ run.timezone }}|{{ var('client') }}|{{ var('terms') + 1 }}|{{ 1234.5 | number(2) }}|{{ var('x', 'd') }}",
                (),
            )
            .unwrap();
        assert_eq!(
            out,
            "statement|acme|prod|2026-09-30|Australia/Sydney|Acme|31|1.234,50|d"
        );
        let e = env.render_str("{{ var('missing') }}", ()).unwrap_err();
        assert!(e.to_string().contains("no value and no default"), "{e}");
        let e = env.render_str("{{ env_var('HOME') }}", ()).unwrap_err();
        assert!(e.to_string().contains("can't be used in a document"), "{e}");
    }

    #[test]
    fn results_keep_the_given_order() {
        let r = |n: u64| {
            Arc::new(QueryResult {
                columns: Arc::new(vec!["a".into()]),
                rows: Vec::new(),
                row_count: n,
            })
        };
        let v = results_value(vec![("zeta".into(), r(1)), ("alpha".into(), r(2))]);
        let env = Environment::new();
        let out = env
            .render_str("{% for k in results %}{{ k }}={{ results[k].row_count }} {% endfor %}{{ results.alpha.row_count }}", minijinja::context! { results => v })
            .unwrap();
        assert_eq!(out, "zeta=1 alpha=2 2");
    }
}
