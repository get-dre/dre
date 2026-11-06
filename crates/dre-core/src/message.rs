//! The built-in `message` format: a Binding's query results rendered through Jinja into a short
//! headline, a title plus text in portable Markdown ([`dre_protocol::markdown`]).
//!
//! Templates read `results.<query>`: `value` (first column of the first row), `first.<column>`,
//! `rows` (at most `max_rows`), `row_count` (exact), `columns` and `sets[n]`. With neither
//! `text:` nor `file:`, [`default_text`] writes one block per query.

use std::sync::Arc;

use dre_protocol::markdown::escape;
use minijinja::value::Value;

use crate::numbers::Locale;
pub use dre_template::result::QueryResult;

/// A message output's own options (besides the shared output keys).
pub const MESSAGE_KEYS: &[&str] = &["text", "file", "title", "max_rows"];
/// How many rows of each query `results.<query>.rows` holds by default.
pub const DEFAULT_MAX_ROWS: u64 = 1_000;
/// How many rows the default template lists before `+ N more`.
const DEFAULT_LIST_ROWS: usize = 10;

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
        let block = match (
            r.row_count,
            r.columns.as_slice(),
            r.rows.first().map(Vec::as_slice),
        ) {
            (0, _, _) => format!("{}: no rows", label(query)),
            (1, [column], Some([value])) => format!("{}: {}", label(column), shown(value, locale)),
            (1, columns, Some(row)) => columns
                .iter()
                .zip(row)
                .map(|(c, v)| format!("{}: {}", label(c), shown(v, locale)))
                .collect::<Vec<_>>()
                .join("\n"),
            (n, _, _) => {
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
pub fn excerpt(title: &str, text: &str, max: usize, secrets: &crate::secrets::Secrets) -> String {
    // Decode Markdown first so escaped values become literal again, then protect them before
    // flattening whitespace or truncating. Mask markers must not be parsed as Markdown.
    let plain = dre_protocol::markdown::to_plain(text);
    let plain = secrets.mask(&plain).replace('\n', " · ");
    let title = secrets.mask_message_title(title);
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
