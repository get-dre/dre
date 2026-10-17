//! A document's images, stylesheets and fonts: `<img src>`, `<link rel="stylesheet" href>` and
//! CSS `url()` (in `<style>`, `style=""` and linked stylesheets). Each resolves relative to the
//! file it's written in (the template, or a stylesheet), must be inside the project, and is
//! never fetched from the network: `http(s)://` is refused. `data:` URIs and `#fragments` are
//! kept as they are.
//!
//! - html with `embed_assets`: stylesheets are inlined and every file becomes a data URI, so the
//!   file works wherever it's delivered ([`embed`]).
//! - pdf: stylesheets are inlined and every file is handed to the PDF engine by name ([`for_pdf`]).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::{Captures, Regex};

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<link\b[^>]*>").unwrap());
static ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\b(rel|href)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap());
static IMG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)(<img\b[^>]*?\bsrc\s*=\s*)(?:"([^"]*)"|'([^']*)')"#).unwrap());
static STYLE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)(<style\b[^>]*>)(.*?)(</style>)").unwrap());
static STYLE_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)(\bstyle\s*=\s*)(?:"([^"]*)"|'([^']*)')"#).unwrap());
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)\s"']*))\s*\)"#).unwrap());
static IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)@import\b"#).unwrap());

/// Files by the name they're handed to the PDF engine under, with their bytes.
pub type Files = Vec<(String, Vec<u8>)>;

/// The prefix of a file's name as handed to the PDF engine.
pub const PDF_KEY: &str = "dre-asset/";

/// What to do with each file found.
enum Mode<'a> {
    /// Replace it with a data URI.
    Embed,
    /// Only check it exists.
    Check,
    /// Name it `dre-asset/<path from the root>` and collect its bytes.
    Pdf(&'a mut Files),
}

/// Where `reference`, written in a file in `base`, points: `None` for a reference left as it is
/// (`data:`, `#fragment`, empty). An error is a sentence naming the reference.
pub fn resolve(reference: &str, root: &Path, base: &Path) -> Result<Option<PathBuf>, String> {
    let r = reference.trim();
    if r.is_empty()
        || r.starts_with('#')
        || r.to_ascii_lowercase().starts_with("data:")
        || r.starts_with(PDF_KEY)
    {
        return Ok(None);
    }
    let lower = r.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || r.starts_with("//")
        || lower.contains("://")
    {
        return Err(format!(
            "`{r}` is a URL; documents load files from the project only (put the file in the project and link it by a relative path)"
        ));
    }
    let path_part = r.split(['?', '#']).next().unwrap_or(r);
    let path = if Path::new(path_part).is_absolute() {
        PathBuf::from(path_part)
    } else {
        base.join(path_part)
    };
    if !path.exists() {
        return Err(format!("`{r}` doesn't exist (looked for {})", shown(root, &path)));
    }
    if !crate::template::inside(root, &path) {
        return Err(format!(
            "`{r}` is outside the project; documents load files from the project only"
        ));
    }
    Ok(Some(path))
}

fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|x| x.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

/// The html with stylesheets inlined and every file as a data URI.
pub fn embed(html: &str, root: &Path, base: &Path) -> Result<String, String> {
    process(html, root, base, &mut Mode::Embed)
}

/// Check every file the html refers to, leaving it unchanged.
pub fn check(html: &str, root: &Path, base: &Path) -> Result<(), String> {
    process(html, root, base, &mut Mode::Check).map(|_| ())
}

/// The html with stylesheets inlined and files named `dre-asset/<path>`, and each file's bytes.
pub fn for_pdf(html: &str, root: &Path, base: &Path) -> Result<(String, Files), String> {
    let mut files = Vec::new();
    let html = process(html, root, base, &mut Mode::Pdf(&mut files))?;
    Ok((html, files))
}

fn process(html: &str, root: &Path, base: &Path, mode: &mut Mode<'_>) -> Result<String, String> {
    let mut err: Option<String> = None;
    // Linked stylesheets first, inlined with their url()s resolved from their own folder.
    let html = LINK.replace_all(html, |c: &Captures| {
        let tag = &c[0];
        let mut rel = None;
        let mut href = None;
        for a in ATTR.captures_iter(tag) {
            let v = a.get(2).or(a.get(3)).or(a.get(4)).map(|m| m.as_str().to_string());
            match a[1].to_ascii_lowercase().as_str() {
                "rel" => rel = v,
                _ => href = v,
            }
        }
        let is_css = rel.is_some_and(|r| r.split_whitespace().any(|x| x.eq_ignore_ascii_case("stylesheet")));
        let (true, Some(href)) = (is_css, href) else {
            return tag.to_string();
        };
        match resolve(&href, root, base) {
            Ok(Some(path)) => match std::fs::read_to_string(&path) {
                Ok(css) => {
                    if IMPORT.is_match(&css) {
                        err.get_or_insert(format!(
                            "`{href}`: `@import` isn't supported; link each stylesheet from the template"
                        ));
                    }
                    let dir = path.parent().unwrap_or(root).to_path_buf();
                    match css_urls(&css, root, &dir, mode) {
                        Ok(css) => format!("<style>\n{css}\n</style>"),
                        Err(e) => {
                            err.get_or_insert(e);
                            String::new()
                        }
                    }
                }
                Err(e) => {
                    err.get_or_insert(format!("can't read `{href}`: {e}"));
                    String::new()
                }
            },
            Ok(None) => tag.to_string(),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    let html = STYLE_BLOCK.replace_all(&html, |c: &Captures| match css_urls(&c[2], root, base, mode) {
        Ok(css) => format!("{}{css}{}", &c[1], &c[3]),
        Err(e) => {
            err.get_or_insert(e);
            String::new()
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    let html = STYLE_ATTR.replace_all(&html, |c: &Captures| {
        let (q, v) = match c.get(2) {
            Some(v) => ('"', v.as_str()),
            None => ('\'', c.get(3).map_or("", |m| m.as_str())),
        };
        match css_urls(v, root, base, mode) {
            Ok(css) => format!("{}{q}{css}{q}", &c[1]),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    let html = IMG.replace_all(&html, |c: &Captures| {
        let (q, src) = match c.get(2) {
            Some(v) => ('"', v.as_str()),
            None => ('\'', c.get(3).map_or("", |m| m.as_str())),
        };
        match file_ref(src, root, base, mode) {
            Ok(v) => format!("{}{q}{v}{q}", &c[1]),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(html.into_owned()),
    }
}

/// CSS with each `url()` handled by `mode`.
fn css_urls(css: &str, root: &Path, base: &Path, mode: &mut Mode<'_>) -> Result<String, String> {
    let mut err = None;
    let out = URL.replace_all(css, |c: &Captures| {
        let r = c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str());
        match file_ref(r, root, base, mode) {
            Ok(v) => format!("url(\"{v}\")"),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out.into_owned()),
    }
}

/// What a reference becomes.
fn file_ref(r: &str, root: &Path, base: &Path, mode: &mut Mode<'_>) -> Result<String, String> {
    let Some(path) = resolve(r, root, base)? else {
        return Ok(r.to_string());
    };
    match mode {
        Mode::Check => Ok(r.to_string()),
        Mode::Embed => {
            let bytes = std::fs::read(&path).map_err(|e| format!("can't read `{r}`: {e}"))?;
            Ok(format!("data:{};base64,{}", mime(&path), base64(&bytes)))
        }
        Mode::Pdf(files) => {
            let key = format!("{PDF_KEY}{}", shown(root, &path));
            if !files.iter().any(|(k, _)| *k == key) {
                let bytes = std::fs::read(&path).map_err(|e| format!("can't read `{r}`: {e}"))?;
                files.push((key.clone(), bytes));
            }
            Ok(key)
        }
    }
}

/// A template's source with each literal relative file reference (no Jinja, no URL) rewritten
/// from `from` (the template's folder) to `to` (the main template's), when the file exists.
pub fn rebase(src: &str, from: &Path, to: &Path) -> String {
    if from == to {
        return src.to_string();
    }
    let fix = |r: &str| -> Option<String> {
        let t = r.trim();
        let lower = t.to_ascii_lowercase();
        if t.is_empty()
            || t.contains("{{")
            || t.contains("{%")
            || t.starts_with('#')
            || t.starts_with('/')
            || lower.starts_with("data:")
            || lower.contains("://")
        {
            return None;
        }
        let target = from.join(t.split(['?', '#']).next().unwrap_or(t));
        target.exists().then(|| relative(&target, to))
    };
    let src = IMG.replace_all(src, |c: &Captures| {
        let (q, v) = match c.get(2) {
            Some(v) => ('"', v.as_str()),
            None => ('\'', c.get(3).map_or("", |m| m.as_str())),
        };
        format!("{}{q}{}{q}", &c[1], fix(v).unwrap_or_else(|| v.to_string()))
    });
    let src = LINK.replace_all(&src, |c: &Captures| {
        ATTR.replace_all(&c[0], |a: &Captures| {
            if !a[1].eq_ignore_ascii_case("href") {
                return a[0].to_string();
            }
            let v = a.get(2).or(a.get(3)).or(a.get(4)).map_or("", |m| m.as_str());
            match fix(v) {
                Some(n) => format!("href=\"{n}\""),
                None => a[0].to_string(),
            }
        })
        .into_owned()
    });
    URL.replace_all(&src, |c: &Captures| {
        let v = c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str());
        match fix(v) {
            Some(n) => format!("url(\"{n}\")"),
            None => c[0].to_string(),
        }
    })
    .into_owned()
}

/// `target` as a path relative to the folder `from` (both absolute), with `/`.
fn relative(target: &Path, from: &Path) -> String {
    let t: Vec<_> = target.components().collect();
    let f: Vec<_> = from.components().collect();
    let common = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), f.len() - common).collect();
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// `validate`: every literal file reference in a template's source (one without Jinja).
pub fn check_source(src: &str, root: &Path, dir: &Path, shown_name: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for c in IMG.captures_iter(src) {
        refs.extend(c.get(2).or(c.get(3)).map(|m| m.as_str().to_string()));
    }
    for tag in LINK.find_iter(src) {
        let attrs: Vec<(String, String)> = ATTR
            .captures_iter(tag.as_str())
            .map(|a| {
                let v = a.get(2).or(a.get(3)).or(a.get(4)).map_or("", |m| m.as_str());
                (a[1].to_ascii_lowercase(), v.to_string())
            })
            .collect();
        if attrs
            .iter()
            .any(|(k, v)| k == "rel" && v.to_ascii_lowercase().contains("stylesheet"))
        {
            refs.extend(attrs.into_iter().filter(|(k, _)| k == "href").map(|(_, v)| v));
        }
    }
    for c in URL.captures_iter(src) {
        refs.extend(c.get(1).or(c.get(2)).or(c.get(3)).map(|m| m.as_str().to_string()));
    }
    refs.into_iter()
        .filter(|r| !r.contains("{{") && !r.contains("{%"))
        .filter_map(|r| resolve(&r, root, dir).err())
        .map(|e| format!("template `{shown_name}`: {e}"))
        .collect()
}

fn mime(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "css" => "text/css",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn embeds_and_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("reports/assets")).unwrap();
        std::fs::write(root.join("reports/assets/logo.png"), b"PNG").unwrap();
        std::fs::write(
            root.join("reports/assets/s.css"),
            "body { background: url(logo.png) }",
        )
        .unwrap();
        let base = root.join("reports");
        let html = r#"<link rel="stylesheet" href="assets/s.css"><img src="assets/logo.png"><p style="background:url('assets/logo.png')">x</p><img src="data:image/png;base64,AA">"#;
        let out = embed(html, root, &base).unwrap();
        assert!(!out.contains("<link"), "{out}");
        assert_eq!(out.matches("data:image/png;base64,UE5H").count(), 3, "{out}");
        assert!(out.contains("data:image/png;base64,AA"));
        let e = embed(r#"<img src="https://x.test/a.png">"#, root, &base).unwrap_err();
        assert!(e.contains("is a URL"), "{e}");
        std::fs::write(dir.path().parent().unwrap().join("outside.png"), b"x").ok();
        let e = embed(r#"<img src="../../outside.png">"#, root, &base).unwrap_err();
        assert!(
            e.contains("outside the project") || e.contains("doesn't exist"),
            "{e}"
        );
        let e = embed(r#"<img src="missing.png">"#, root, &base).unwrap_err();
        assert!(e.contains("doesn't exist"), "{e}");
        let (pdf_html, files) = for_pdf(html, root, &base).unwrap();
        assert!(
            pdf_html.contains("dre-asset/reports/assets/logo.png"),
            "{pdf_html}"
        );
        assert_eq!(files.len(), 1);
    }
}
