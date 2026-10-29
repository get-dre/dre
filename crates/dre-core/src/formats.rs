//! The format catalog: what each output format is, and the files an output makes.
//!
//! Every rule that depends on a format's name lives here: its file extension, whether it holds
//! one table per file (so an output of several queries is split), whether it's an Excel workbook
//! (sheet names, `columns`, template bindings), a document (html, pdf) or the built-in message.
//! `validate` and `run` both ask it, so the names `validate` predicts are the ones `run` writes.
//! A new format means a line here and its plugin. Format plugins don't describe these
//! properties yet; when the protocol's `describe` carries them, this is where they plug in.

use std::collections::BTreeMap;

/// The format of an output without `format:`.
pub const DEFAULT: &str = "csv";

/// The built-in format that renders query results into a short headline.
pub const MESSAGE: &str = "message";

/// Formats whose file holds one table: an output of several queries writes a file per query.
const ONE_TABLE: &[&str] = &["csv", "delimited", "fixed_width", "parquet"];

/// Formats that render a document from a template and the run's context.
const DOCUMENTS: &[&str] = &["html", "pdf"];

/// The Excel workbook format.
const WORKBOOK: &str = "xlsx";

/// An output format, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format<'a>(&'a str);

impl<'a> Format<'a> {
    pub fn new(name: &'a str) -> Format<'a> {
        Format(name)
    }

    pub fn name(self) -> &'a str {
        self.0
    }

    /// The built-in message, which no plugin writes.
    pub fn is_message(self) -> bool {
        self.0 == MESSAGE
    }

    /// The extension of its files, without the dot.
    pub fn extension(self) -> &'a str {
        match self.0 {
            "delimited" | "fixed_width" => "txt",
            MESSAGE => "md",
            f => f,
        }
    }

    /// Whether a file of it holds one table.
    pub fn one_table(self) -> bool {
        ONE_TABLE.contains(&self.0)
    }

    /// An Excel workbook: one sheet per query, Excel's sheet-name rules, `columns`, template
    /// `bindings`, and no `extension` of its own.
    pub fn is_workbook(self) -> bool {
        self.0 == WORKBOOK
    }

    /// A document (html, pdf): its template gets the run's context.
    pub fn is_document(self) -> bool {
        DOCUMENTS.contains(&self.0)
    }

    /// Whether it takes `output.template`.
    pub fn takes_template(self) -> bool {
        self.is_workbook() || self.is_document()
    }

    /// Whether its template decides what to show, so an empty query can't drop a section
    /// (`on_empty: skip` has no effect).
    pub fn template_decides_sections(self) -> bool {
        self.is_message() || self.is_document()
    }
}

/// What decides the names of one output's files.
#[derive(Debug, Clone, Copy)]
pub struct Files<'a> {
    pub format: Format<'a>,
    /// `output.extension`, over the format's.
    pub extension: Option<&'a str>,
    /// Whether the output has a `template`.
    pub template: bool,
}

impl<'a> Files<'a> {
    /// The file plan of `output`.
    pub fn of(output: &'a crate::project::Output) -> Files<'a> {
        Files {
            format: Format::new(&output.format),
            extension: output.extension.as_deref(),
            template: output.template.is_some(),
        }
    }

    /// The extension of its files, without the dot (may be empty).
    pub fn extension(&self) -> &'a str {
        self.extension.unwrap_or_else(|| self.format.extension())
    }

    /// `<stem>.<extension>`, or `stem` without an extension.
    pub fn name(&self, stem: &str) -> String {
        match self.extension() {
            "" => stem.to_string(),
            ext => format!("{stem}.{ext}"),
        }
    }

    /// The output's file name: `output_name` (`--output-name`), else the last part of the
    /// first destination path that isn't a zip's, else `<stem>.<extension>`.
    pub fn base_name<'p>(
        &self,
        stem: &str,
        output_name: Option<&str>,
        paths: impl IntoIterator<Item = &'p str>,
    ) -> String {
        let from_path = paths
            .into_iter()
            .next()
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .filter(|n| !n.is_empty());
        match (output_name, from_path) {
            (Some(n), _) => n.to_string(),
            (None, Some(n)) => n.to_string(),
            (None, None) => self.name(stem),
        }
    }

    /// Whether `tabs` result sets go to a file each: a one-table format without a template.
    pub fn splits(&self, tabs: usize) -> bool {
        tabs > 1 && self.format.one_table() && !self.template
    }

    /// One file per tab: `<stem>_<tab><.ext>` from `base`.
    pub fn split_names(base: &str, tabs: &[&str]) -> Result<Vec<String>, String> {
        let (stem, ext) = split_ext(base);
        tabs.iter()
            .map(|t| {
                if t.contains(['/', '\\']) {
                    return Err(format!("`{t}` can't be used in a file name"));
                }
                Ok(format!("{stem}_{t}{ext}"))
            })
            .collect()
    }

    /// The files the output writes from `tabs` (the names of its result sets, in order): each
    /// file's name and the tabs (indexes) in it.
    pub fn plan(&self, base: &str, tabs: &[&str]) -> Result<Vec<(String, Vec<usize>)>, String> {
        if !self.splits(tabs.len()) {
            return Ok(vec![(base.to_string(), (0..tabs.len()).collect())]);
        }
        Ok(Files::split_names(base, tabs)?
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n, vec![i]))
            .collect())
    }
}

/// `name` split before its extension's dot: `("a", ".csv")`; a leading dot isn't one.
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Excel's rules for a sheet name.
pub fn check_sheet_name(n: &str) -> Result<(), String> {
    if n.is_empty() || n.chars().count() > 31 {
        return Err(format!("sheet name `{n}` must be 1 to 31 characters long"));
    }
    if let Some(c) = n.chars().find(|c| "[]:*?/\\".contains(*c)) {
        return Err(format!(
            "sheet name `{n}` contains `{c}`, which Excel doesn't allow in sheet names"
        ));
    }
    if n.starts_with('\'') || n.ends_with('\'') {
        return Err(format!("sheet name `{n}` can't start or end with an apostrophe"));
    }
    if n.eq_ignore_ascii_case("history") {
        return Err("`History` is reserved by Excel and can't be a sheet name".into());
    }
    Ok(())
}

/// A workbook's sheets, as `(sheet name, query)`: each name valid, and no two alike (Excel
/// ignores case).
pub fn check_sheets<'s>(sheets: impl IntoIterator<Item = (&'s str, &'s str)>) -> Result<(), String> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for (name, query) in sheets {
        check_sheet_name(name)?;
        if let Some(prev) = seen.insert(name.to_lowercase(), query) {
            return Err(format!(
                "sheet name `{name}` is used twice (by `{prev}` and `{query}`); Excel sheet names must be unique"
            ));
        }
    }
    Ok(())
}

/// The first name that's in `names` twice (a zip is flat: two files of one name clash).
pub fn first_clash<'n>(names: impl IntoIterator<Item = &'n str>) -> Option<&'n str> {
    let mut seen = std::collections::BTreeSet::new();
    names.into_iter().find(|n| !seen.insert(*n))
}

#[cfg(test)]
mod tests;
