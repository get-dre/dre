//! DRE format plugins `html` and `pdf`: one document per output, from a Jinja HTML + CSS
//! template filled with the output's query results and the run's context.
//!
//! Core streams every result set and sends the template context in `write`'s `template`
//! (`dre_core::document::Context`). The plugin buffers the result sets, renders the template once
//! at `finish` ([`template`]), resolves its images, stylesheets and fonts inside the project
//! ([`assets`]), and writes the HTML, or lays it out into a PDF ([`pdf`]), in-process: no browser,
//! no network.
//!
//! Options (defaults): `max_rows` (50,000 rows across the output's queries; more is an error,
//! never a truncated document). html: `embed_assets` (true). pdf: `page_size` (`A4`),
//! `orientation` (`portrait`), `margins` (`20mm`), used only where the template's CSS sets no
//! `@page`; `author`, `subject`; `password` (an open password: AES-256, after rendering).

mod assets;
mod fallback;
mod fonts;
pub mod pdf;
mod template;

use std::sync::Arc;

use dre_core::document::{Check, Context};
use dre_protocol::options::{OptionField, OptionType};
use dre_protocol::plugin::{
    About, ErrorKind, Format, Plugin, PluginError, Result, ResultSets, WriteRequest, serve_package,
};
use serde_json::{Map, Value};

pub use template::Data;

/// `max_rows` by default: rows across every query of one document.
pub const DEFAULT_MAX_ROWS: u64 = 50_000;

pub fn serve() -> ! {
    let version = env!("CARGO_PKG_VERSION");
    serve_package(vec![
        Plugin::Format(About::new("html", version), Box::new(Document { pdf: false })),
        Plugin::Format(About::new("pdf", version), Box::new(Document { pdf: true })),
    ])
}

struct Document {
    pdf: bool,
}

/// The options both formats read.
pub struct Options {
    pub max_rows: u64,
    pub embed_assets: bool,
    pub page: pdf::PageOptions,
    /// pdf: the open password, rendered by core.
    pub password: Option<String>,
}

impl Options {
    pub fn parse(o: &Map<String, Value>) -> Options {
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
        Options {
            max_rows: o
                .get("max_rows")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_ROWS),
            embed_assets: o.get("embed_assets").and_then(Value::as_bool).unwrap_or(true),
            page: pdf::PageOptions {
                size: s("page_size").unwrap_or_else(|| "A4".into()),
                landscape: s("orientation").as_deref() == Some("landscape"),
                margins: s("margins").unwrap_or_else(|| "20mm".into()),
                author: s("author"),
                subject: s("subject"),
            },
            password: s("password"),
        }
    }
}

impl Format for Document {
    fn options(&self) -> Vec<OptionField> {
        use OptionType::*;
        let mut v = vec![
            OptionField::new(
                "max_rows",
                Integer,
                "the most rows across the output's queries; more fails the run",
            )
            .range(Some(1.0), None)
            .default(DEFAULT_MAX_ROWS),
        ];
        if self.pdf {
            v.extend([
                OptionField::new(
                    "page_size",
                    String,
                    "paper size, used when the template's CSS sets no `@page` size",
                )
                .choices(pdf::PAGE_SIZES)
                .default("A4"),
                OptionField::new("orientation", String, "used when the template's CSS sets no `@page` size")
                    .choices(&["portrait", "landscape"])
                    .default("portrait"),
                OptionField::new(
                    "margins",
                    String,
                    "page margins as in CSS, like `20mm` or `15mm 20mm`; used when the CSS sets no `@page` margin",
                )
                .default("20mm"),
                OptionField::new("author", String, "the PDF's author metadata"),
                OptionField::new("subject", String, "the PDF's subject metadata"),
                OptionField::new(
                    "password",
                    String,
                    "an open password: the PDF is encrypted with AES-256 and opens only with it; Jinja like `{{ each.pdf_password }}`",
                ),
            ]);
        } else {
            v.push(
                OptionField::new(
                    "embed_assets",
                    Boolean,
                    "inline images, stylesheets and fonts as data URIs, so the file works wherever it's delivered",
                )
                .default(true),
            );
        }
        v
    }

    fn validate(&self, o: &Map<String, Value>) -> Vec<String> {
        let mut errs = Vec::new();
        if let Some(m) = o.get("margins").and_then(Value::as_str)
            && pdf::parse_margins(m).is_err()
        {
            errs.push(format!(
                "`margins` `{m}` must be one to four lengths, like `20mm` or `15mm 20mm` (units mm, cm, in, pt, px)"
            ));
        }
        if o.get("password").and_then(Value::as_str) == Some("") {
            errs.push(
                "`password` is empty; an empty password would leave the PDF unencrypted: set one, or remove the key"
                    .into(),
            );
        }
        errs
    }

    fn unknown_option_hint(&self, key: &str) -> Option<String> {
        (!self.pdf && key == "password").then(|| {
            "an HTML file can't carry a password: use `format: pdf`, or zip it with `zip: {password: ...}` on the destination".into()
        })
    }

    fn validate_template(&self, _options: &Map<String, Value>, t: &Value) -> (Vec<String>, Vec<String>) {
        match serde_json::from_value::<Check>(t.clone()) {
            Ok(c) => template::check(&c),
            Err(e) => (
                vec![format!("the template check from core is malformed: {e}")],
                Vec::new(),
            ),
        }
    }

    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        let ctx: Context = match &req.template {
            Some(t) => serde_json::from_value(t.clone())
                .map_err(|e| format!("the template context from core is malformed: {e}; update DRE"))?,
            None => {
                return Err(PluginError::new(
                    ErrorKind::Config,
                    format!(
                        "`{}` needs DRE 0.6.0 or later, which sends the template context",
                        req.format
                    ),
                )
                .into());
            }
        };
        let o = Options::parse(&req.options);
        let cal = ctx.calendar()?;
        let mut data = Data::default();
        while let Some(mut rs) = sets.next_set()? {
            let columns: Vec<String> = rs.schema.fields().iter().map(|f| f.name().clone()).collect();
            let types: Vec<(String, String)> = rs
                .schema
                .fields()
                .iter()
                .map(|f| (f.name().clone(), f.data_type().to_string()))
                .collect();
            let mut batches = Vec::new();
            while let Some(b) = rs.next_batch()? {
                data.rows += b.num_rows() as u64;
                if data.rows > o.max_rows {
                    return Err(format!(
                        "the document's queries return more than {} rows (`max_rows`); aggregate in SQL, filter the data, or raise `max_rows`",
                        o.max_rows
                    )
                    .into());
                }
                batches.push(b);
            }
            let formats = rs
                .meta
                .columns
                .iter()
                .filter_map(|(c, opts)| Some((c.clone(), opts.format.clone()?)))
                .collect();
            let result = dre_core::document::query_result(columns, &batches, cal);
            data.push(rs.meta.query.clone(), Arc::new(result), formats, types);
        }
        let rendered = template::render(&ctx, &data)?;
        for w in &rendered.warnings {
            sets.warn(w.clone());
        }
        let root = std::path::Path::new(&ctx.project_root);
        let base = rendered.base_dir.as_path();
        if self.pdf {
            let mut warnings = Vec::new();
            let mut resolve = |font| sets.request_fonts(font).map_err(|e| e.to_string());
            let resolver = if req.font_requests {
                Some(&mut resolve as &mut dyn FnMut(_) -> std::result::Result<Vec<String>, String>)
            } else {
                None
            };
            let mut bytes = pdf::render_pdf_with_fonts(
                &rendered.html,
                root,
                base,
                &o.page,
                &ctx,
                &mut warnings,
                resolver,
            )?;
            if let Some(pw) = &o.password {
                if dre_protocol::options::is_template(pw) {
                    return Err(PluginError::new(
                        ErrorKind::Config,
                        "`password` holds Jinja that wasn't rendered: a templated password needs DRE 0.7.0 or later",
                    )
                    .into());
                }
                if !pw.is_ascii() {
                    sets.warn(
                        "the PDF password has non-ASCII characters: macOS Preview and Safari can't open such files; keep passwords to ASCII letters, digits and punctuation"
                            .to_string(),
                    );
                }
                bytes = pdf::encrypt(&bytes, pw)?;
            }
            for w in warnings {
                sets.warn(w);
            }
            std::fs::write(&req.path, bytes).map_err(|e| format!("can't write {}: {e}", req.path))?;
        } else {
            let html = if o.embed_assets {
                assets::embed(&rendered.html, root, base)?
            } else {
                assets::check(&rendered.html, root, base)?;
                rendered.html
            };
            std::fs::write(&req.path, html).map_err(|e| format!("can't write {}: {e}", req.path))?;
        }
        Ok(vec![req.path.clone()])
    }
}
