//! Selector resolution, shared by `dre validate` (schedules.yml) and `dre run`.
//!
//! A bare token is read as an exact report name, a tag and a folder leaf name anywhere under
//! `reports/` (a dotted token: a folder path from `reports/`). Readings that pick different
//! reports are an error, so adding a report, tag or folder never silently changes what an
//! existing selector runs; `report:x`, `tag:x` and `folder:a.b` pick one reading. Every term must
//! match some report.
//! `source:sales` picks every report a query of which reads a table of source `sales`, and
//! `source:sales.orders` those reading that table (as the parse pass found them).

use crate::codes::Code;
use std::fmt;

use crate::project::{Project, Report, dotted};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    /// A bare token's readings (`report:x`, `tag:x`, `folder:a.x`) pick different reports.
    Ambiguous {
        token: String,
        candidates: Vec<String>,
    },
    NoMatch {
        token: String,
    },
    /// `source:` names a source or table that isn't declared.
    UnknownSource {
        token: String,
        message: String,
    },
}

impl SelectorError {
    pub fn code(&self) -> Code {
        match self {
            SelectorError::Ambiguous { .. } => Code::AmbiguousSelector,
            SelectorError::NoMatch { .. } => Code::SelectorMatchesNothing,
            SelectorError::UnknownSource { .. } => Code::UnknownSource,
        }
    }
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectorError::Ambiguous { token, candidates } => {
                write!(
                    f,
                    "\"{token}\" picks different reports as {}; write the one you mean",
                    candidates.join(", ")
                )
            }
            SelectorError::NoMatch { token } => {
                write!(f, "selector `{token}` matches no report name, tag or folder")
            }
            SelectorError::UnknownSource { token, message } => write!(f, "selector `{token}`: {message}"),
        }
    }
}

impl std::error::Error for SelectorError {}

/// Resolve a selection: one or more selectors separated by spaces, commas or semicolons
/// (`daily tag:regulatory`, `daily,monthly`, `daily;monthly`), matching any of them. Reports come
/// back in project order, each once.
pub fn resolve<'p>(project: &'p Project, selection: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let mut picked: Vec<&'p Report> = Vec::new();
    for term in selection.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if term.is_empty() {
            continue;
        }
        for r in resolve_one(project, term)? {
            if !picked.iter().any(|x| x.name == r.name) {
                picked.push(r);
            }
        }
    }
    picked.sort_by_key(|r| project.reports().iter().position(|x| x.name == r.name));
    Ok(picked)
}

fn resolve_one<'p>(project: &'p Project, token: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let token = token.trim();
    let under = |folder: &[String]| -> Vec<&'p Report> {
        project
            .reports()
            .iter()
            .filter(|r| r.folder.starts_with(folder))
            .collect()
    };
    let tagged = |tag: &str| -> Vec<&'p Report> {
        project
            .reports()
            .iter()
            .filter(|r| r.tags.iter().any(|t| t == tag))
            .collect()
    };
    let path = |p: &str| -> Vec<String> { p.split('.').map(str::to_string).collect() };
    let no_match = || SelectorError::NoMatch {
        token: token.to_string(),
    };
    let found = |reports: Vec<&'p Report>| {
        if reports.is_empty() {
            Err(no_match())
        } else {
            Ok(reports)
        }
    };
    if let Some(sel) = token.strip_prefix("source:") {
        return by_source(project, token, sel);
    }
    if let Some(name) = token.strip_prefix("report:") {
        return project.report(name).map(|r| vec![r]).ok_or_else(no_match);
    }
    if let Some(tag) = token.strip_prefix("tag:") {
        return found(tagged(tag));
    }
    if let Some(folder) = token.strip_prefix("folder:") {
        return found(under(&path(folder)));
    }
    // Every reading of a bare token, as the explicit selector that picks it.
    let mut readings: Vec<(String, Vec<&'p Report>)> = Vec::new();
    if let Some(r) = project.report(token) {
        readings.push((format!("report:{token}"), vec![r]));
    }
    if token.contains('.') {
        readings.push((format!("folder:{token}"), under(&path(token))));
    } else {
        readings.push((format!("tag:{token}"), tagged(token)));
        for f in project
            .folders()
            .iter()
            .filter(|f| f.last().is_some_and(|l| l == token))
        {
            readings.push((format!("folder:{}", dotted(f)), under(f)));
        }
    }
    readings.retain(|(_, reports)| !reports.is_empty());
    let names = |reports: &[&Report]| reports.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    match readings.as_slice() {
        [] => Err(no_match()),
        [(_, first), rest @ ..] if rest.iter().all(|(_, r)| names(r) == names(first)) => Ok(first.clone()),
        _ => Err(SelectorError::Ambiguous {
            token: token.to_string(),
            candidates: readings.into_iter().map(|(s, _)| format!("`{s}`")).collect(),
        }),
    }
}

/// `source:<source>` or `source:<source>.<table>`: the reports some query of which reads it.
fn by_source<'p>(project: &'p Project, token: &str, sel: &str) -> Result<Vec<&'p Report>, SelectorError> {
    let unknown = |message: String| SelectorError::UnknownSource {
        token: token.to_string(),
        message,
    };
    let (source, table) = match sel.split_once('.') {
        Some((s, t)) => (s, Some(t)),
        None => (sel, None),
    };
    let Some(def) = project.source(source) else {
        let known: Vec<&str> = project.sources().keys().map(String::as_str).collect();
        return Err(unknown(if known.is_empty() {
            format!("no source `{source}`: the project declares no `sources:`")
        } else {
            format!("no source `{source}` (declared: {})", known.join(", "))
        }));
    };
    if let Some(t) = table
        && def.table(t).is_none()
    {
        return Err(unknown(format!(
            "source `{source}` has no table `{t}` (it has: {})",
            def.tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let reads = |key: &str| match table {
        Some(t) => key == format!("{source}.{t}"),
        None => key.split_once('.').is_some_and(|(s, _)| s == source),
    };
    Ok(project
        .reports()
        .iter()
        .filter(|r| {
            r.bindings
                .iter()
                .filter_map(|b| b.parsed.as_ref())
                .any(|p| p.source_keys().into_iter().any(reads))
        })
        .collect())
}
