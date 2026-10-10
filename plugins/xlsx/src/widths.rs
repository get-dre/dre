//! Column widths sized from the content (`autofit`, on by default; a column's `width: auto` or a
//! number of characters). A column's width is its longest value as Excel shows it (number
//! formats and dates applied), across the header, the first `MEASURE_ROWS` data rows and the
//! totals row, plus `PADDING`, at least Excel's default and at most `MAX_WIDTH`. A fixed `width`
//! is used as it is. Precedence: the column's `width`, then the tab's `autofit`, then the
//! output's.

use dre_protocol::msg::ColumnWidth;

use crate::cells::Excel;

/// Excel's default column width, in characters.
pub const MIN_WIDTH: f64 = 8.43;
/// The widest an autofitted column gets; text past it is cut off by the next cell, as in Excel.
pub const MAX_WIDTH: f64 = 60.0;
/// Room around the longest value.
pub const PADDING: f64 = 2.0;
/// How many data rows of each sheet are measured.
pub const MEASURE_ROWS: u64 = 10_000;

/// One sheet's columns: which are measured, which have a fixed width, and the longest value
/// seen in each.
pub struct Widths {
    measure: Vec<bool>,
    fixed: Vec<Option<f64>>,
    longest: Vec<usize>,
    rows: u64,
}

impl Widths {
    /// `autofit`: the tab's, else the output's; `columns`: each column's `width`, if set.
    pub fn new(autofit: bool, columns: &[Option<ColumnWidth>]) -> Widths {
        Widths {
            measure: columns
                .iter()
                .map(|w| match w {
                    Some(ColumnWidth::Auto(_)) => true,
                    Some(ColumnWidth::Chars(_)) => false,
                    None => autofit,
                })
                .collect(),
            fixed: columns
                .iter()
                .map(|w| match w {
                    Some(ColumnWidth::Chars(c)) => Some(*c),
                    _ => None,
                })
                .collect(),
            longest: vec![0; columns.len()],
            rows: 0,
        }
    }

    /// Whether anything on the sheet needs a width set.
    pub fn any(&self) -> bool {
        self.measure.iter().any(|m| *m) || self.fixed.iter().any(Option::is_some)
    }

    /// A header or totals cell's text.
    pub fn text(&mut self, col: usize, s: &str) {
        if self.measure.get(col).copied().unwrap_or(false) {
            self.longest[col] = self.longest[col].max(text_len(s));
        }
    }

    /// A data cell, shown with number format `code`; counted for the first `MEASURE_ROWS` rows.
    pub fn value(&mut self, col: usize, v: &Option<Excel>, code: Option<&str>) {
        if self.rows < MEASURE_ROWS && self.measure.get(col).copied().unwrap_or(false) {
            self.longest[col] = self.longest[col].max(shown_len(v, code));
        }
    }

    /// A totals cell's value, measured whatever the row count.
    pub fn total(&mut self, col: usize, v: &Option<Excel>, code: Option<&str>) {
        if self.measure.get(col).copied().unwrap_or(false) {
            self.longest[col] = self.longest[col].max(shown_len(v, code));
        }
    }

    pub fn end_row(&mut self) {
        self.rows += 1;
    }

    /// Each column's width, in characters: `None` leaves Excel's (or the template's) own.
    pub fn widths(&self) -> Vec<Option<f64>> {
        (0..self.measure.len())
            .map(|c| match (self.fixed[c], self.measure[c]) {
                (Some(w), _) => Some(w),
                (None, true) => Some((self.longest[c] as f64 + PADDING).clamp(MIN_WIDTH, MAX_WIDTH)),
                (None, false) => None,
            })
            .collect()
    }
}

/// Characters a text takes, counting East Asian wide characters as two.
pub fn text_len(s: &str) -> usize {
    s.lines()
        .map(|l| l.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum::<usize>())
        .max()
        .unwrap_or(0)
}

fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x20000..=0x3FFFD)
}

/// How many characters Excel shows for `v` under number format `code`.
pub fn shown_len(v: &Option<Excel>, code: Option<&str>) -> usize {
    match v {
        None => 0,
        Some(Excel::Text(s)) => text_len(s),
        Some(Excel::Bool(_)) => 5,
        Some(Excel::Number(n)) => match code {
            Some(c) if !c.eq_ignore_ascii_case("general") && c != "@" => number_len(*n, c),
            _ => general_len(*n),
        },
        Some(Excel::Date(_)) | Some(Excel::DateTime(_)) | Some(Excel::Time(_)) => code.map_or(10, date_len),
    }
}

/// `General`: the shortest form, up to about 11 characters (Excel switches to scientific).
fn general_len(n: f64) -> usize {
    let s = if n.fract() == 0.0 && n.abs() < 1e11 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.9}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    s.chars().count().min(11)
}

/// The section of a format code that applies to `n` (`pos;neg;zero;text`), and whether the
/// negative section shows the sign itself (parentheses, say).
fn section(code: &str, n: f64) -> (&str, bool) {
    let mut sections = Vec::new();
    let (mut start, mut quoted, mut bracket) = (0, false, false);
    for (i, ch) in code.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            ';' if !quoted && !bracket => {
                sections.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    sections.push(&code[start..]);
    match (n < 0.0, n == 0.0, sections.len()) {
        (true, _, l) if l >= 2 => (sections[1], true),
        (false, true, l) if l >= 3 => (sections[2], false),
        _ => (sections[0], false),
    }
}

/// A number under a number format: digits (with grouping), decimals, sign, percent, and the
/// literal text (`"€ "`, `[$€-x-euro2]`, units).
fn number_len(n: f64, code: &str) -> usize {
    let (sec, own_sign) = section(code, n);
    let (mut decimals, mut grouping, mut percent, mut literal) = (0usize, false, false, 0usize);
    let (mut after_point, mut quoted, mut chars) = (false, false, sec.chars().peekable());
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            _ if quoted => literal += 1,
            '\\' => {
                chars.next();
                literal += 1;
            }
            '[' => {
                let inner: String = chars.by_ref().take_while(|c| *c != ']').collect();
                // `[$€-x-euro2]`: the currency symbol; `[Red]`, `[>100]`: nothing shown.
                if let Some(sym) = inner.strip_prefix('$') {
                    literal += text_len(sym.split('-').next().unwrap_or(""));
                }
            }
            '_' => {
                chars.next();
                literal += 1;
            }
            '*' => {
                chars.next();
            }
            '.' => after_point = true,
            '0' | '#' | '?' if after_point => decimals += 1,
            '0' | '#' | '?' => {}
            ',' if !after_point => grouping = true,
            ',' => {}
            '%' => percent = true,
            'E' | 'e' => literal += 4,
            _ => literal += 1,
        }
    }
    let v = if percent { n.abs() * 100.0 } else { n.abs() };
    let int_digits = format!("{:.0}", v.trunc()).len().max(1);
    let groups = if grouping { (int_digits - 1) / 3 } else { 0 };
    let sign = usize::from(n < 0.0 && !own_sign);
    int_digits + groups + decimals + usize::from(decimals > 0) + usize::from(percent) + sign + literal
}

/// A date, time or timestamp under its format code: each token's widest output (`mmmm` is a
/// month name, `yyyy` four digits, `AM/PM` two letters).
fn date_len(code: &str) -> usize {
    let (sec, _) = section(code, 1.0);
    let lower = sec.to_ascii_lowercase();
    let b = lower.as_bytes();
    let (mut i, mut len, mut quoted) = (0, 0, false);
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            quoted = !quoted;
            i += 1;
            continue;
        }
        if quoted {
            len += 1;
            i += 1;
            continue;
        }
        if c == b'[' {
            // `[h]`: elapsed hours, shown as digits.
            let end = lower[i..].find(']').map_or(b.len(), |e| i + e + 1);
            len += if lower[i..end].starts_with("[h")
                || lower[i..end].starts_with("[m")
                || lower[i..end].starts_with("[s")
            {
                3
            } else {
                0
            };
            i = end;
            continue;
        }
        if lower[i..].starts_with("am/pm") {
            len += 2;
            i += 5;
            continue;
        }
        if lower[i..].starts_with("a/p") {
            len += 1;
            i += 3;
            continue;
        }
        let run = b[i..].iter().take_while(|x| **x == c).count();
        len += match (c, run) {
            (b'y', r) if r <= 2 => 2,
            (b'y', _) => 4,
            (b'm', 3) => 3,
            (b'm', r) if r >= 4 => 9,
            (b'd', 3) => 3,
            (b'd', r) if r >= 4 => 9,
            (b'm' | b'd' | b'h' | b's', _) => 2,
            (b'\\', _) => {
                i += 2;
                len += 1;
                continue;
            }
            (_, r) => r,
        };
        i += run;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_lengths() {
        assert_eq!(
            shown_len(&Some(Excel::Number(152630.38)), Some("#,##0.00")),
            "152,630.38".len()
        );
        assert_eq!(
            shown_len(&Some(Excel::Number(-5.0)), Some("#,##0.00;[Red](#,##0.00)")),
            "(5.00)".len()
        );
        assert_eq!(
            shown_len(&Some(Excel::Number(0.125)), Some("0.0%")),
            "12.5%".len()
        );
        assert_eq!(
            shown_len(&Some(Excel::Number(1234.5)), Some("[$€-x-euro2] #,##0.00")),
            "€ 1,234.50".chars().count()
        );
        assert_eq!(shown_len(&Some(Excel::Number(1234.5)), None), "1234.5".len());
        assert_eq!(
            shown_len(&Some(Excel::Date(46047.0)), Some("dd/mm/yyyy")),
            "25/01/2026".len()
        );
        assert_eq!(
            shown_len(&Some(Excel::Date(46047.0)), Some("mmmm yyyy")),
            "September 2026".len()
        );
        assert_eq!(
            shown_len(&Some(Excel::Time(0.5)), Some("h:mm AM/PM")),
            "12:00 PM".len()
        );
        assert_eq!(text_len("Europe, Middle East & Africa"), 28);
        assert_eq!(text_len("日本"), 4);
    }

    #[test]
    fn widths_follow_precedence_and_limits() {
        use dre_protocol::msg::{AutoWidth, ColumnWidth};
        let mut w = Widths::new(true, &[None, Some(ColumnWidth::Chars(30.0)), None]);
        w.text(0, "n");
        w.value(2, &Some(Excel::Text("x".repeat(200))), None);
        assert_eq!(w.widths(), [Some(MIN_WIDTH), Some(30.0), Some(MAX_WIDTH)]);
        let mut w = Widths::new(false, &[None, Some(ColumnWidth::Auto(AutoWidth::Auto))]);
        w.value(1, &Some(Excel::Text("Europe, Middle East & Africa".into())), None);
        assert_eq!(w.widths(), [None, Some(30.0)]);
    }
}
