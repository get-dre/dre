//! Fonts for the PDF. The plugin bundles IBM Plex Sans (regular, bold, italic, bold italic) and
//! IBM Plex Mono (OFL 1.1, `fonts/OFL.txt`); a project adds its own through `@font-face`. System
//! fonts are never used, so a PDF looks the same on every machine.
//!
//! The PDF engine knows a font by the family name inside the file and has no `@font-face`, so
//! this module reads each `@font-face`, registers its file, and rewrites the `font-family` lists
//! that use the declared name. Every list gets the bundled family followed by other declared
//! project families as fallbacks; a family that's neither bundled nor declared is warned about
//! by name. Downloaded Noto follows those project and bundled families.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::{Captures, Regex};

pub const SANS: &str = "IBM Plex Sans";
pub const MONO: &str = "IBM Plex Mono";

/// The bundled files.
pub const BUNDLED: &[&[u8]] = &[
    include_bytes!("../fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../fonts/IBMPlexSans-BoldItalic.ttf"),
    include_bytes!("../fonts/IBMPlexMono-Regular.ttf"),
];

const GENERIC: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-sans-serif",
    "ui-serif",
    "ui-monospace",
    "inherit",
    "initial",
    "unset",
];

static STYLE_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)(<style\b[^>]*>)(.*?)(</style>)").expect("the pattern is a valid regex")
});
static STYLE_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)(\bstyle\s*=\s*)(?:"([^"]*)"|'([^']*)')"#).expect("the pattern is a valid regex")
});
static FONT_FACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)@font-face\s*\{([^}]*)\}").expect("the pattern is a valid regex"));
static FAMILY_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(font-family\s*:\s*)([^;}]+)").expect("the pattern is a valid regex"));
static SRC_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)\s"']*))\s*\)"#)
        .expect("the pattern is a valid regex")
});
static HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<head\b[^>]*>").expect("the pattern is a valid regex"));

/// One font file to register.
pub struct Font {
    pub bytes: Vec<u8>,
}

/// The html ready for the engine, and the project's fonts to register (the bundled ones are
/// registered by the caller). `files` holds every `dre-asset/` file's bytes ([`crate::assets`]).
pub fn prepare(
    html: &str,
    files: &[(String, Vec<u8>)],
    warnings: &mut Vec<String>,
) -> Result<(String, Vec<Font>), String> {
    let mut fonts = Vec::new();
    // Declared name (lower case) -> the family inside its file.
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut err = None;
    let html = STYLE_BLOCK.replace_all(html, |c: &Captures| {
        let css = FONT_FACE.replace_all(&c[2], |f: &Captures| {
            match font_face(&f[1], files) {
                Ok((alias, family, bytes)) => {
                    aliases.insert(alias.to_lowercase(), family);
                    fonts.push(Font { bytes });
                }
                Err(e) => {
                    err.get_or_insert(e);
                }
            }
            String::new()
        });
        format!("{}{css}{}", &c[1], &c[3])
    });
    if let Some(e) = err {
        return Err(e);
    }
    let mut known: BTreeSet<String> = [SANS, MONO].iter().map(|s| s.to_lowercase()).collect();
    known.extend(aliases.values().map(|f| f.to_lowercase()));
    let mut unknown = BTreeSet::new();
    let mut fix = |css: &str| -> String {
        FAMILY_DECL
            .replace_all(css, |d: &Captures| {
                format!("{}{}", &d[1], family_list(&d[2], &aliases, &known, &mut unknown))
            })
            .into_owned()
    };
    let html = STYLE_BLOCK.replace_all(&html, |c: &Captures| format!("{}{}{}", &c[1], fix(&c[2]), &c[3]));
    let html = STYLE_ATTR.replace_all(&html, |c: &Captures| match c.get(2) {
        Some(v) => format!("{}\"{}\"", &c[1], fix(v.as_str()).replace('"', "'")),
        None => format!(
            "{}'{}'",
            &c[1],
            fix(c.get(3).map_or("", |m| m.as_str())).replace('\'', "\"")
        ),
    });
    for f in unknown {
        warnings.push(format!(
            "font `{f}` is neither bundled nor declared with `@font-face`, so {SANS} is used instead; add the font file to the project and declare it"
        ));
    }
    // The default for text with no `font-family` at all.
    let base = format!("<style>html {{ font-family: \"{SANS}\"; }}</style>");
    let mut html = match HEAD.find(&html) {
        Some(m) => format!("{}{base}{}", &html[..m.end()], &html[m.end()..]),
        None => format!("{base}{html}"),
    };
    // Coverage checks count every declared project face. Make those faces available as actual
    // fallbacks too, including outside elements that explicitly select their declared alias.
    let families: BTreeSet<&String> = aliases.values().collect();
    for family in families {
        if ![SANS, MONO]
            .iter()
            .any(|bundled| family.eq_ignore_ascii_case(bundled))
        {
            html = append_fallback(&html, family);
        }
    }
    Ok((html, fonts))
}

/// One `@font-face` block: its declared family, the family inside its file, and the file.
fn font_face(body: &str, files: &[(String, Vec<u8>)]) -> Result<(String, String, Vec<u8>), String> {
    let decl = |name: &str| {
        body.split(';').find_map(|d| {
            let (k, v) = d.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let alias = decl("font-family")
        .map(|f| f.trim_matches(['"', '\'']).trim().to_string())
        .ok_or("an `@font-face` has no `font-family`")?;
    let src = decl("src").ok_or_else(|| format!("`@font-face` `{alias}` has no `src`"))?;
    // The first url() of a font file in the project (the asset pass named it `dre-asset/...`).
    let mut tried = Vec::new();
    for c in SRC_URL.captures_iter(&src) {
        let r = c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str());
        tried.push(r.to_string());
        if let Some((_, bytes)) = files.iter().find(|(k, _)| k == r) {
            let lower = r.to_ascii_lowercase();
            if lower.ends_with(".woff") || lower.ends_with(".woff2") {
                return Err(format!(
                    "`@font-face` `{alias}`: `{}` is a web font; give a `.ttf` or `.otf` file for PDFs",
                    r.trim_start_matches(crate::assets::PDF_KEY)
                ));
            }
            let family = family_name(bytes).ok_or_else(|| {
                format!(
                    "`@font-face` `{alias}`: `{}` isn't a font file DRE can read (use .ttf or .otf)",
                    r.trim_start_matches(crate::assets::PDF_KEY)
                )
            })?;
            return Ok((alias, family, bytes.clone()));
        }
    }
    Err(format!(
        "`@font-face` `{alias}` names no font file in the project (src: {src})"
    ))
}

/// The family name inside a TrueType or OpenType file: its typographic family if set, else its
/// family.
pub fn family_name(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let get = |id: u16| {
        face.names()
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .find_map(|n| n.to_string())
    };
    get(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| get(ttf_parser::name_id::FAMILY))
}

/// A `font-family` value with declared names replaced by their file's family and the bundled
/// family appended. Unknown names are collected for a warning.
fn family_list(
    value: &str,
    aliases: &BTreeMap<String, String>,
    known: &BTreeSet<String>,
    unknown: &mut BTreeSet<String>,
) -> String {
    let important = value.to_ascii_lowercase().contains("!important");
    let value = value.replace("!important", "").replace("!IMPORTANT", "");
    let mut names = Vec::new();
    let mut mono = false;
    for raw in value.split(',') {
        let n = raw.trim().trim_matches(['"', '\'']).trim();
        if n.is_empty() {
            continue;
        }
        let lower = n.to_lowercase();
        if GENERIC.contains(&lower.as_str()) {
            mono |= lower.contains("monospace");
            continue;
        }
        match aliases.get(&lower) {
            Some(f) => names.push(format!("\"{f}\"")),
            None => {
                if !known.contains(&lower) {
                    unknown.insert(n.to_string());
                }
                names.push(format!("\"{n}\""));
            }
        }
    }
    let fallback = if mono { MONO } else { SANS };
    if !names
        .iter()
        .any(|n| n.trim_matches('"').eq_ignore_ascii_case(fallback))
    {
        names.push(format!("\"{fallback}\""));
    }
    let mut out = names.join(", ");
    if important {
        out.push_str(" !important");
    }
    out
}

/// Japanese characters absent from every bundled and project font need Noto.
pub fn needs_japanese(html: &str, project: &[Font]) -> bool {
    static NON_TEXT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<!--.*?-->|<style\b[^>]*>.*?</style>|<script\b[^>]*>.*?</script>|<[^>]*>")
            .expect("the pattern is a valid regex")
    });
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&#([xX][0-9a-fA-F]+|[0-9]+);").expect("the pattern is a valid regex"));
    let mut text = NON_TEXT.replace_all(html, "").into_owned();
    // Generated content participates in shaping even though it lives in CSS, not text nodes.
    static CONTENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)\bcontent\s*:\s*((?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^;}'"])+)"#)
            .expect("the pattern is a valid regex")
    });
    static CSS_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\([0-9a-fA-F]{1,6})(?:\r\n|[ \t\r\n\x0c])?").expect("the pattern is a valid regex")
    });
    let mut add_content = |css: &str| {
        for content in CONTENT.captures_iter(css) {
            let decoded = CSS_ESCAPE.replace_all(&content[1], |c: &Captures| {
                u32::from_str_radix(&c[1], 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            });
            text.push_str(&decoded);
        }
    };
    for style in STYLE_BLOCK.captures_iter(html) {
        add_content(&style[2]);
    }
    for style in STYLE_ATTR.captures_iter(html) {
        add_content(style.get(2).or(style.get(3)).map_or("", |m| m.as_str()));
    }
    let text = ENTITY.replace_all(&text, |c: &Captures| {
        let number = c[1]
            .strip_prefix(['x', 'X'])
            .map(|n| u32::from_str_radix(n, 16))
            .unwrap_or_else(|| c[1].parse());
        number
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_default()
    });
    let faces: Vec<_> = BUNDLED
        .iter()
        .copied()
        .chain(project.iter().map(|f| f.bytes.as_slice()))
        .filter_map(|bytes| ttf_parser::Face::parse(bytes, 0).ok())
        .collect();
    text.chars().any(|c| matches!(c as u32, 0x3000..=0x30ff | 0x31f0..=0x31ff | 0x3400..=0x9fff | 0xff00..=0xffef | 0x20000..=0x323af)
        && !faces.iter().any(|f| f.glyph_index(c).is_some_and(|id| id.0 != 0)))
}

/// Append a fallback family after explicitly selected families and Plex.
pub fn append_fallback(html: &str, family: &str) -> String {
    let fix = |css: &str| {
        FAMILY_DECL
            .replace_all(css, |c: &Captures| {
                let value = c[2].trim();
                let (value, important) = if let Some(i) = value.to_ascii_lowercase().find("!important") {
                    (value[..i].trim(), " !important")
                } else {
                    (value, "")
                };
                if value.split(',').any(|name| {
                    name.trim()
                        .trim_matches(['\"', '\''])
                        .eq_ignore_ascii_case(family)
                }) {
                    c[0].to_string()
                } else {
                    format!("{}{value}, \"{family}\"{important}", &c[1])
                }
            })
            .into_owned()
    };
    let html = STYLE_BLOCK.replace_all(html, |c: &Captures| format!("{}{}{}", &c[1], fix(&c[2]), &c[3]));
    STYLE_ATTR
        .replace_all(&html, |c: &Captures| match c.get(2) {
            Some(v) => format!("{}\"{}\"", &c[1], fix(v.as_str()).replace('"', "'")),
            None => format!(
                "{}'{}'",
                &c[1],
                fix(c.get(3).map_or("", |m| m.as_str())).replace('\'', "\"")
            ),
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_families() {
        assert_eq!(family_name(BUNDLED[0]).as_deref(), Some(SANS));
        assert_eq!(family_name(BUNDLED[4]).as_deref(), Some(MONO));
    }

    #[test]
    fn rewrites_declared_and_warns_unknown() {
        let files = vec![("dre-asset/fonts/brand.ttf".to_string(), BUNDLED[1].to_vec())];
        let html = r#"<html><head><style>@font-face { font-family: "Brand"; src: url("dre-asset/fonts/brand.ttf"); font-weight: bold; }
h1 { font-family: Brand, serif; } code { font-family: monospace; } p { font-family: "Comic Nonexistent"; }</style></head><body style="font-family: Brand">x</body></html>"#;
        let mut w = Vec::new();
        let (out, fonts) = prepare(html, &files, &mut w).unwrap();
        assert_eq!(fonts.len(), 1);
        assert!(!out.contains("@font-face"), "{out}");
        assert!(out.contains(r#"h1 { font-family: "IBM Plex Sans"; }"#), "{out}");
        assert!(out.contains(r#"code { font-family: "IBM Plex Mono"; }"#), "{out}");
        assert!(
            out.contains(r#"p { font-family: "Comic Nonexistent", "IBM Plex Sans"; }"#),
            "{out}"
        );
        assert!(out.contains(r#"style="font-family: 'IBM Plex Sans'""#), "{out}");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("Comic Nonexistent"));
    }
}
