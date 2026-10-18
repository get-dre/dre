//! The `document` package driven through the protocol, as core drives it: `write` with the
//! template context, result sets, then `finish`; and `validate` with a template check.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use dre_core::document::{Check, Context, RunInfo};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use dre_protocol::{Kind, PluginId};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-document"))
}

fn ctx(root: &Path, file: Option<PathBuf>) -> Context {
    Context {
        file: file.map(|f| f.to_string_lossy().into_owned()),
        project_root: root.to_string_lossy().into_owned(),
        run: RunInfo {
            report: "statement".into(),
            set: Some("acme".into()),
            target: "prod".into(),
            schedule: None,
            date: chrono::NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
            now: "2026-10-01T02:00:00Z".parse().unwrap(),
            scheduled_at: None,
            timezone: "Australia/Sydney".into(),
            week_start: "monday".into(),
            week_numbering: "iso".into(),
        },
        vars: serde_json::Map::from_iter([
            ("client".to_string(), json!("Acme <Pty> Ltd")),
            ("unused_by_sql".to_string(), json!("Net 30 days")),
        ]),
        cli_vars: Default::default(),
        locale: "en-AU".into(),
        each: None,
    }
}

fn headline() -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "balance",
            Arc::new(Float64Array::from(vec![12345.678])) as ArrayRef,
        ),
        ("members", Arc::new(Int64Array::from(vec![3])) as ArrayRef),
    ])
    .unwrap()
}

fn holdings(n: usize) -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "fund",
            Arc::new(StringArray::from(
                (0..n).map(|i| format!("Fund {i}")).collect::<Vec<_>>(),
            )) as ArrayRef,
        ),
        (
            "amount",
            Arc::new(Float64Array::from(
                (0..n).map(|i| i as f64 * 1000.5).collect::<Vec<_>>(),
            )) as ArrayRef,
        ),
        ("as_of", Arc::new(Date32Array::from(vec![20727; n])) as ArrayRef),
    ])
    .unwrap()
}

fn meta(query: &str, columns: &[(&str, &str)]) -> ResultSetMeta {
    ResultSetMeta {
        name: query.into(),
        query: query.into(),
        result_index: 1,
        anchor: None,
        header: None,
        columns: columns
            .iter()
            .map(|(c, f)| {
                (
                    c.to_string(),
                    ColumnOptions {
                        format: Some(f.to_string()),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        autofit: None,
        style: None,
    }
}

/// Write `format` with the two result sets; the file's bytes and the warnings, or the error.
fn write(
    format: &str,
    root: &Path,
    template: Option<PathBuf>,
    options: Value,
    rows: usize,
) -> Result<(Vec<u8>, Vec<String>), String> {
    let out = root.join(format!("target/out.{format}"));
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p =
        PluginProcess::start_for(bin(), Some(&PluginId::new(Kind::Format, format)), log, None).unwrap();
    let Value::Object(options) = options else { panic!() };
    let t = serde_json::to_value(ctx(root, template)).unwrap();
    p.write_begin(
        out.to_str().unwrap(),
        format,
        options,
        vec![
            meta("headline", &[]),
            meta("holdings", &[("amount", "$#,##0.00"), ("as_of", "d mmm yyyy")]),
        ],
        Some(t),
    )
    .unwrap();
    let h = headline();
    p.write_result_set(&h.schema(), vec![h.clone()])
        .map_err(|e| e.to_string())?;
    let b = holdings(rows);
    p.write_result_set(&b.schema(), vec![b.clone()])
        .map_err(|e| e.to_string())?;
    let (files, warnings) = p.write_finish_with_warnings().map_err(|e| e.to_string())?;
    let _ = p.close();
    assert_eq!(files, vec![out.to_string_lossy().to_string()]);
    Ok((std::fs::read(&out).unwrap(), warnings))
}

/// A project with a statement template, a shared base, terms in their own file, a logo and a
/// stylesheet.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    let files: &[(&str, &[u8])] = &[
        (
            "templates/base.html",
            br#"<!doctype html><html lang="en-AU"><head><title>{% block title %}{% endblock %}</title>
<link rel="stylesheet" href="assets/brand.css"></head>
<body><img src="assets/logo.svg">{% block body %}{% endblock %}<footer>{% include "terms.html" %}</footer></body></html>"#,
        ),
        ("templates/assets/brand.css", b"h1 { color: #1f7a4d; } body { font-family: Brand, sans-serif; }"),
        (
            "templates/assets/logo.svg",
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#1f7a4d"/></svg>"##,
        ),
        ("templates/terms.html", b"<p class=\"terms\">Terms: {{ var('unused_by_sql') }}</p>"),
        (
            "reports/statements/statement.html",
            br#"{% extends "base.html" %}{% block title %}Statement for {{ var('client') }}{% endblock %}
{% block body %}<h1>{{ var('client') }}</h1>
<p>Balance {{ results.headline.value | currency('AUD', 2) }} on {{ run.date | date('d mmmm yyyy') }} ({{ target.name }}, {{ run.set }})</p>
<p>Order: {% for q in results %}{{ q }} {% endfor %}</p>
{{ table(results.holdings) }}{% endblock %}"#,
        ),
    ];
    for (f, body) in files {
        let p = r.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    dir
}

#[test]
fn the_package_conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn html_is_one_self_contained_file_with_results_vars_and_context() {
    let dir = project();
    let t = dir.path().join("reports/statements/statement.html");
    let (bytes, warnings) = write("html", dir.path(), Some(t), json!({}), 3).unwrap();
    let html = String::from_utf8(bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // Escaped by default; a var no query uses; dates in the run's format; the context.
    assert!(html.contains("<h1>Acme &lt;Pty&gt; Ltd</h1>"), "{html}");
    assert!(html.contains("Terms: Net 30 days"), "{html}");
    assert!(
        html.contains("Balance A$12,345.68") || html.contains("Balance AUD"),
        "{html}"
    );
    assert!(html.contains("on 30 September 2026 (prod, acme)"), "{html}");
    assert!(html.contains("Order: headline holdings "), "{html}");
    // table() with the YAML column formats.
    assert!(html.contains("<td class=\"num\">$2,001.00</td>"), "{html}");
    assert!(html.contains("<td>1 Oct 2026</td>"), "{html}");
    // Self-contained: the stylesheet inlined, the logo a data URI.
    assert!(!html.contains("<link"), "{html}");
    assert!(html.contains("h1 { color: #1f7a4d; }"), "{html}");
    assert!(html.contains("src=\"data:image/svg+xml;base64,"), "{html}");
    // Kept as links on request.
    let t = dir.path().join("reports/statements/statement.html");
    let (bytes, _) = write("html", dir.path(), Some(t), json!({"embed_assets": false}), 3).unwrap();
    let html = String::from_utf8(bytes).unwrap();
    assert!(
        html.contains("href=\"../../templates/assets/brand.css\""),
        "{html}"
    );
}

#[test]
fn the_default_template_is_a_table_per_query() {
    let dir = project();
    let (bytes, _) = write("html", dir.path(), None, json!({}), 2).unwrap();
    let html = String::from_utf8(bytes).unwrap();
    assert!(
        html.contains("<h2>headline</h2>") && html.contains("<h2>holdings</h2>"),
        "{html}"
    );
    assert!(html.contains("Fund 1"), "{html}");
}

#[test]
fn an_undefined_value_names_the_line_and_the_columns() {
    let dir = project();
    let t = dir.path().join("reports/bad.html");
    std::fs::write(&t, "<p>ok</p>\n<p>{{ results.holdings.first.amout }}</p>").unwrap();
    let e = write("html", dir.path(), Some(t), json!({}), 1).unwrap_err();
    assert!(e.contains("reports/bad.html, line 2"), "{e}");
    assert!(e.contains("results.holdings (fund, amount, as_of)"), "{e}");
}

#[test]
fn max_rows_fails_with_a_hint() {
    let dir = project();
    let e = write("html", dir.path(), None, json!({"max_rows": 10}), 20).unwrap_err();
    assert!(
        e.contains("more than 10 rows (`max_rows`)") && e.contains("aggregate in SQL"),
        "{e}"
    );
}

#[test]
fn urls_and_files_outside_the_project_are_refused() {
    let dir = project();
    let t = dir.path().join("reports/url.html");
    std::fs::write(&t, r#"<img src="https://example.com/x.png">"#).unwrap();
    let e = write("html", dir.path(), Some(t.clone()), json!({}), 1).unwrap_err();
    assert!(e.contains("is a URL"), "{e}");
    let e = write("pdf", dir.path(), Some(t), json!({}), 1).unwrap_err();
    assert!(e.contains("is a URL"), "{e}");
    let t = dir.path().join("reports/escape.html");
    std::fs::write(&t, r#"{% include "../../etc/passwd" %}"#).unwrap();
    let e = write("html", dir.path(), Some(t), json!({}), 1).unwrap_err();
    assert!(e.contains("inside the project"), "{e}");
}

fn pages(pdf: &[u8]) -> usize {
    let s = String::from_utf8_lossy(pdf);
    s.matches("/Type /Page\n").count()
        + s.matches("/Type /Page ").count()
        + s.matches("/Type /Page>").count()
        + s.matches("/Type/Page/").count()
}

#[test]
fn pdf_is_paginated_deterministic_and_labelled() {
    let dir = project();
    let t = dir.path().join("reports/statements/statement.html");
    let (a, warnings) = write(
        "pdf",
        dir.path(),
        Some(t.clone()),
        json!({"author": "DRE tests"}),
        300,
    )
    .unwrap();
    assert!(a.starts_with(b"%PDF-"));
    assert!(pages(&a) > 1, "pages: {}", pages(&a));
    let s = String::from_utf8_lossy(&a);
    assert!(
        s.contains("/Title (Statement for Acme <Pty> Ltd)") || s.contains("/Title <"),
        "no title"
    );
    assert!(s.contains("/Author (DRE tests)"), "no author");
    assert!(s.contains("/Lang (en-AU)"), "no lang");
    assert!(s.contains("/CreationDate (D:20261001020000"), "no fixed date");
    // Only bundled or project fonts, subset.
    assert!(s.contains("IBMPlexSans"), "no bundled font");
    // `Brand` is declared nowhere: it falls back, with a warning naming it.
    assert!(
        warnings.iter().any(|w| w.contains("font `Brand`")),
        "{warnings:?}"
    );
    let (b, _) = write("pdf", dir.path(), Some(t), json!({"author": "DRE tests"}), 300).unwrap();
    assert!(a == b, "the same input made two different PDFs");
}

#[test]
fn validate_checks_the_template() {
    let dir = project();
    let r = dir.path();
    std::fs::write(
        r.join("reports/v.html"),
        r#"{% include "missing.html" %}{% include var('x') ~ ".html" %}{{ results.holding.value }}{{ 1 | nosuchfilter }}<img src="nope.png">{% if %}"#,
    )
    .unwrap();
    std::fs::write(
        r.join("reports/w.html"),
        r#"{% include "missing.html" %}{% include var('x') ~ ".html" %}{{ results.holding.value }}{{ 1 | nosuchfilter }}<img src="nope.png">"#,
    )
    .unwrap();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p =
        PluginProcess::start_for(bin(), Some(&PluginId::new(Kind::Format, "pdf")), log, None).unwrap();
    let check = |file: &str| {
        serde_json::to_value(Check {
            file: Some(r.join(file).to_string_lossy().into_owned()),
            project_root: r.to_string_lossy().into_owned(),
            queries: vec!["headline".into(), "holdings".into()],
        })
        .unwrap()
    };
    let (errors, _) = p
        .validate_with(Default::default(), Some(check("reports/v.html")))
        .unwrap();
    assert!(
        errors.iter().any(|e| e.contains("reports/v.html, line 1")),
        "syntax: {errors:?}"
    );
    let (errors, warnings) = p
        .validate_with(Default::default(), Some(check("reports/w.html")))
        .unwrap();
    let all = errors.join("\n");
    assert!(all.contains("missing.html"), "{all}");
    assert!(
        all.contains("`results.holding` isn't a query of this output")
            && all.contains("did you mean `results.holdings`"),
        "{all}"
    );
    assert!(all.contains("unknown filter `nosuchfilter`"), "{all}");
    assert!(all.contains("`nope.png` doesn't exist"), "{all}");
    assert!(warnings.iter().any(|w| w.contains("is computed")), "{warnings:?}");
    let (errors, warnings) = p
        .validate_with(
            Default::default(),
            Some(check("reports/statements/statement.html")),
        )
        .unwrap();
    assert!(
        errors.is_empty() && warnings.is_empty(),
        "{errors:?} {warnings:?}"
    );
    let _ = p.close();
}

#[test]
fn css_page_size_wins_over_the_options() {
    let dir = project();
    let t = dir.path().join("reports/a5.html");
    std::fs::write(
        &t,
        "<html><head><style>@page { size: A5; margin: 10mm }</style></head><body><p>x</p></body></html>",
    )
    .unwrap();
    let (pdf, _) = write("pdf", dir.path(), Some(t), json!({"page_size": "Letter"}), 1).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    // A5 is 419.53 x 595.28 points; Letter would be 612 x 792.
    let mb: Vec<&str> = s.match_indices("/MediaBox").map(|(i, _)| &s[i..i + 40]).collect();
    assert!(s.contains("/MediaBox [0 0 419.5"), "not A5: {mb:?}");
    // With no @page, the option applies.
    let t = dir.path().join("reports/plain.html");
    std::fs::write(&t, "<html><body><p>x</p></body></html>").unwrap();
    let (pdf, _) = write("pdf", dir.path(), Some(t), json!({"page_size": "Letter"}), 1).unwrap();
    assert!(
        String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 612"),
        "not Letter"
    );
}

#[test]
fn rows_format_columns_by_their_yaml_format_and_types_are_listed() {
    let dir = project();
    let t = dir.path().join("reports/fmt.html");
    std::fs::write(
        &t,
        "{% for r in results.holdings.rows %}[{{ r.fmt('amount') }}|{{ r.fmt('as_of') }}|{{ r.fmt('fund') }}|{{ r.amount }}]{% endfor %}\
         first={{ results.holdings.first.fmt('amount') }} \
         types={% for c, t in results.holdings.column_types | items %}{{ c }}:{{ t }};{% endfor %} \
         plain={{ results.headline.first.fmt('balance') }}",
    )
    .unwrap();
    let (bytes, _) = write("html", dir.path(), Some(t), json!({}), 2).unwrap();
    let html = String::from_utf8(bytes).unwrap();
    // YAML formats: `$#,##0.00` and `d mmm yyyy`; text as it is; the raw value stays a number.
    assert!(html.contains("[$1,000.50|1 Oct 2026|Fund 1|1000.5]"), "{html}");
    assert!(html.contains("first=$0.00"), "{html}");
    assert!(
        html.contains("types=fund:Utf8;amount:Float64;as_of:Date32;"),
        "{html}"
    );
    // No YAML format: shown as table() shows it.
    assert!(html.contains("plain=12,345.68"), "{html}");
    let t = dir.path().join("reports/bad.html");
    std::fs::write(&t, "{{ results.holdings.first.fmt('nope') }}").unwrap();
    let e = write("html", dir.path(), Some(t), json!({}), 1).unwrap_err();
    assert!(e.contains("the row has no column `nope`"), "{e}");
}

/// Every stream of a PDF, inflated where it's compressed.
fn streams(pdf: &[u8]) -> String {
    use std::io::Read;
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = pdf[at..].windows(6).position(|w| w == b"stream") {
        let start = at + i;
        at = start + 6;
        if start >= 3 && &pdf[start - 3..start] == b"end" {
            continue;
        }
        let body_at = at + if pdf[at..].starts_with(b"\r\n") { 2 } else { 1 };
        let end = pdf[body_at..]
            .windows(9)
            .position(|w| w == b"endstream")
            .map_or(pdf.len(), |e| body_at + e);
        let body = &pdf[body_at..end];
        let mut text = String::new();
        if flate2::read::ZlibDecoder::new(body)
            .read_to_string(&mut text)
            .is_err()
        {
            text = String::from_utf8_lossy(body).into_owned();
        }
        out.push_str(&text);
        out.push('\n');
        at = end;
    }
    out
}

#[test]
fn image_files_appear_in_the_pdf() {
    let dir = project();
    // The logo's fill, #1f7a4d, as a PDF colour operand.
    let drawn = |body: &str| {
        let t = dir.path().join("templates/img.html");
        std::fs::write(&t, format!("<html><body><p>x</p>{body}</body></html>")).unwrap();
        let (pdf, _) = write("pdf", dir.path(), Some(t), json!({}), 1).unwrap();
        let s = streams(&pdf);
        s.contains("0.12156863 0.4784313")
    };
    assert!(!drawn(""), "the logo's colour in a PDF without it");
    assert!(
        drawn(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#1f7a4d"/></svg>"##
        ),
        "inline svg"
    );
    let svg_file = drawn(r#"<img src="assets/logo.svg" alt="logo">"#);
    // A 1x1 PNG: a raster image is an image XObject.
    std::fs::write(
        dir.path().join("templates/assets/dot.png"),
        [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
            0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
            0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
            0x44, 0xAE, 0x42, 0x60, 0x82,
        ],
    )
    .unwrap();
    let t = dir.path().join("templates/png.html");
    std::fs::write(
        &t,
        r#"<html><body><p>x</p><img src="assets/dot.png" alt="dot" width="20" height="20"></body></html>"#,
    )
    .unwrap();
    let (pdf, _) = write("pdf", dir.path(), Some(t), json!({}), 1).unwrap();
    assert!(
        String::from_utf8_lossy(&pdf).contains("/Image") || streams(&pdf).contains("/Image"),
        "a PNG file"
    );
    assert!(svg_file, "an SVG file");
}
