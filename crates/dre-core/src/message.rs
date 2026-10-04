//! The built-in `message` format: a Binding's query results rendered through Jinja into a short
//! headline, a title plus text in portable Markdown ([`dre_protocol::markdown`]).
//!
//! Templates read `results.<query>`: `value` (first column of the first row), `first.<column>`,
//! `rows` (at most `max_rows`), `row_count` (exact), `columns` and `sets[n]`. With neither
//! `text:` nor `file:`, [`default_text`] writes one block per query.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::ipc::reader::FileReader;
use dre_protocol::markdown::escape;
use minijinja::value::{Enumerator, Object, ObjectRepr, Value};

use crate::dates::Calendar;
use crate::numbers::Locale;

/// A message output's own options (besides the shared output keys).
pub const MESSAGE_KEYS: &[&str] = &["text", "file", "title", "max_rows"];
/// How many rows of each query `results.<query>.rows` holds by default.
pub const DEFAULT_MAX_ROWS: u64 = 1_000;
/// How many rows the default template lists before `+ N more`.
const DEFAULT_LIST_ROWS: usize = 10;

/// One query's result as message templates see it.
#[derive(Debug)]
pub struct QueryResult {
    pub columns: Arc<Vec<String>>,
    /// At most the cap's rows.
    pub rows: Vec<Vec<Value>>,
    /// Every row the query returned.
    pub row_count: u64,
}

impl QueryResult {
    /// Read up to `max_rows` rows of a spooled result set.
    pub fn read(spool: &Path, row_count: u64, max_rows: u64, cal: Calendar) -> Result<QueryResult, String> {
        let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
            .map_err(|e| e.to_string())?;
        let columns = Arc::new(
            reader
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect(),
        );
        let mut rows = Vec::new();
        for b in reader {
            if rows.len() as u64 >= max_rows {
                break;
            }
            let b = b.map_err(|e| e.to_string())?;
            let left = (max_rows - rows.len() as u64) as usize;
            let b = if b.num_rows() > left { b.slice(0, left) } else { b };
            rows.extend(crate::values::typed_rows(&b, cal));
        }
        Ok(QueryResult {
            columns,
            rows,
            row_count,
        })
    }

    pub fn capped(&self) -> bool {
        (self.rows.len() as u64) < self.row_count
    }

    /// The template value: an object with `value`, `first`, `rows`, `row_count`, `columns` and
    /// `sets`.
    pub fn value(self: Arc<Self>) -> Value {
        Value::from_object(ResultValue(self))
    }

    fn row(&self, i: usize) -> Option<Value> {
        self.rows
            .get(i)
            .map(|r| crate::render::Row::value(self.columns.clone(), r.clone()))
    }
}

#[derive(Debug)]
struct ResultValue(Arc<QueryResult>);

impl Object for ResultValue {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let r = &self.0;
        Some(match key.as_str()? {
            "value" => r
                .rows
                .first()
                .and_then(|row| row.first().cloned())
                .unwrap_or(Value::from(())),
            "first" => r.row(0).unwrap_or(Value::from(())),
            "rows" => Value::from((0..r.rows.len()).filter_map(|i| r.row(i)).collect::<Vec<_>>()),
            "row_count" => Value::from(r.row_count),
            "columns" => Value::from(r.columns.as_ref().clone()),
            // One .sql file makes one result set (its last statement's), so `sets` holds it once.
            "sets" => Value::from(vec![Value::from_object(ResultValue(r.clone()))]),
            _ => return None,
        })
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Str(&["value", "first", "rows", "row_count", "columns", "sets"])
    }
}

/// A value as the default template shows it: numbers through `number` (two decimals unless
/// whole), everything else as text; escaped for Markdown.
fn shown(v: &Value, locale: Locale) -> String {
    if v.is_none() {
        return String::new();
    }
    let n = match v.as_i64() {
        Some(i) => Some(i as f64),
        None if v.kind() == minijinja::value::ValueKind::Number => f64::try_from(v.clone()).ok(),
        None => None,
    };
    let text = match n {
        Some(x) if x.fract() == 0.0 => locale.number(x, 0),
        Some(x) => locale.number(x, 2),
        None => v.to_string(),
    };
    escape(&text)
}

/// The default message: one block per query. A single value is `label: value`; one row is
/// `label: value` lines; several rows are a list of at most ten, then `+ N more`.
pub fn default_text(results: &[(String, Arc<QueryResult>)], locale: Locale) -> String {
    let mut blocks = Vec::new();
    for (query, r) in results {
        let label = |c: &str| escape(c);
        let block = match (r.row_count, r.columns.len()) {
            (0, _) => format!("{}: no rows", label(query)),
            (1, 1) => format!("{}: {}", label(&r.columns[0]), shown(&r.rows[0][0], locale)),
            (1, _) => r
                .columns
                .iter()
                .zip(&r.rows[0])
                .map(|(c, v)| format!("{}: {}", label(c), shown(v, locale)))
                .collect::<Vec<_>>()
                .join("\n"),
            (n, _) => {
                let mut lines = vec![format!("**{}**", label(query))];
                for row in r.rows.iter().take(DEFAULT_LIST_ROWS) {
                    let cells: Vec<String> = row.iter().map(|v| shown(v, locale)).collect();
                    lines.push(format!("- {}", cells.join(", ")));
                }
                let listed = r.rows.len().min(DEFAULT_LIST_ROWS) as u64;
                if n > listed {
                    lines.push(format!("+ {} more", crate::run::thousands(n - listed)));
                }
                lines.join("\n")
            }
        };
        blocks.push(block);
    }
    blocks.join("\n\n")
}

/// What a message output writes to its `.md` file.
pub fn file_body(title: &str, text: &str) -> String {
    format!("# {title}\n\n{text}\n")
}

/// One line for the log: the title and the start of the text, as plain text.
pub fn excerpt(title: &str, text: &str, max: usize) -> String {
    let plain = dre_protocol::markdown::to_plain(text).replace('\n', " · ");
    let line = format!("{title} — {plain}");
    if line.chars().count() <= max {
        line
    } else {
        format!(
            "{}…",
            line.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}
