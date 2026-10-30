//! The PDF engine, behind [`render_pdf`] alone: fulgur (Blitz for HTML and CSS layout, Krilla
//! for the PDF), in-process, offline. Nothing else in the plugin touches it, so it can be
//! replaced.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::LazyLock;

use dre_core::document::Context;
use fulgur::{AssetBundle, Engine, Margin, PageSize};
use regex::Regex;

use crate::{assets, fallback, fonts};

/// The `page_size` choices.
pub const PAGE_SIZES: &[&str] = &["A3", "A4", "A5", "Letter", "Legal"];

/// An `@page` rule's declarations (before any nested margin box).
static PAGE_RULE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)@page\b[^{]*\{([^{}]*)").expect("the pattern is a valid regex"));
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title>").expect("the pattern is a valid regex"));
static LANG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<html\b[^>]*?\blang\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#)
        .expect("the pattern is a valid regex")
});

/// What a PDF does when a script's fallback font can't be had (`missing_fonts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MissingFonts {
    /// Make the PDF, with boxes for the script's characters, and warn once per script per run.
    #[default]
    Warn,
    /// Fail the PDF.
    Error,
}

/// The page options, used where the template's CSS sets no `@page`.
pub struct PageOptions {
    pub size: String,
    pub landscape: bool,
    pub margins: String,
    pub author: Option<String>,
    pub subject: Option<String>,
}

/// Lay the rendered html out into a PDF. Assets resolve from `base` (the template's folder),
/// inside `root`. `warnings` gets fonts that fell back.
pub fn render_pdf(
    html: &str,
    root: &Path,
    base: &Path,
    page: &PageOptions,
    ctx: &Context,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    render_pdf_with_fonts(html, root, base, page, ctx, warnings, MissingFonts::Warn, None)
}

/// The font exchange's answer to a request: the font's file paths, or why core couldn't supply it.
pub type FontAnswer = Result<Vec<String>, dre_protocol::msg::FontError>;

/// Lay out a PDF, asking the host for the Noto fonts its uncovered scripts need before layout.
/// Without a resolver (older cores), keep the previous offline rendering behavior, unless
/// `missing` is [`MissingFonts::Error`] and the PDF needs a fallback font.
#[allow(clippy::too_many_arguments)]
pub fn render_pdf_with_fonts(
    html: &str,
    root: &Path,
    base: &Path,
    page: &PageOptions,
    ctx: &Context,
    warnings: &mut Vec<String>,
    missing: MissingFonts,
    resolver: Option<&mut dyn FnMut(dre_protocol::msg::FontRequest) -> FontAnswer>,
) -> Result<Vec<u8>, String> {
    let (html, files) = assets::for_pdf(html, root, base)?;
    let (mut html, project_fonts) = fonts::prepare(&html, &files, warnings)?;
    let mut fallback_fonts = Vec::new();
    match resolver {
        Some(resolve) => {
            let plan = fallback::plan(&html, &project_fonts);
            if let Some(note) = &plan.note {
                warnings.push(note.clone());
            }
            let mut families = BTreeMap::new();
            let mut unavailable = Vec::new();
            for &noto in &plan.fonts {
                let (name, version) = noto.package();
                let request = dre_protocol::msg::FontRequest {
                    name: name.into(),
                    version: version.into(),
                };
                // A font that downloaded but can't be read is new each time: never `repeated`.
                let loaded = resolve(request).and_then(|paths| load_fallback(&paths).map_err(Into::into));
                match loaded {
                    Ok((family, files)) => {
                        families.insert(noto, family);
                        fallback_fonts.extend(files);
                    }
                    Err(e) => unavailable.push((noto, e)),
                }
            }
            if missing == MissingFonts::Error && !unavailable.is_empty() {
                return Err(missing_fonts_error(&unavailable));
            }
            for (noto, e) in unavailable.iter().filter(|(_, e)| !e.repeated) {
                warnings.push(missing_font_warning(*noto, &e.message));
            }
            html = fallback::apply(&html, &plan, &families);
        }
        None if missing == MissingFonts::Error => {
            let plan = fallback::plan(&html, &project_fonts);
            if !plan.fonts.is_empty() {
                let scripts: Vec<_> = plan.fonts.iter().map(|n| n.label()).collect();
                let which = if scripts.len() == 1 {
                    scripts[0]
                } else {
                    "those scripts"
                };
                return Err(format!(
                    "no {} font for this PDF, so it isn't made (`missing_fonts: error`): fetching fonts needs DRE 0.9.0 or later; or add a font covering {which} to the project with `@font-face`",
                    or_list(&scripts)
                ));
            }
        }
        None => {}
    }
    let mut bundle = AssetBundle::new();
    for f in fonts::BUNDLED {
        bundle
            .add_font_bytes(f.to_vec())
            .map_err(|e| format!("the bundled font: {e}"))?;
    }
    for f in project_fonts {
        bundle
            .add_font_bytes(f.bytes)
            .map_err(|e| format!("a project font: {e}"))?;
    }
    for bytes in fallback_fonts {
        bundle
            .add_font_bytes(bytes)
            .map_err(|e| format!("a fallback font: {e}"))?;
    }
    for (name, bytes) in files {
        bundle.add_image(name, bytes);
    }
    let size = match page.size.as_str() {
        "A3" => PageSize::A3,
        "A5" => PageSize::A5,
        "Letter" => PageSize::LETTER,
        "Legal" => PageSize::LEGAL,
        _ => PageSize::A4,
    };
    // The template's CSS decides the page; the options fill in only what it leaves unset.
    let css_sets = |prop: &str| {
        let decl = Regex::new(&format!(r"(?i)(^|[;\s]){prop}\s*:")).expect("valid");
        PAGE_RULE.captures_iter(&html).any(|c| decl.is_match(&c[1]))
    };
    let mut b = Engine::builder()
        .system_fonts(false)
        .creation_date(ctx.run.now.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .creator(format!("DRE {}", env!("CARGO_PKG_VERSION")))
        .producer("DRE document plugin")
        .assets(bundle);
    if !css_sets("size") {
        b = b.page_size(size).landscape(page.landscape);
    }
    if !css_sets("margin") {
        b = b.margin(parse_margins(&page.margins)?);
    }
    if let Some(t) = TITLE.captures(&html) {
        let t = unescape(t[1].trim());
        if !t.is_empty() {
            b = b.title(t);
        }
    }
    if let Some(l) = LANG.captures(&html) {
        b = b.lang(l.get(1).or(l.get(2)).or(l.get(3)).map_or("", |m| m.as_str()));
    }
    if let Some(a) = &page.author {
        b = b.author(a.clone());
    }
    if let Some(s) = &page.subject {
        b = b.description(s.clone());
    }
    let engine = b.build();
    match catch_unwind(AssertUnwindSafe(|| engine.render(&html))) {
        Ok(Ok(pdf)) => Ok(pdf),
        Ok(Err(e)) => Err(format!("the PDF layout failed: {e}")),
        Err(_) => Err(
            "the PDF layout failed inside the layout engine; a very long table (over about 30,000 rows) does this: split it, or aggregate in SQL"
                .into(),
        ),
    }
}

/// The ways to get a missing font, for `codes` (`dre fonts install` codes, space-separated) and
/// `which` (the script, or "those scripts").
fn fixes(codes: &str, which: &str) -> String {
    format!(
        "To fix it, set `DRE_REGISTRY_URL` to an offline copy of the registry, run `dre fonts install {codes}` on a connected machine and copy its `~/.dre/fonts` here, or add a font covering {which} to the project with `@font-face`"
    )
}

/// The warning for a script whose font core couldn't supply (`missing_fonts: warn`).
fn missing_font_warning(noto: fallback::Noto, reason: &str) -> String {
    let label = noto.label();
    format!(
        "no {label} font for this PDF, so its {label} text shows as boxes ({reason}). {}",
        fixes(noto.code(), label)
    )
}

/// The error for `missing_fonts: error`, naming every script without a font.
fn missing_fonts_error(unavailable: &[(fallback::Noto, dre_protocol::msg::FontError)]) -> String {
    let labels: Vec<_> = unavailable.iter().map(|(n, _)| n.label()).collect();
    let reasons: Vec<_> = unavailable
        .iter()
        .map(|(n, e)| format!("{}: {}", n.label(), e.message))
        .collect();
    let codes: Vec<_> = unavailable.iter().map(|(n, _)| n.code()).collect();
    format!(
        "no {} font for this PDF ({}), so it isn't made (`missing_fonts: error`). {}",
        or_list(&labels),
        reasons.join("; "),
        fixes(&codes.join(" "), which(&labels))
    )
}

/// The script, or "those scripts" for several.
fn which<'a>(labels: &[&'a str]) -> &'a str {
    match labels {
        [one] => one,
        _ => "those scripts",
    }
}

/// "A", "A or B", "A, B or C".
fn or_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// A fetched package's files and the family inside them. Any unreadable file fails the package,
/// which counts as a missing font: a warning, or an error with `missing_fonts: error`.
fn load_fallback(paths: &[String]) -> Result<(String, Vec<Vec<u8>>), String> {
    let mut family = None;
    let mut files = Vec::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
        let name = fonts::family_name(&bytes).ok_or_else(|| format!("{path} isn't a font file"))?;
        family.get_or_insert(name);
        files.push(bytes);
    }
    Ok((family.ok_or("the registry gave no font files")?, files))
}

/// Encrypt a PDF with an open (user) `password`: AES-256 (PDF 2.0, revision 6), every
/// permission granted, and a random owner password nobody keeps, so the encryption can't be
/// lifted with a blank owner password.
pub fn encrypt(pdf: &[u8], password: &str) -> Result<Vec<u8>, String> {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
    use lopdf::{Document, EncryptionState, EncryptionVersion, Permissions};

    if password.is_empty() {
        return Err("`password` is empty; an empty password would leave the PDF unencrypted".into());
    }
    let mut key = [0u8; 32];
    let mut owner = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("no random numbers for the PDF key: {e}"))?;
    getrandom::fill(&mut owner).map_err(|e| format!("no random numbers for the PDF key: {e}"))?;
    let owner: String = owner.iter().map(|b| format!("{b:02x}")).collect();
    let mut doc = Document::load_mem(pdf).map_err(|e| format!("can't read the PDF to encrypt it: {e}"))?;
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &owner,
        user_password: password,
        permissions: Permissions::all(),
    })
    // Never the password itself in the message: it may be a member's secret.
    .map_err(|e| format!("the password can't be used to encrypt the PDF: {e}"))?;
    doc.encrypt(&state)
        .map_err(|e| format!("can't encrypt the PDF: {e}"))?;
    let mut out = Vec::with_capacity(pdf.len() + 1024);
    doc.save_to(&mut out)
        .map_err(|e| format!("can't write the encrypted PDF: {e}"))?;
    Ok(out)
}

/// CSS-style margins: one to four lengths (`20mm`, `15mm 20mm`), in mm, cm, in, pt or px.
pub fn parse_margins(s: &str) -> Result<Margin, String> {
    let pts: Vec<f32> = s
        .split_whitespace()
        .map(length_pt)
        .collect::<Option<_>>()
        .ok_or_else(|| format!("`margins` `{s}` isn't one to four lengths"))?;
    let [top, right, bottom, left] = match pts.as_slice() {
        [a] => [*a; 4],
        [v, h] => [*v, *h, *v, *h],
        [t, h, b] => [*t, *h, *b, *h],
        [t, r, b, l] => [*t, *r, *b, *l],
        _ => return Err(format!("`margins` `{s}` isn't one to four lengths")),
    };
    Ok(Margin {
        top,
        right,
        bottom,
        left,
    })
}

fn length_pt(s: &str) -> Option<f32> {
    let unit_at = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (n, unit) = s.split_at(unit_at);
    let n: f32 = n.parse().ok()?;
    if n < 0.0 {
        return None;
    }
    Some(match unit {
        "mm" => n * 72.0 / 25.4,
        "cm" => n * 72.0 / 2.54,
        "in" => n * 72.0,
        "pt" => n,
        "px" => n * 0.75,
        _ => return None,
    })
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margins() {
        let m = parse_margins("10mm 1in").unwrap();
        assert!((m.top - 28.346).abs() < 0.01 && (m.right - 72.0).abs() < 0.01);
        assert!(parse_margins("10").is_err());
        assert!(parse_margins("1mm 2mm 3mm 4mm 5mm").is_err());
    }
}
