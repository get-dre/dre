//! Rendering a document template: the Jinja environment (core's context and filters, plus
//! `date`, `table()` and template lookup for includes), the results, error messages that name
//! the template line and the available columns, and `validate`'s checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dre_core::document::{Check, Context};
use dre_core::message::QueryResult;
use dre_core::numbers::Locale;
use minijinja::value::{Enumerator, Object, ObjectRepr, Value};
use minijinja::{Environment, Error, ErrorKind};
use regex::Regex;

/// The name of the built-in template, used when an output names none.
const DEFAULT_NAME: &str = "<default template>";
/// A table per query, under the report's name.
const DEFAULT_TEMPLATE: &str = r#"<!doctype html>
<html lang="{{ locale }}">
<head>
<meta charset="utf-8">
<title>{{ run.report }}{% if run.set %} ({{ run.set }}){% endif %}</title>
<style>
body { font-size: 10pt; }
h1 { font-size: 16pt; }
h2 { font-size: 12pt; margin-top: 1.5em; }
table.dre-table { border-collapse: collapse; width: 100%; }
table.dre-table th, table.dre-table td { border-bottom: 1px solid #ccc; padding: 3px 6px; text-align: left; }
table.dre-table td.num { text-align: right; }
</style>
</head>
<body>
<h1>{{ run.report }}{% if run.set %} ({{ run.set }}){% endif %}</h1>
<p>{{ run.date }}</p>
{% for name in results %}
<h2>{{ name }}</h2>
{{ table(results[name]) }}
{% endfor %}
</body>
</html>
"#;
/// Marks a name the include lookup couldn't resolve: the message follows.
const UNRESOLVED: &str = "\u{0}unresolved:";

/// The output's query results, in YAML order, with each query's column `format`s.
#[derive(Default)]
pub struct Data {
    /// Rows across every query.
    pub rows: u64,
    queries: Vec<Query>,
}

struct Query {
    name: String,
    result: Arc<QueryResult>,
    /// Column -> its YAML `format` (an Excel format code).
    formats: BTreeMap<String, String>,
}

impl Data {
    pub fn push(&mut self, name: String, result: Arc<QueryResult>, formats: BTreeMap<String, String>) {
        self.queries.push(Query {
            name,
            result,
            formats,
        });
    }

    /// `results`: each query's result, iterating in YAML order.
    fn value(&self) -> Value {
        Value::from_object(Results(
            self.queries
                .iter()
                .map(|q| {
                    let v = Value::from_object(Res {
                        name: q.name.clone(),
                        inner: q.result.clone().value(),
                        formats: q.formats.clone(),
                    });
                    (q.name.clone(), v)
                })
                .collect(),
        ))
    }

    /// "results.a (x, y); results.b (z)", for errors.
    fn available(&self) -> String {
        if self.queries.is_empty() {
            return "the output has no queries".into();
        }
        self.queries
            .iter()
            .map(|q| format!("results.{} ({})", q.name, q.result.columns.join(", ")))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `results`, in YAML order.
#[derive(Debug)]
struct Results(Vec<(String, Value)>);

impl Object for Results {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(self.0.iter().map(|(n, _)| Value::from(n.as_str())).collect())
    }
}

/// One query's result: core's message value, plus its name and column formats for `table()`.
#[derive(Debug)]
struct Res {
    name: String,
    inner: Value,
    formats: BTreeMap<String, String>,
}

impl Object for Res {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        self.inner.get_item(key).ok().filter(|v| !v.is_undefined())
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Str(&["value", "first", "rows", "row_count", "columns", "sets"])
    }
}

/// A rendered document.
pub struct Rendered {
    pub html: String,
    /// The template's folder: relative assets resolve from here.
    pub base_dir: PathBuf,
    pub warnings: Vec<String>,
}

/// Render the output's template (or the default one) with its results.
pub fn render(ctx: &Context, data: &Data) -> Result<Rendered, String> {
    let root = PathBuf::from(&ctx.project_root);
    let locale = ctx.locale()?;
    let env = environment(ctx, &root, locale)?;
    let (name, base_dir) = match &ctx.file {
        Some(f) => (f.clone(), Path::new(f).parent().unwrap_or(&root).to_path_buf()),
        None => (DEFAULT_NAME.to_string(), root.clone()),
    };
    let tmpl = env.get_template(&name).map_err(|e| message(&e, &root, None))?;
    let globals = minijinja::context! {
        results => data.value(),
        locale => locale.tag,
    };
    let html = tmpl.render(globals).map_err(|e| message(&e, &root, Some(data)))?;
    Ok(Rendered {
        html,
        base_dir,
        warnings: Vec::new(),
    })
}

/// The environment: core's (`var()`, `run.*`, `target.name`, dates, number filters), with
/// HTML escaping, the `date` filter, `table()`, and includes looked up inside the project.
fn environment(ctx: &Context, root: &Path, locale: Locale) -> Result<Environment<'static>, String> {
    let mut env = dre_core::document::environment(ctx)?;
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    let r = root.to_path_buf();
    env.set_path_join_callback(move |name, parent| {
        let from = if parent == DEFAULT_NAME {
            r.clone()
        } else {
            Path::new(parent)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| r.clone())
        };
        match dre_core::template_file::resolve(&r, &from, name) {
            Ok(p) => p.to_string_lossy().into_owned().into(),
            Err(e) => format!("{UNRESOLVED}{e}").into(),
        }
    });
    let r = root.to_path_buf();
    // Assets resolve from the main template's folder; an included template's own relative
    // references are rebased onto it, so a shared `base.html` can link its logo next to itself.
    let main_dir = ctx
        .file
        .as_deref()
        .and_then(|f| Path::new(f).parent())
        .unwrap_or(root)
        .to_path_buf();
    let main = ctx.file.clone();
    env.set_loader(move |name| {
        if name == DEFAULT_NAME {
            return Ok(Some(DEFAULT_TEMPLATE.to_string()));
        }
        if let Some(e) = name.strip_prefix(UNRESOLVED) {
            return Err(Error::new(ErrorKind::InvalidOperation, e.to_string()));
        }
        let p = Path::new(name);
        if !p.is_absolute() || !inside(&r, p) {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("`{name}` isn't a template file inside the project"),
            ));
        }
        let src = std::fs::read_to_string(p)
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("can't read {name}: {e}")))?;
        if main.as_deref() == Some(name) {
            return Ok(Some(src));
        }
        let dir = p.parent().unwrap_or(&r);
        Ok(Some(crate::assets::rebase(&src, dir, &main_dir)))
    });
    env.add_filter("date", |v: Value, fmt: Option<String>| -> Result<Value, Error> {
        date_filter(v, fmt.as_deref())
    });
    env.add_function(
        "table",
        move |v: Value, columns: Option<Vec<String>>| -> Result<Value, Error> { table(&v, columns, locale) },
    );
    Ok(env)
}

/// Whether `p` is `root` or inside it, after resolving `..` and links.
pub fn inside(root: &Path, p: &Path) -> bool {
    match (root.canonicalize(), p.canonicalize()) {
        (Ok(r), Ok(p)) => p.starts_with(r),
        _ => false,
    }
}

/// An error as one sentence: the template (relative to the project), its line, what went wrong,
/// and for an undefined value the queries and columns there are.
fn message(e: &Error, root: &Path, data: Option<&Data>) -> String {
    let mut deepest: &Error = e;
    while let Some(s) = std::error::Error::source(deepest).and_then(|s| s.downcast_ref::<Error>()) {
        deepest = s;
    }
    let file = deepest
        .name()
        .map(|n| shown(root, n))
        .unwrap_or_else(|| "the template".into());
    let at = match deepest.line() {
        Some(l) => format!("{file}, line {l}"),
        None => file,
    };
    let detail = deepest
        .detail()
        .map(str::to_string)
        .unwrap_or_else(|| deepest.kind().to_string());
    let mut msg = format!("{at}: {detail}");
    if deepest.kind() == ErrorKind::UndefinedError
        && let Some(d) = data
    {
        msg.push_str(&format!(". Available: {}", d.available()));
    }
    msg
}

fn shown(root: &Path, name: &str) -> String {
    if name == DEFAULT_NAME {
        return name.into();
    }
    Path::new(name)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| name.to_string())
}

// ---------------------------------------------------------------------------------------------
// `date` and `table()`
// ---------------------------------------------------------------------------------------------

/// `value | date(fmt)`: a date or timestamp in an Excel-style format (`dd/mm/yyyy`, `d mmm yyyy`,
/// `yyyy-mm-dd hh:mm`); default `yyyy-mm-dd`. Timestamps are in the run's timezone already.
fn date_filter(v: Value, fmt: Option<&str>) -> Result<Value, Error> {
    if v.is_none() || v.is_undefined() {
        return Ok(Value::from(""));
    }
    let strf = excel_date_format(fmt.unwrap_or("yyyy-mm-dd"));
    if v.as_object().is_some() {
        return format_object(&v, &strf).map(Value::from);
    }
    let s = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return Ok(Value::from(d.format(&strf).to_string()));
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&s) {
        return Ok(Value::from(t.format(&strf).to_string()));
    }
    Err(Error::new(
        ErrorKind::InvalidOperation,
        format!("`date` needs a date or timestamp, got `{s}`"),
    ))
}

/// A DRE date or timestamp (`run.date`, a date column) formatted with its own `format()`.
fn format_object(v: &Value, strftime: &str) -> Result<String, Error> {
    Environment::new().render_str(
        "{{ v.format(f) }}",
        minijinja::context! { v => v.clone(), f => strftime },
    )
}

/// An Excel date format code as a strftime format.
pub fn excel_date_format(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let twelve = lower.contains("am/pm");
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // `m`/`mm` after an hour or before seconds means minutes.
    let mut after_hour = false;
    while i < chars.len() {
        let c = chars[i];
        let run = |ch: char| {
            chars[i..]
                .iter()
                .take_while(|x| x.eq_ignore_ascii_case(&ch))
                .count()
        };
        match c.to_ascii_lowercase() {
            '"' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&x| x == '"')
                    .map_or(chars.len(), |p| i + 1 + p);
                for &x in &chars[i + 1..end] {
                    push_literal(&mut out, x);
                }
                i = end + 1;
                continue;
            }
            '\\' if i + 1 < chars.len() => {
                push_literal(&mut out, chars[i + 1]);
                i += 2;
                continue;
            }
            'y' => {
                let n = run(c);
                out.push_str(if n <= 2 { "%y" } else { "%Y" });
                i += n;
            }
            'd' => {
                let n = run(c);
                out.push_str(match n {
                    1 => "%-d",
                    2 => "%d",
                    3 => "%a",
                    _ => "%A",
                });
                i += n;
            }
            'h' => {
                let n = run(c);
                out.push_str(match (twelve, n) {
                    (true, 1) => "%-I",
                    (true, _) => "%I",
                    (false, 1) => "%-H",
                    (false, _) => "%H",
                });
                after_hour = true;
                i += n;
            }
            's' => {
                let n = run(c);
                out.push_str(if n == 1 { "%-S" } else { "%S" });
                i += n;
            }
            'm' => {
                let n = run(c);
                let rest: String = chars[i + n..].iter().collect::<String>().to_ascii_lowercase();
                let minutes = n <= 2 && (after_hour || rest.trim_start_matches([':', ' ']).starts_with('s'));
                out.push_str(match (minutes, n) {
                    (true, 1) => "%-M",
                    (true, _) => "%M",
                    (false, 1) => "%-m",
                    (false, 2) => "%m",
                    (false, 3) => "%b",
                    (false, _) => "%B",
                });
                after_hour = false;
                i += n;
            }
            'a' if lower[char_offset(&chars, i)..].starts_with("am/pm") => {
                out.push_str("%p");
                i += 5;
            }
            _ => {
                push_literal(&mut out, c);
                i += 1;
            }
        }
    }
    out
}

fn char_offset(chars: &[char], i: usize) -> usize {
    chars[..i].iter().map(|c| c.len_utf8()).sum()
}

fn push_literal(out: &mut String, c: char) {
    if c == '%' {
        out.push_str("%%");
    } else {
        out.push(c);
    }
}

/// A number in an Excel number format (`#,##0.00`, `0%`, `$#,##0`), for `locale`. `None`: the
/// code isn't a number format DRE understands, so the value is shown as it is.
pub fn excel_number(x: f64, code: &str, locale: Locale) -> Option<String> {
    enum Tok {
        Lit(String),
        Pat(char),
    }
    let section = code.split(';').next().unwrap_or(code);
    let chars: Vec<char> = section.chars().collect();
    let mut toks = Vec::new();
    let mut pct = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = |q: char| {
            chars[i + 1..]
                .iter()
                .position(|&x| x == q)
                .map_or(chars.len(), |p| i + 1 + p)
        };
        match c {
            '"' => {
                let end = close('"');
                toks.push(Tok::Lit(chars[i + 1..end].iter().collect()));
                i = end + 1;
                continue;
            }
            '\\' if i + 1 < chars.len() => {
                toks.push(Tok::Lit(chars[i + 1].to_string()));
                i += 2;
                continue;
            }
            '[' => {
                let end = close(']');
                let inner: String = chars[i + 1..end].iter().collect();
                if let Some(sym) = inner.strip_prefix('$') {
                    toks.push(Tok::Lit(sym.split('-').next().unwrap_or("").to_string()));
                }
                i = end + 1;
                continue;
            }
            '_' | '*' => {
                i += 2;
                continue;
            }
            '0' | '#' | '?' | ',' | '.' => toks.push(Tok::Pat(c)),
            '%' => {
                pct = true;
                toks.push(Tok::Lit("%".into()));
            }
            c if c.is_ascii_alphabetic() => return None,
            c => toks.push(Tok::Lit(c.to_string())),
        }
        i += 1;
    }
    let is_digit = |t: &Tok| matches!(t, Tok::Pat('0' | '#' | '?'));
    let first = toks.iter().position(is_digit)?;
    let last = toks.iter().rposition(is_digit)?;
    let lit = |ts: &[Tok]| -> String {
        ts.iter()
            .map(|t| match t {
                Tok::Lit(s) => s.clone(),
                Tok::Pat(c) => c.to_string(),
            })
            .collect()
    };
    let prefix = lit(&toks[..first]);
    let suffix = lit(&toks[last + 1..]);
    let pattern: String = toks[first..=last]
        .iter()
        .filter_map(|t| match t {
            Tok::Pat(c) => Some(*c),
            Tok::Lit(_) => None,
        })
        .collect();
    let (int, frac) = pattern.split_once('.').unwrap_or((&pattern, ""));
    let decimals = frac.chars().filter(|c| matches!(c, '0' | '#' | '?')).count();
    let v = if pct { x * 100.0 } else { x };
    let n = locale.format_number(v.abs(), decimals, int.contains(','));
    let sign = if v < 0.0 && n.chars().any(|c| c.is_ascii_digit() && c != '0') {
        "-"
    } else {
        ""
    };
    Some(format!("{sign}{prefix}{n}{suffix}"))
}

/// `table(results.q)`: a styled `<table>` of a query's rows, columns in order (or `columns`),
/// each value shown with its column's YAML `format`, else as a number or text.
fn table(v: &Value, columns: Option<Vec<String>>, locale: Locale) -> Result<Value, Error> {
    let Some(res) = v.downcast_object_ref::<Res>() else {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "`table()` takes a query's result, like `table(results.holdings)`",
        ));
    };
    let all: Vec<String> = res
        .inner
        .get_attr("columns")?
        .try_iter()?
        .map(|c| c.to_string())
        .collect();
    let cols = match columns {
        Some(c) => {
            if let Some(bad) = c.iter().find(|x| !all.contains(x)) {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!(
                        "`table()`: results.{} has no column `{bad}` (columns: {})",
                        res.name,
                        all.join(", ")
                    ),
                ));
            }
            c
        }
        None => all,
    };
    let mut h = String::from("<table class=\"dre-table\"><thead><tr>");
    for c in &cols {
        h.push_str(&format!("<th>{}</th>", escape(c)));
    }
    h.push_str("</tr></thead><tbody>");
    for row in res.inner.get_attr("rows")?.try_iter()? {
        h.push_str("<tr>");
        for c in &cols {
            let cell = row.get_attr(c)?;
            let (text, num) = shown_cell(&cell, res.formats.get(c).map(String::as_str), locale);
            if num {
                h.push_str(&format!("<td class=\"num\">{}</td>", escape(&text)));
            } else {
                h.push_str(&format!("<td>{}</td>", escape(&text)));
            }
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table>");
    Ok(Value::from_safe_string(h))
}

/// A cell as `table()` shows it, and whether it's a number (right-aligned).
fn shown_cell(v: &Value, format: Option<&str>, locale: Locale) -> (String, bool) {
    if v.is_none() || v.is_undefined() {
        return (String::new(), false);
    }
    let n = match v.kind() {
        minijinja::value::ValueKind::Number => f64::try_from(v.clone()).ok(),
        _ => None,
    };
    match (n, format) {
        (Some(x), Some(f)) => match excel_number(x, f, locale) {
            Some(s) => (s, true),
            None => (v.to_string(), true),
        },
        (Some(x), None) if x.fract() == 0.0 => (locale.number(x, 0), true),
        (Some(x), None) => (locale.number(x, 2), true),
        (None, Some(f)) if v.as_object().is_some() => {
            let shown = format_object(v, &excel_date_format(f)).ok();
            (shown.unwrap_or_else(|| v.to_string()), false)
        }
        _ => (v.to_string(), false),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------------------------
// `validate`
// ---------------------------------------------------------------------------------------------

/// `dre validate`'s checks of a template: syntax, includes (literal names; computed ones are
/// warned), assets, filters, and `results.<name>` naming a query of the output. Errors, then
/// warnings.
pub fn check(c: &Check) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(file) = &c.file else {
        return (errors, warnings);
    };
    let root = PathBuf::from(&c.project_root);
    let ctx = Context {
        file: Some(file.clone()),
        project_root: c.project_root.clone(),
        run: dre_core::document::RunInfo {
            report: String::new(),
            set: None,
            target: String::new(),
            schedule: None,
            date: chrono::Utc::now().date_naive(),
            now: chrono::Utc::now(),
            scheduled_at: None,
            timezone: "UTC".into(),
            week_start: "monday".into(),
            week_numbering: "iso".into(),
        },
        vars: Default::default(),
        cli_vars: Default::default(),
        locale: "en".into(),
    };
    let env = match environment(&ctx, &root, Locale::default()) {
        Ok(e) => e,
        Err(e) => return (vec![e], warnings),
    };
    // Every template reached through literal includes, from the main one.
    let mut todo = vec![file.clone()];
    let mut seen = BTreeSet::new();
    let include = Regex::new(r#"\{%-?\s*(?:include|extends|import|from)\s+(?:ignore\s+missing\s+)?(?:\[)?\s*("([^"]*)"|'([^']*)'|[^\s%]+)"#).unwrap();
    let results_ref =
        Regex::new(r#"results\s*(?:\.\s*([A-Za-z_][A-Za-z0-9_]*)|\[\s*["']([^"']+)["']\s*\])"#).unwrap();
    let filter_ref = Regex::new(r"\|\s*([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut filters = BTreeSet::new();
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let shown_name = shown(&root, &name);
        let src = match std::fs::read_to_string(&name) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("template `{shown_name}`: {e}"));
                continue;
            }
        };
        if let Err(e) = env.template_from_named_str(&name, &src) {
            errors.push(message(&e, &root, None));
            continue;
        }
        let dir = Path::new(&name).parent().unwrap_or(&root).to_path_buf();
        for cap in include.captures_iter(&src) {
            match cap.get(2).or_else(|| cap.get(3)) {
                Some(lit) => match dre_core::template_file::resolve(&root, &dir, lit.as_str()) {
                    Ok(p) => todo.push(p.to_string_lossy().into_owned()),
                    Err(e) => errors.push(format!("template `{shown_name}`: an include: {e}")),
                },
                None => warnings.push(format!(
                    "template `{shown_name}`: the include `{}` is computed, so it isn't checked until the run",
                    &cap[1]
                )),
            }
        }
        for cap in results_ref.captures_iter(&src) {
            let q = cap
                .get(1)
                .or_else(|| cap.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            if !c.queries.iter().any(|x| x == q) {
                let hint = nearest(q, &c.queries)
                    .map(|n| format!("; did you mean `results.{n}`?"))
                    .unwrap_or_default();
                errors.push(format!(
                    "template `{shown_name}`: `results.{q}` isn't a query of this output (its queries: {}){hint}",
                    if c.queries.is_empty() { "none".to_string() } else { c.queries.join(", ") }
                ));
            }
        }
        for cap in filter_ref.captures_iter(&src) {
            filters.insert((cap[1].to_string(), shown_name.clone()));
        }
        errors.extend(crate::assets::check_source(&src, &root, &dir, &shown_name));
    }
    for (f, at) in filters {
        if let Err(e) = env.render_str(&format!("{{{{ none | {f} }}}}"), ())
            && e.kind() == ErrorKind::UnknownFilter
        {
            errors.push(format!("template `{at}`: unknown filter `{f}`"));
        }
    }
    errors.dedup();
    (errors, warnings)
}

/// The closest name within two edits.
fn nearest<'a>(name: &str, names: &'a [String]) -> Option<&'a str> {
    names
        .iter()
        .map(|n| (edits(name, n), n))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, n)| n.as_str())
}

fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_dates() {
        for (code, want) in [
            ("yyyy-mm-dd", "%Y-%m-%d"),
            ("dd/mm/yyyy", "%d/%m/%Y"),
            ("d mmm yyyy", "%-d %b %Y"),
            ("mmmm yyyy", "%B %Y"),
            ("yyyy-mm-dd hh:mm", "%Y-%m-%d %H:%M"),
            ("h:mm AM/PM", "%-I:%M %p"),
            ("dddd \"the\" d", "%A the %-d"),
        ] {
            assert_eq!(excel_date_format(code), want, "{code}");
        }
    }

    #[test]
    fn excel_numbers() {
        let en = Locale::default();
        let de = Locale::parse("de-DE").unwrap();
        for (x, code, loc, want) in [
            (1234.567, "#,##0.00", en, "1,234.57"),
            (1234.567, "0.00", en, "1234.57"),
            (1234.567, "#,##0", en, "1,235"),
            (0.0412, "0.0%", en, "4.1%"),
            (-1234.5, "$#,##0.00", en, "-$1,234.50"),
            (1234.5, "#,##0.00 \"AUD\"", en, "1,234.50 AUD"),
            (1234.5, "#,##0.00", de, "1.234,50"),
            (1234.5, "[$€-407] #,##0.00", de, "€ 1.234,50"),
        ] {
            assert_eq!(excel_number(x, code, loc).as_deref(), Some(want), "{x} {code}");
        }
        assert_eq!(excel_number(1.0, "General", en), None);
    }

    #[test]
    fn near_names() {
        let q = vec!["holdings".to_string(), "members".to_string()];
        assert_eq!(nearest("holding", &q), Some("holdings"));
        assert_eq!(nearest("xyz", &q), None);
    }
}
