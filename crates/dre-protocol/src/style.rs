//! xlsx styling: the `style:` maps on an output, a query entry (tab) and a column. Shared by core
//! (which checks a query entry's style and passes it on) and the xlsx plugin (which checks the
//! output's and applies them all).
//!
//! A cell style: `bold`, `italic`, `underline` (true or false), `font` (`{name, size}`),
//! `font_color` and `fill` (`"#RRGGBB"`), `align` (`left`, `center`, `right`) and `border`
//! (`none`, `thin`, `medium`, around the cell). A sheet style (output or tab) takes the cell keys
//! for every data cell, plus `header` and `totals` (cell styles for those rows), `banded_rows`
//! (a fill for every other data row, or `false`) and `borders` (`none`, `thin`, `medium`: around
//! every cell of the table). A tab's style merges over the output's, key by key; a column's over
//! both, for that column's data cells.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A border's weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Border {
    None,
    Thin,
    Medium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A font's name and size (points).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Font {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

/// How one cell looks; every key optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellStyle {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub font: Option<Font>,
    /// `RRGGBB`.
    pub font_color: Option<u32>,
    pub fill: Option<u32>,
    pub align: Option<Align>,
    pub border: Option<Border>,
}

/// `banded_rows`: a fill, or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Off,
    Fill(u32),
}

/// How a sheet looks: its data cells, the header and totals rows, banding and borders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SheetStyle {
    pub cells: CellStyle,
    pub header: CellStyle,
    pub totals: CellStyle,
    /// The fill of every other data row, or `Off` (over an inherited banding).
    pub banded_rows: Option<Band>,
    pub borders: Option<Border>,
}

const CELL_KEYS: &[&str] = &[
    "bold",
    "italic",
    "underline",
    "font",
    "font_color",
    "fill",
    "align",
    "border",
];
const SHEET_KEYS: &[&str] = &["header", "totals", "banded_rows", "borders"];

fn color(key: &str, v: &Value) -> Result<u32, String> {
    let s = v.as_str().unwrap_or_default();
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() == 6
        && let Ok(n) = u32::from_str_radix(hex, 16)
    {
        return Ok(n);
    }
    Err(format!("`{key}` must be a colour like `\"#1F4E78\"`, got {v}"))
}

fn enum_of<T: for<'a> Deserialize<'a>>(key: &str, v: &Value, choices: &str) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|_| format!("`{key}` must be one of {choices}, got {v}"))
}

fn flag(key: &str, v: &Value) -> Result<bool, String> {
    v.as_bool()
        .ok_or_else(|| format!("`{key}` must be true or false, got {v}"))
}

/// One cell key into `s`; `false` when `key` isn't a cell key.
fn cell_key(s: &mut CellStyle, key: &str, v: &Value, errs: &mut Vec<String>) -> bool {
    let r: Result<(), String> = match key {
        "bold" => flag(key, v).map(|b| s.bold = Some(b)),
        "italic" => flag(key, v).map(|b| s.italic = Some(b)),
        "underline" => flag(key, v).map(|b| s.underline = Some(b)),
        "font_color" => color(key, v).map(|c| s.font_color = Some(c)),
        "fill" => color(key, v).map(|c| s.fill = Some(c)),
        "align" => enum_of(key, v, "`left`, `center`, `right`").map(|a| s.align = Some(a)),
        "border" => enum_of(key, v, "`none`, `thin`, `medium`").map(|b| s.border = Some(b)),
        "font" => serde_json::from_value::<Font>(v.clone())
            .map_err(|_| format!("`font` must be a map like `{{name: Calibri, size: 11}}`, got {v}"))
            .and_then(|f| match f.size {
                Some(sz) if !(1.0..=409.0).contains(&sz) => {
                    Err("`font.size` must be from 1 to 409 points".into())
                }
                _ => Ok(f),
            })
            .map(|f| s.font = Some(f)),
        _ => return false,
    };
    if let Err(e) = r {
        errs.push(e);
    }
    true
}

fn unknown(key: &str, allowed: &[&[&str]]) -> String {
    let all: Vec<String> = allowed
        .iter()
        .flat_map(|k| k.iter())
        .map(|k| format!("`{k}`"))
        .collect();
    format!("unknown style key `{key}`; expected one of {}", all.join(", "))
}

/// A cell style (a column's `style`, a sheet style's `header` or `totals`), and every problem.
pub fn parse_cell(v: &Value) -> (CellStyle, Vec<String>) {
    let mut s = CellStyle::default();
    let mut errs = Vec::new();
    let Some(m) = v.as_object() else {
        return (
            s,
            vec![format!(
                "`style` must be a map like `{{bold: true, fill: \"#FFF2CC\"}}`, got {v}"
            )],
        );
    };
    for (k, val) in m {
        if !cell_key(&mut s, k, val, &mut errs) {
            errs.push(unknown(k, &[CELL_KEYS]));
        }
    }
    (s, errs)
}

/// A sheet style (an output's or a tab's `style`), and every problem.
pub fn parse_sheet(v: &Value) -> (SheetStyle, Vec<String>) {
    let mut s = SheetStyle::default();
    let mut errs = Vec::new();
    let Some(m) = v.as_object() else {
        return (
            s,
            vec![format!(
                "`style` must be a map like `{{banded_rows: \"#F2F2F2\"}}`, got {v}"
            )],
        );
    };
    for (k, val) in m {
        match k.as_str() {
            "header" | "totals" => {
                let (c, e) = parse_cell(val);
                errs.extend(e.into_iter().map(|e| format!("`{k}`: {e}")));
                if k == "header" {
                    s.header = c;
                } else {
                    s.totals = c;
                }
            }
            "banded_rows" => match val {
                Value::Bool(false) => s.banded_rows = Some(Band::Off),
                _ => match color(k, val) {
                    Ok(c) => s.banded_rows = Some(Band::Fill(c)),
                    Err(_) => errs.push(format!(
                        "`banded_rows` must be a colour like `\"#F2F2F2\"`, or false, got {val}"
                    )),
                },
            },
            "borders" => match enum_of(k, val, "`none`, `thin`, `medium`") {
                Ok(b) => s.borders = Some(b),
                Err(e) => errs.push(e),
            },
            _ => {
                if !cell_key(&mut s.cells, k, val, &mut errs) {
                    errs.push(unknown(k, &[CELL_KEYS, SHEET_KEYS]));
                }
            }
        }
    }
    (s, errs)
}

impl CellStyle {
    /// `self` with every key `over` sets replacing it.
    pub fn merged(&self, over: &CellStyle) -> CellStyle {
        let font = match (&self.font, &over.font) {
            (Some(a), Some(b)) => Some(Font {
                name: b.name.clone().or_else(|| a.name.clone()),
                size: b.size.or(a.size),
            }),
            (a, b) => b.clone().or_else(|| a.clone()),
        };
        CellStyle {
            bold: over.bold.or(self.bold),
            italic: over.italic.or(self.italic),
            underline: over.underline.or(self.underline),
            font,
            font_color: over.font_color.or(self.font_color),
            fill: over.fill.or(self.fill),
            align: over.align.or(self.align),
            border: over.border.or(self.border),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == CellStyle::default()
    }
}

impl SheetStyle {
    /// `self` (the output's) with the tab's style over it.
    pub fn merged(&self, over: &SheetStyle) -> SheetStyle {
        SheetStyle {
            cells: self.cells.merged(&over.cells),
            header: self.header.merged(&over.header),
            totals: self.totals.merged(&over.totals),
            banded_rows: over.banded_rows.or(self.banded_rows),
            borders: over.borders.or(self.borders),
        }
    }

    /// The band fill, if banding is on.
    pub fn band(&self) -> Option<u32> {
        match self.banded_rows {
            Some(Band::Fill(c)) => Some(c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_checks_and_merges() {
        let (s, e) = parse_sheet(&json!({
            "font": {"name": "Arial", "size": 10},
            "header": {"bold": true, "fill": "#1F4E78", "font_color": "FFFFFF"},
            "banded_rows": "#F2F2F2",
            "borders": "thin",
            "totals": {"fill": "#DDEBF7"}
        }));
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(s.header.fill, Some(0x1F4E78));
        assert_eq!(s.band(), Some(0xF2F2F2));
        let (tab, _) = parse_sheet(&json!({"banded_rows": false, "font": {"size": 12}}));
        let m = s.merged(&tab);
        assert_eq!(m.band(), None);
        assert_eq!(
            m.cells.font,
            Some(Font {
                name: Some("Arial".into()),
                size: Some(12.0)
            })
        );
        let (_, e) =
            parse_sheet(&json!({"fill": "blue", "borders": "thick", "colour": 1, "header": {"bold": "yes"}}));
        assert_eq!(e.len(), 4, "{e:?}");
        let (_, e) = parse_cell(&json!({"banded_rows": "#FFFFFF"}));
        assert!(e[0].contains("unknown style key `banded_rows`"), "{e:?}");
    }
}
