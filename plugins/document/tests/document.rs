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
    p.write_result_set(&h.schema(), vec![h])
        .map_err(|e| e.to_string())?;
    let b = holdings(rows);
    p.write_result_set(&b.schema(), vec![b])
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

#[test]
fn a_pdf_password_encrypts_with_aes_256_and_a_random_owner_password() {
    let dir = project();
    let t = dir.path().join("reports/statements/statement.html");
    let (plain, _) = write("pdf", dir.path(), Some(t.clone()), json!({}), 3).unwrap();
    let (a, _) = write(
        "pdf",
        dir.path(),
        Some(t.clone()),
        json!({"password": "s3cret-Ü"}),
        3,
    )
    .unwrap();
    assert!(!lopdf::Document::load_mem(&plain).unwrap().is_encrypted());
    let doc = lopdf::Document::load_mem(&a).unwrap();
    assert!(doc.is_encrypted());
    // AES-256: the standard handler's version 5, revision 6.
    let enc = doc.get_encrypted().unwrap();
    assert_eq!(enc.get(b"V").unwrap().as_i64().unwrap(), 5);
    assert_eq!(enc.get(b"R").unwrap().as_i64().unwrap(), 6);
    // Opens with the password, not without or with a wrong one, nor with a blank owner password.
    assert!(doc.authenticate_password("s3cret-Ü").is_ok());
    assert!(doc.authenticate_password("").is_err());
    assert!(doc.authenticate_password("s3cret").is_err());
    assert!(doc.authenticate_owner_password("").is_err());
    let opened =
        lopdf::Document::load_mem_with_options(&a, lopdf::LoadOptions::with_password("s3cret-Ü")).unwrap();
    let pages = opened.get_pages().len();
    assert!(pages > 0 && pages == lopdf::Document::load_mem(&plain).unwrap().get_pages().len());
    // The password never appears in the file; each run draws a new owner password and key.
    assert!(!String::from_utf8_lossy(&a).contains("s3cret"));
    let (b, _) = write("pdf", dir.path(), Some(t), json!({"password": "s3cret-Ü"}), 3).unwrap();
    let o = |pdf: &[u8]| {
        let d = lopdf::Document::load_mem(pdf).unwrap();
        d.get_encrypted()
            .unwrap()
            .get(b"O")
            .unwrap()
            .as_str()
            .unwrap()
            .to_vec()
    };
    assert_ne!(o(&a), o(&b));
}

#[test]
fn an_empty_or_unrendered_password_fails_and_html_takes_none() {
    let dir = project();
    let t = dir.path().join("reports/statements/statement.html");
    let e = write("pdf", dir.path(), Some(t.clone()), json!({"password": ""}), 3).unwrap_err();
    assert!(e.contains("`password` is empty"), "{e}");
    let e = write(
        "pdf",
        dir.path(),
        Some(t.clone()),
        json!({"password": "{{ each.pw }}"}),
        3,
    )
    .unwrap_err();
    assert!(e.contains("DRE 0.7.0"), "{e}");
    let e = write("html", dir.path(), Some(t), json!({"password": "x"}), 3).unwrap_err();
    assert!(
        e.contains("unknown option `password`") && e.contains("zip: {password"),
        "{e}"
    );
    let log: LogSink = Arc::new(|_, _| {});
    let mut p =
        PluginProcess::start_for(bin(), Some(&PluginId::new(Kind::Format, "pdf")), log, None).unwrap();
    let Value::Object(o) = json!({"password": ""}) else {
        panic!()
    };
    let (errors, _) = p.validate_with(o, None).unwrap();
    assert!(
        errors.iter().any(|e| e.contains("`password` is empty")),
        "{errors:?}"
    );
    let Value::Object(o) = json!({"password": "{{ each.pw }}"}) else {
        panic!()
    };
    let (errors, _) = p.validate_with(o, None).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let _ = p.close();
}

#[test]
fn japanese_pdf_requests_fallback_fonts_before_layout() {
    let dir = tempfile::tempdir().unwrap();
    let template = dir.path().join("letter.html");
    std::fs::write(
        &template,
        "<html lang=\"ja\"><body>こんにちは, <b>東京</b></body></html>",
    )
    .unwrap();
    let out = dir.path().join("letter.pdf");
    let mut p = PluginProcess::start_for(
        bin(),
        Some(&PluginId::new(Kind::Format, "pdf")),
        Arc::new(|_, _| {}),
        None,
    )
    .unwrap();
    p.write_begin_with_font_requests(
        out.to_str().unwrap(),
        "pdf",
        Default::default(),
        vec![],
        Some(serde_json::to_value(ctx(dir.path(), Some(template))).unwrap()),
        true,
    )
    .unwrap();
    let mut requested = false;
    let (_, warnings) = p
        .write_finish_with_fonts(&mut |font| {
            assert_eq!(font.name, "noto_sans_jp");
            assert_eq!(font.version, "2.4.0");
            requested = true;
            Err("test registry unavailable".into())
        })
        .unwrap();
    p.close().unwrap();
    assert!(requested, "Japanese PDF must request its fallback before layout");
    assert!(warnings.iter().any(|w| w.contains("test registry unavailable")));
}

fn japanese_font_paths() -> Vec<String> {
    if let Some(dir) = std::env::var_os("DRE_TEST_FONT_DIR") {
        return ["NotoSansCJKjp-Regular-2.004.otf", "NotoSansCJKjp-Bold-2.004.otf"]
            .iter()
            .map(|name| Path::new(&dir).join(name).to_string_lossy().into_owned())
            .collect();
    }
    static CACHE: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().unwrap());
    let env = dre_core::context::Env::from_process().with_home(CACHE.path().to_path_buf());
    dre_core::fonts::resolve(
        &dre_core::context::Context::subprocess(env),
        &dre_protocol::msg::FontRequest {
            name: "noto_sans_jp".into(),
            version: "2.4.0".into(),
        },
    )
    .expect("the pinned Japanese font test assets must be available in the registry or DRE_TEST_FONT_DIR")
}

fn write_japanese(format: &str, root: &Path, html: &str) -> (Vec<u8>, usize) {
    let template = root.join("letter.html");
    std::fs::write(&template, html).unwrap();
    let out = root.join(format!("letter.{format}"));
    let mut p = PluginProcess::start_for(
        bin(),
        Some(&PluginId::new(Kind::Format, format)),
        Arc::new(|_, _| {}),
        None,
    )
    .unwrap();
    p.write_begin_with_font_requests(
        out.to_str().unwrap(),
        format,
        Default::default(),
        vec![],
        Some(serde_json::to_value(ctx(root, Some(template))).unwrap()),
        true,
    )
    .unwrap();
    let mut requests = 0;
    let (_, warnings) = p
        .write_finish_with_fonts(&mut |font| {
            assert_eq!(font.name, "noto_sans_jp");
            assert_eq!(font.version, "2.4.0");
            requests += 1;
            Ok(japanese_font_paths())
        })
        .unwrap();
    p.close().unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    (std::fs::read(out).unwrap(), requests)
}

fn assert_real_japanese_glyphs(pdf: &[u8]) {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let page_numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    let extracted = doc.extract_text(&page_numbers).unwrap();
    assert!(extracted.contains("こんにちは"), "{extracted:?}");
    assert!(extracted.contains("東京"), "{extracted:?}");
    let mut glyphs = Vec::new();
    for page in doc.get_pages().values() {
        let content = lopdf::content::Content::decode(&doc.get_page_content(*page)).unwrap();
        for op in content.operations {
            if op.operator == "Tj" || op.operator == "TJ" {
                fn collect(object: &lopdf::Object, glyphs: &mut Vec<u16>) {
                    match object {
                        lopdf::Object::String(bytes, _) => {
                            let (chunks, remainder) = bytes.as_chunks::<2>();
                            assert!(remainder.is_empty());
                            glyphs.extend(chunks.iter().map(|b| u16::from_be_bytes(*b)));
                        }
                        lopdf::Object::Array(items) => {
                            for item in items {
                                collect(item, glyphs);
                            }
                        }
                        _ => {}
                    }
                }
                for operand in &op.operands {
                    collect(operand, &mut glyphs);
                }
            }
        }
    }
    assert!(glyphs.len() >= 7, "expected Japanese text, got {glyphs:?}");
    assert!(
        glyphs.iter().all(|gid| *gid != 0),
        "PDF used .notdef glyph: {glyphs:?}"
    );
    let text = String::from_utf8_lossy(pdf);
    assert!(
        text.contains("NotoSansCJKjp-Regular"),
        "regular Japanese font not embedded"
    );
    assert!(
        text.contains("NotoSansCJKjp-Bold"),
        "bold Japanese font not embedded"
    );
}

#[test]
fn japanese_pdf_embeds_regular_and_bold_without_missing_glyphs() {
    let dir = tempfile::tempdir().unwrap();
    let (pdf, requests) = write_japanese(
        "pdf",
        dir.path(),
        "<html lang=\"ja\"><body>こんにちは, <b>東京</b></body></html>",
    );
    assert_real_japanese_glyphs(&pdf);
    assert_eq!(requests, 1);
}

#[test]
fn project_japanese_font_prevents_fetch_and_html_never_fetches() {
    let dir = tempfile::tempdir().unwrap();
    let paths = japanese_font_paths();
    for (source, target) in paths.iter().zip(["brand.otf", "brand-bold.otf"]) {
        std::fs::copy(source, dir.path().join(target)).unwrap();
    }
    let html = "<html lang=\"ja\"><head><style>@font-face {font-family: Brand; src: url(brand.otf);} @font-face {font-family: Brand; src: url(brand-bold.otf);font-weight:bold;} body {font-family:Brand;}</style></head><body>こんにちは, <b>東京</b></body></html>";
    let (pdf, requests) = write_japanese("pdf", dir.path(), html);
    assert_eq!(requests, 0);
    assert_real_japanese_glyphs(&pdf);
    let (html, requests) = write_japanese("html", dir.path(), "<p>こんにちは, 東京</p>");
    assert_eq!(requests, 0);
    assert_eq!(html, "<p>こんにちは, 東京</p>".as_bytes());
}

#[test]
fn older_core_keeps_rendering_japanese_without_the_new_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let template = dir.path().join("legacy.html");
    std::fs::write(&template, "<p>こんにちは, 東京</p>").unwrap();
    let (pdf, _) = write("pdf", dir.path(), Some(template), json!({}), 1).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn fallback_preserves_the_projects_latin_font() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("brand.ttf"),
        include_bytes!("../fonts/IBMPlexMono-Regular.ttf"),
    )
    .unwrap();
    let (pdf, requests) = write_japanese(
        "pdf",
        dir.path(),
        "<html><head><style>@font-face {font-family: Brand; src:url(brand.ttf);} body {font-family:Brand;}</style></head><body>Brand Latin こんにちは <b>東京</b></body></html>",
    );
    assert_eq!(requests, 1);
    assert_real_japanese_glyphs(&pdf);
    assert!(
        String::from_utf8_lossy(&pdf).contains("IBMPlexMono"),
        "project font lost its priority"
    );
}

#[test]
fn generated_japanese_content_requests_fonts() {
    let dir = tempfile::tempdir().unwrap();
    let (pdf, requests) = write_japanese(
        "pdf",
        dir.path(),
        "<html><head><style>p::before {content:'こんにちは, ';} b::before {content:'東京';}</style></head><body><p></p><b></b></body></html>",
    );
    assert_real_japanese_glyphs(&pdf);
    assert_eq!(requests, 1);
}

#[test]
fn escaped_generated_japanese_content_requests_fonts() {
    let dir = tempfile::tempdir().unwrap();
    let (pdf, requests) = write_japanese(
        "pdf",
        dir.path(),
        r"<html><head><style>p::before {content:'\3053 \3093 \306B \3061 \306F , ';} b::before {content:'\6771 \4EAC ';}</style></head><body><p></p><b></b></body></html>",
    );
    assert_real_japanese_glyphs(&pdf);
    assert_eq!(requests, 1);
}

#[test]
fn uppercase_numeric_japanese_entities_request_fonts() {
    let dir = tempfile::tempdir().unwrap();
    let (pdf, requests) = write_japanese(
        "pdf",
        dir.path(),
        "<html><body>&#X3053;&#X3093;&#X306B;&#X3061;&#X306F;, <b>&#X6771;&#X4EAC;</b></body></html>",
    );
    assert_real_japanese_glyphs(&pdf);
    assert_eq!(requests, 1);
}

#[test]
fn declared_japanese_fonts_are_actual_fallbacks_outside_their_selected_elements() {
    let dir = tempfile::tempdir().unwrap();
    for (source, target) in japanese_font_paths().iter().zip(["brand.otf", "brand-bold.otf"]) {
        std::fs::copy(source, dir.path().join(target)).unwrap();
    }
    for rule in ["", "span {font-family:Brand;}"] {
        let html = format!(
            "<html><head><style>@font-face {{font-family:Brand;src:url(brand.otf);}} @font-face {{font-family:Brand;src:url(brand-bold.otf);font-weight:bold;}} {rule}</style></head><body><span>Brand</span>こんにちは, <b>東京</b></body></html>"
        );
        let (pdf, requests) = write_japanese("pdf", dir.path(), &html);
        assert_real_japanese_glyphs(&pdf);
        assert_eq!(requests, 0);
    }
}

#[test]
fn font_downloads_keep_the_runs_project_http_timeout() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/packages.json", listener.local_addr().unwrap());
    // Each request gets a complete response after two seconds. A one-second project timeout
    // must fail first; the default timeout would instead reach the HTTP 404 response.
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let server = std::thread::spawn(move || {
        while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    std::thread::spawn(move || {
                        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut request = [0; 2048];
                        let _ = socket.read(&mut request);
                        std::thread::sleep(Duration::from_secs(2));
                        let _ = socket.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("{e}"),
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let template = dir.path().join("timeout.html");
    std::fs::write(&template, "<p>こんにちは, 東京</p>").unwrap();
    let env =
        dre_core::context::Env::from_vars([("DRE_REGISTRY_URL", url)]).with_home(dir.path().join("home"));
    let run = dre_core::context::Context::subprocess(env);
    let mut plugin = run
        .host()
        .start(
            &dre_core::plugins::Located {
                path: bin().to_path_buf(),
                plugin: PluginId::new(Kind::Format, "pdf"),
                package: "document".into(),
            },
            Arc::new(|_, _| {}),
            None,
        )
        .unwrap();
    // Cover processes started before the project's flags finished loading.
    run.set_project_http_timeout(1);
    run.set_project_http_timeout(50);
    plugin
        .write(dre_core::plugin_host::WriteRequest {
            path: dir.path().join("timeout.pdf").to_string_lossy().into_owned(),
            format: "pdf".into(),
            options: Default::default(),
            result_sets: vec![],
            template: Some(serde_json::to_value(ctx(dir.path(), Some(template))).unwrap()),
        })
        .unwrap();
    let written = plugin.finish().unwrap();
    plugin.close();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert!(
        written.warnings.iter().any(|w| w.contains("no answer for 1s")),
        "{:?}",
        written.warnings
    );
}

/// A pinned Noto package from the registry (`DRE_REGISTRY_URL` points tests at a mirror), cached
/// once per test binary.
fn noto_paths(name: &str, version: &str) -> Vec<String> {
    static CACHE: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().unwrap());
    let env = dre_core::context::Env::from_process().with_home(CACHE.path().to_path_buf());
    dre_core::fonts::resolve(
        &dre_core::context::Context::subprocess(env),
        &dre_protocol::msg::FontRequest {
            name: name.into(),
            version: version.into(),
        },
    )
    .unwrap_or_else(|e| panic!("the pinned {name} {version} test assets must be in the registry: {e}"))
}

/// Write a PDF whose font requests core answers from the registry. Returns the PDF, the
/// requested packages in order, and the warnings.
fn write_with_noto(html: &str) -> (Vec<u8>, Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let template = dir.path().join("multi.html");
    std::fs::write(&template, html).unwrap();
    let out = dir.path().join("multi.pdf");
    let mut p = PluginProcess::start_for(
        bin(),
        Some(&PluginId::new(Kind::Format, "pdf")),
        Arc::new(|_, _| {}),
        None,
    )
    .unwrap();
    p.write_begin_with_font_requests(
        out.to_str().unwrap(),
        "pdf",
        Default::default(),
        vec![],
        Some(serde_json::to_value(ctx(dir.path(), Some(template))).unwrap()),
        true,
    )
    .unwrap();
    let mut requested = Vec::new();
    let (_, warnings) = p
        .write_finish_with_fonts(&mut |font| {
            requested.push(font.name.clone());
            Ok(noto_paths(&font.name, &font.version))
        })
        .unwrap();
    p.close().unwrap();
    (std::fs::read(out).unwrap(), requested, warnings)
}

/// Every glyph the PDF draws, in content-stream order: its font's `BaseFont` (without the
/// subset tag and encoding), its glyph code, and the text its `ToUnicode` map gives that code.
fn drawn_glyphs(pdf: &[u8]) -> Vec<(String, u16, String)> {
    use std::collections::HashMap;

    fn to_unicode(doc: &lopdf::Document, font: &lopdf::Dictionary) -> HashMap<u16, String> {
        let mut map = HashMap::new();
        let Ok(stream) = font
            .get(b"ToUnicode")
            .and_then(|o| o.as_reference())
            .and_then(|id| doc.get_object(id))
            .and_then(|o| o.as_stream())
        else {
            return map;
        };
        let text = String::from_utf8_lossy(&stream.decompressed_content().unwrap()).into_owned();
        let hex = regex::Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>").unwrap();
        for section in text.split("beginbfchar").skip(1) {
            let section = section.split("endbfchar").next().unwrap();
            for c in hex.captures_iter(section) {
                let code = u16::from_str_radix(&c[1], 16).unwrap();
                let units: Vec<u16> = c[2]
                    .as_bytes()
                    .chunks(4)
                    .map(|u| u16::from_str_radix(std::str::from_utf8(u).unwrap(), 16).unwrap())
                    .collect();
                map.insert(code, String::from_utf16_lossy(&units));
            }
        }
        map
    }

    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let mut glyphs = Vec::new();
    for page in doc.get_pages().values() {
        let fonts = doc.get_page_fonts(*page).unwrap();
        let maps: HashMap<Vec<u8>, (String, HashMap<u16, String>)> = fonts
            .iter()
            .map(|(name, font)| {
                let base =
                    String::from_utf8_lossy(font.get(b"BaseFont").unwrap().as_name().unwrap()).into_owned();
                let base = base.split_once('+').map_or(base.as_str(), |(_, b)| b);
                let base = base.trim_end_matches("-Identity-H").to_string();
                (name.clone(), (base, to_unicode(&doc, font)))
            })
            .collect();
        let content = lopdf::content::Content::decode(&doc.get_page_content(*page)).unwrap();
        let mut current: Option<&(String, HashMap<u16, String>)> = None;
        for op in content.operations {
            if op.operator == "Tf" {
                current = op.operands[0].as_name().ok().and_then(|n| maps.get(n));
            }
            if op.operator != "Tj" && op.operator != "TJ" {
                continue;
            }
            let (base, map) = current.expect("text is drawn with a font selected");
            let mut strings = Vec::new();
            for operand in &op.operands {
                match operand {
                    lopdf::Object::String(bytes, _) => strings.push(bytes.clone()),
                    lopdf::Object::Array(items) => strings.extend(items.iter().filter_map(|i| match i {
                        lopdf::Object::String(bytes, _) => Some(bytes.clone()),
                        _ => None,
                    })),
                    _ => {}
                }
            }
            for bytes in strings {
                for pair in bytes.chunks(2) {
                    let code = u16::from_be_bytes([pair[0], pair[1]]);
                    glyphs.push((base.clone(), code, map.get(&code).cloned().unwrap_or_default()));
                }
            }
        }
    }
    glyphs
}

/// The fonts that draw `text`'s characters.
fn fonts_drawing(glyphs: &[(String, u16, String)], text: &str) -> std::collections::BTreeSet<String> {
    glyphs
        .iter()
        .filter(|(_, _, t)| !t.is_empty() && t.chars().all(|c| text.contains(c)))
        .map(|(base, _, _)| base.clone())
        .collect()
}

#[test]
fn a_mixed_japanese_and_chinese_document_draws_each_with_its_lang_font() {
    // The same Han character in both: only the `lang` attribute decides which font draws it.
    let (pdf, requested, warnings) = write_with_noto(
        r#"<html lang="ja"><body><p>直す</p><table><tr><td lang="zh-Hans">直</td><td lang="zh-TW"><b>直</b></td></tr></table></body></html>"#,
    );
    assert_eq!(requested, ["noto_sans_jp", "noto_sans_sc", "noto_sans_tc"]);
    assert!(warnings.is_empty(), "{warnings:?}");
    let glyphs = drawn_glyphs(&pdf);
    assert!(glyphs.iter().all(|(_, code, _)| *code != 0), "{glyphs:?}");
    assert_eq!(
        fonts_drawing(&glyphs, "直"),
        [
            "NotoSansCJKjp-Regular",
            "NotoSansCJKsc-Regular",
            "NotoSansCJKtc-Bold"
        ]
        .map(String::from)
        .into(),
        "{glyphs:?}"
    );
}

#[test]
fn han_without_lang_is_simplified_chinese_with_a_note() {
    let (pdf, requested, warnings) = write_with_noto("<p>北京</p>");
    assert_eq!(requested, ["noto_sans_sc"]);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("`lang`"), "{warnings:?}");
    assert_eq!(
        fonts_drawing(&drawn_glyphs(&pdf), "北京"),
        ["NotoSansCJKsc-Regular".to_string()].into()
    );
}

#[test]
fn arabic_joins_right_to_left() {
    // Three meems: initial, medial and final forms are three different glyphs. Isolated
    // (unshaped) letters would repeat one glyph.
    let (pdf, requested, warnings) = write_with_noto("<p>ممم بحر</p>");
    assert_eq!(requested, ["noto_sans_arabic"]);
    assert!(warnings.is_empty(), "{warnings:?}");
    let glyphs = drawn_glyphs(&pdf);
    let arabic: Vec<_> = glyphs
        .iter()
        .filter(|(base, ..)| base.starts_with("NotoSansArabic"))
        .collect();
    assert!(arabic.iter().all(|(_, code, _)| *code != 0), "{glyphs:?}");
    let meems: std::collections::BTreeSet<u16> = arabic
        .iter()
        .filter(|(.., t)| t == "م")
        .map(|(_, code, _)| *code)
        .collect();
    assert_eq!(meems.len(), 3, "meem should take three joined forms: {glyphs:?}");
    // Drawn left to right on the page, so a right-to-left word comes out last letter first.
    let word: String = arabic
        .iter()
        .map(|(.., t)| t.as_str())
        .filter(|t| "بحر".contains(*t))
        .collect();
    assert_eq!(word, "رحب", "{glyphs:?}");
}

#[test]
fn devanagari_conjuncts_shape() {
    let (pdf, requested, warnings) = write_with_noto("<p>नमस्ते</p>");
    assert_eq!(requested, ["noto_sans_devanagari"]);
    assert!(warnings.is_empty(), "{warnings:?}");
    let glyphs = drawn_glyphs(&pdf);
    let deva: Vec<_> = glyphs
        .iter()
        .filter(|(base, ..)| base.starts_with("NotoSansDevanagari"))
        .collect();
    assert!(
        !deva.is_empty() && deva.iter().all(|(_, code, _)| *code != 0),
        "{glyphs:?}"
    );
    // स्त is one conjunct: the virama is never drawn on its own, and six code points take fewer
    // glyphs.
    assert!(deva.iter().all(|(.., t)| t != "\u{094D}"), "{glyphs:?}");
    assert!(deva.len() < "नमस्ते".chars().count(), "{glyphs:?}");
}

#[test]
fn hebrew_thai_and_korean_draw_with_their_fonts() {
    let (pdf, requested, warnings) = write_with_noto(r#"<p>שלום</p><p>สวัสดี</p><p lang="ko">한국어 漢字</p>"#);
    assert_eq!(requested, ["noto_sans_kr", "noto_sans_hebrew", "noto_sans_thai"]);
    assert!(warnings.is_empty(), "{warnings:?}");
    let glyphs = drawn_glyphs(&pdf);
    assert!(glyphs.iter().all(|(_, code, _)| *code != 0), "{glyphs:?}");
    for (text, font) in [
        ("שלום", "NotoSansHebrew-Regular"),
        ("漢字", "NotoSansCJKkr-Regular"),
    ] {
        assert_eq!(
            fonts_drawing(&glyphs, text),
            [font.to_string()].into(),
            "{text}: {glyphs:?}"
        );
    }
    assert!(
        glyphs.iter().any(|(base, ..)| base == "NotoSansThai-Regular"),
        "{glyphs:?}"
    );
}
