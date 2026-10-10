//! The `style:` maps (dre_protocol::style) as Excel formats: on sheets DRE creates through
//! rust_xlsxwriter, and, for a column's `style`, on template cells through umya.

use dre_protocol::style::{Align, Border, CellStyle, SheetStyle};
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, FormatPattern, FormatUnderline};

fn border(b: Border) -> FormatBorder {
    match b {
        Border::None => FormatBorder::None,
        Border::Thin => FormatBorder::Thin,
        Border::Medium => FormatBorder::Medium,
    }
}

/// `f` with `s` applied, and `borders` (the table's) around the cell unless `s` sets its own.
pub fn apply(mut f: Format, s: &CellStyle, borders: Option<Border>) -> Format {
    if s.bold == Some(true) {
        f = f.set_bold();
    }
    if s.italic == Some(true) {
        f = f.set_italic();
    }
    if s.underline == Some(true) {
        f = f.set_underline(FormatUnderline::Single);
    }
    if let Some(font) = &s.font {
        if let Some(name) = &font.name {
            f = f.set_font_name(name);
        }
        if let Some(size) = font.size {
            f = f.set_font_size(size);
        }
    }
    if let Some(c) = s.font_color {
        f = f.set_font_color(Color::RGB(c));
    }
    if let Some(c) = s.fill {
        f = f
            .set_pattern(FormatPattern::Solid)
            .set_background_color(Color::RGB(c));
    }
    if let Some(a) = s.align {
        f = f.set_align(match a {
            Align::Left => FormatAlign::Left,
            Align::Center => FormatAlign::Center,
            Align::Right => FormatAlign::Right,
        });
    }
    if let Some(b) = s.border.or(borders) {
        f = f.set_border(border(b));
    }
    f
}

/// A sheet's look, resolved: the header's and totals' styles (over DRE's defaults: bold, and a
/// thin top border on totals), each column's data style, the band fill and the borders.
pub struct Look {
    pub header: CellStyle,
    pub totals: CellStyle,
    pub columns: Vec<CellStyle>,
    pub band: Option<u32>,
    pub borders: Option<Border>,
}

impl Look {
    /// `sheet` (the output's style with the tab's over it) and each column's own style.
    pub fn new(sheet: &SheetStyle, columns: Vec<Option<CellStyle>>) -> Look {
        let bold = CellStyle {
            bold: Some(true),
            ..Default::default()
        };
        Look {
            header: bold.merged(&sheet.header),
            totals: bold.merged(&sheet.totals),
            columns: columns
                .into_iter()
                .map(|c| sheet.cells.merged(&c.unwrap_or_default()))
                .collect(),
            band: sheet.band(),
            borders: sheet.borders,
        }
    }

    /// Whether data cells need more than a number format.
    pub fn styled(&self) -> bool {
        self.band.is_some() || self.borders.is_some() || self.columns.iter().any(|c| !c.is_empty())
    }

    /// A data cell's format: its number format `code`, its column's style, the band on
    /// alternate rows.
    pub fn data(&self, col: usize, code: Option<&str>, banded: bool) -> Format {
        let mut s = self.columns[col].clone();
        if banded && s.fill.is_none() {
            s.fill = self.band;
        }
        let f = code.map_or_else(Format::new, |c| Format::new().set_num_format(c));
        apply(f, &s, self.borders)
    }

    pub fn header(&self) -> Format {
        apply(Format::new(), &self.header, self.borders)
    }

    /// A totals cell: bold with a thin top border (unless the style says otherwise), the
    /// column's number format.
    pub fn totals(&self, code: Option<&str>) -> Format {
        let f = code.map_or_else(Format::new, |c| Format::new().set_num_format(c));
        let f = apply(f, &self.totals, self.borders);
        if self.totals.border.is_none() && self.borders.is_none() {
            f.set_border_top(FormatBorder::Thin)
        } else {
            f
        }
    }
}

/// A column's `style` on a template cell (umya), over what the template gives it.
pub fn apply_template(cell: &mut umya_spreadsheet::Cell, s: &CellStyle) {
    let style = cell.style_mut();
    let font = style.font_mut();
    if let Some(b) = s.bold {
        font.set_bold(b);
    }
    if let Some(i) = s.italic {
        font.set_italic(i);
    }
    if s.underline == Some(true) {
        font.set_underline("single");
    }
    if let Some(f) = &s.font {
        if let Some(name) = &f.name {
            font.set_name(name);
        }
        if let Some(size) = f.size {
            font.set_size(size);
        }
    }
    if let Some(c) = s.font_color {
        font.color_mut().set_argb_str(format!("FF{c:06X}"));
    }
    if let Some(c) = s.fill {
        style.set_background_color_solid(format!("FF{c:06X}"));
    }
    if let Some(a) = s.align {
        use umya_spreadsheet::HorizontalAlignmentValues as H;
        style.alignment_mut().set_horizontal(match a {
            Align::Left => H::Left,
            Align::Center => H::Center,
            Align::Right => H::Right,
        });
    }
    if let Some(b) = s.border {
        let kind = match b {
            Border::None => umya_spreadsheet::Border::BORDER_NONE,
            Border::Thin => umya_spreadsheet::Border::BORDER_THIN,
            Border::Medium => umya_spreadsheet::Border::BORDER_MEDIUM,
        };
        let borders = style.borders_mut();
        borders.left_mut().set_border_style(kind);
        borders.right_mut().set_border_style(kind);
        borders.top_mut().set_border_style(kind);
        borders.bottom_mut().set_border_style(kind);
    }
}
