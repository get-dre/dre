//! Noto fallback fonts for PDF text that the bundled and project fonts can't draw.
//!
//! [`plan`] reads the rendered HTML and returns the pinned Noto packages its uncovered characters
//! need, one per script. Han characters are shared by Chinese, Japanese and Korean and drawn
//! differently by each, so their font follows the nearest `lang` attribute, as in a browser.
//! Text with no CJK `lang` is guessed from the whole document: kana means Japanese, Hangul means
//! Korean, Han alone means Simplified Chinese, with a note suggesting `lang`.
//!
//! [`apply`] then orders the fetched families in the CSS: every `font-family` ends with
//! `var(--dre-fallback)`, and a rule per `lang` value puts that language's Han font first.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::fonts::Font;

/// One pinned Noto package in DRE's registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Noto {
    Japanese,
    SimplifiedChinese,
    TraditionalChinese,
    Korean,
    Arabic,
    Hebrew,
    Devanagari,
    Thai,
}

impl Noto {
    /// The registry package name and exact version.
    pub fn package(self) -> (&'static str, &'static str) {
        match self {
            Noto::Japanese => ("noto_sans_jp", "2.4.0"),
            Noto::SimplifiedChinese => ("noto_sans_sc", "2.4.0"),
            Noto::TraditionalChinese => ("noto_sans_tc", "2.4.0"),
            Noto::Korean => ("noto_sans_kr", "2.4.0"),
            Noto::Arabic => ("noto_sans_arabic", "2.13.0"),
            Noto::Hebrew => ("noto_sans_hebrew", "3.1.0"),
            Noto::Devanagari => ("noto_sans_devanagari", "2.7.0"),
            Noto::Thai => ("noto_sans_thai", "2.2.0"),
        }
    }

    /// The script's name, for warnings.
    pub fn label(self) -> &'static str {
        match self {
            Noto::Japanese => "Japanese",
            Noto::SimplifiedChinese => "Simplified Chinese",
            Noto::TraditionalChinese => "Traditional Chinese",
            Noto::Korean => "Korean",
            Noto::Arabic => "Arabic",
            Noto::Hebrew => "Hebrew",
            Noto::Devanagari => "Devanagari",
            Noto::Thai => "Thai",
        }
    }
}

/// The Noto packages a PDF needs, and how its Han text picks among them.
#[derive(Debug, Default)]
pub struct Plan {
    /// The packages to fetch, each once.
    pub fonts: BTreeSet<Noto>,
    /// A one-line note when Han text had no `lang` and was guessed to be Simplified Chinese.
    pub note: Option<String>,
    /// The Han font for text whose nearest `lang` names no CJK language.
    guess: Option<Noto>,
    /// Every well-formed `lang` value in the document, with its Han font if it names one.
    langs: BTreeMap<String, Option<Noto>>,
}

/// What the HTML's uncovered characters need. `project` holds the `@font-face` fonts; the
/// bundled Plex fonts are always checked.
pub fn plan(html: &str, project: &[Font]) -> Plan {
    let faces: Vec<_> = crate::fonts::BUNDLED
        .iter()
        .copied()
        .chain(project.iter().map(|f| f.bytes.as_slice()))
        .filter_map(|bytes| ttf_parser::Face::parse(bytes, 0).ok())
        .collect();
    let mut covered: HashMap<char, bool> = HashMap::new();
    let mut plan = Plan::default();
    // Han text per CJK language, and what text with no CJK language contains, for the guess.
    let mut han_langs: BTreeSet<Noto> = BTreeSet::new();
    let (mut kana, mut hangul, mut han, mut cjk) = (false, false, false, false);
    for (text, lang) in text_runs(html) {
        let han_font = lang.and_then(han_font);
        for c in text.chars() {
            let Some(script) = script(c) else { continue };
            if *covered
                .entry(c)
                .or_insert_with(|| faces.iter().any(|f| f.glyph_index(c).is_some_and(|id| id.0 != 0)))
            {
                continue;
            }
            match (script, han_font) {
                (Script::Other(noto), _) => {
                    plan.fonts.insert(noto);
                }
                // Each Noto CJK font is complete: kana, Hangul and every Han form.
                (_, Some(noto)) => {
                    han_langs.insert(noto);
                }
                (Script::Kana, None) => kana = true,
                (Script::Hangul, None) => hangul = true,
                (Script::Han, None) => han = true,
                (Script::Cjk, None) => cjk = true,
            }
        }
        if let Some(lang) = lang {
            plan.langs.entry(lang.to_string()).or_insert(han_font);
        }
    }
    if kana || hangul || han || cjk {
        let guess = if kana {
            Noto::Japanese
        } else if hangul {
            Noto::Korean
        } else {
            Noto::SimplifiedChinese
        };
        if guess == Noto::SimplifiedChinese && han {
            plan.note = Some(
                "PDF Han text without a `lang` uses Simplified Chinese glyphs; set `lang` on its element or `<html>` (`ja`, `ko`, `zh-Hant`) to choose"
                    .into(),
            );
        }
        plan.guess = Some(guess);
        han_langs.insert(guess);
    }
    plan.fonts.extend(han_langs);
    plan
}

/// Make the fetched `families` (each package's family name) the last fallbacks of every
/// `font-family`, with the Han font of each element's nearest `lang` first among them.
///
/// Every declaration `font-family: L` becomes `--dre-font: L; font-family: L,
/// var(--dre-fallback)`, and a rule per `lang` value sets `--dre-fallback`. Custom properties
/// inherit, so a descendant's own `font-family` gets its ancestor's order. An element with a
/// `lang` and no `font-family` of its own recomputes its inherited list from `--dre-font`; that
/// rule has no specificity, so any of the template's own `font-family` rules win over it.
pub fn apply(html: &str, plan: &Plan, families: &BTreeMap<Noto, String>) -> String {
    if families.is_empty() {
        return html.to_string();
    }
    let order = |first: Option<Noto>| {
        let first = first.filter(|noto| families.contains_key(noto));
        first
            .into_iter()
            .chain(families.keys().copied().filter(|noto| Some(*noto) != first))
            .map(|noto| format!("\"{}\"", families[&noto].replace(['"', '\\'], "")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let html = crate::fonts::map_families(html, |value, important| {
        Some(format!(
            "--dre-font: {value}{important}; font-family: {value}, var(--dre-fallback){important}"
        ))
    });
    let mut css = format!(
        "html {{ --dre-fallback: {}; }} :where([lang]) {{ font-family: var(--dre-font), var(--dre-fallback); }}",
        order(plan.guess)
    );
    for (lang, han) in &plan.langs {
        css.push_str(&format!(
            " [lang=\"{lang}\"] {{ --dre-fallback: {}; }}",
            order(han.or(plan.guess))
        ));
    }
    crate::fonts::insert_style(&html, &css)
}

/// What a character needs from a fallback font.
#[derive(Clone, Copy)]
enum Script {
    /// Hiragana and katakana.
    Kana,
    Hangul,
    /// Han ideographs.
    Han,
    /// Other CJK-only characters (punctuation, full-width forms, bopomofo), drawn by any Han font.
    Cjk,
    /// A script with one font.
    Other(Noto),
}

fn script(c: char) -> Option<Script> {
    Some(match c as u32 {
        0x0590..=0x05ff | 0xfb1d..=0xfb4f => Script::Other(Noto::Hebrew),
        0x0600..=0x06ff | 0x0750..=0x077f | 0x0870..=0x08ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff => {
            Script::Other(Noto::Arabic)
        }
        0x0900..=0x097f | 0xa8e0..=0xa8ff => Script::Other(Noto::Devanagari),
        0x0e00..=0x0e7f => Script::Other(Noto::Thai),
        0x3040..=0x30ff | 0x31f0..=0x31ff | 0xff66..=0xff9f => Script::Kana,
        0x1100..=0x11ff | 0x3130..=0x318f | 0xa960..=0xa97f | 0xac00..=0xd7ff | 0xffa0..=0xffdc => {
            Script::Hangul
        }
        0x2e80..=0x2fdf
        | 0x3005..=0x3007
        | 0x3021..=0x3029
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xf900..=0xfaff
        | 0x20000..=0x323af => Script::Han,
        0x3000..=0x303f
        | 0x3100..=0x312f
        | 0x31a0..=0x31bf
        | 0x3190..=0x319f
        | 0x31c0..=0x31ef
        | 0x3200..=0x33ff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xffef => Script::Cjk,
        _ => return None,
    })
}

/// The Han font a `lang` value names, if it's a CJK language.
fn han_font(lang: &str) -> Option<Noto> {
    let lang = lang.to_ascii_lowercase();
    let mut tags = lang.split(['-', '_']);
    match tags.next()? {
        "ja" => Some(Noto::Japanese),
        "ko" => Some(Noto::Korean),
        "zh" => Some(if tags.any(|t| matches!(t, "hant" | "tw" | "hk" | "mo")) {
            Noto::TraditionalChinese
        } else {
            Noto::SimplifiedChinese
        }),
        _ => None,
    }
}

static LANG_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)(?:^|\s)lang\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#)
        .expect("the pattern is a valid regex")
});

/// A `lang` value usable in a CSS selector: a language tag's letters, digits, `-` and `_`. Other
/// values are ignored, by both the scan and the CSS, so the two always agree.
fn lang_of(attrs: &str) -> Option<&str> {
    let c = LANG_ATTR.captures(attrs)?;
    let value = c.get(1).or(c.get(2)).or(c.get(3))?.as_str();
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        .then_some(value)
}

/// The document's visible text and CSS generated content, each run with its nearest `lang`.
fn text_runs(html: &str) -> Vec<(String, Option<&str>)> {
    fn current<'a>(open: &[(String, Option<&'a str>)]) -> Option<&'a str> {
        open.iter().rev().find_map(|(_, lang)| *lang)
    }
    // Plain ASCII without character references can't need a fallback: skip it, as most of a
    // large table is.
    fn push<'a>(runs: &mut Vec<(String, Option<&'a str>)>, text: &str, lang: Option<&'a str>) {
        if !text.is_ascii() || text.contains("&#") {
            runs.push((decode_entities(text), lang));
        }
    }
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)<!--.*?-->|<style\b[^>]*>(.*?)</style>|<script\b[^>]*>.*?</script>|<title\b[^>]*>.*?</title>|<(/?)([a-z][^\s/>]*)([^>]*)>",
        )
        .expect("the pattern is a valid regex")
    });
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr",
    ];
    let mut runs = Vec::new();
    // Open elements: name and `lang` (an empty `lang` means an unknown language).
    let mut open: Vec<(String, Option<&str>)> = Vec::new();
    let mut root_lang = None;
    let mut generated = String::new();
    let mut at = 0;
    for t in TOKEN.captures_iter(html) {
        let m = t.get(0).expect("a match has group 0");
        push(&mut runs, &html[at..m.start()], current(&open));
        at = m.end();
        if let Some(css) = t.get(1) {
            generated.push_str(&generated_content(css.as_str()));
            continue;
        }
        let Some(name) = t.get(3) else { continue };
        let name = name.as_str().to_ascii_lowercase();
        let attrs = t.get(4).map_or("", |a| a.as_str());
        if let Some(style) = crate::fonts::STYLE_ATTR.captures(attrs) {
            generated.push_str(&generated_content(
                style.get(2).or(style.get(3)).map_or("", |m| m.as_str()),
            ));
        }
        if !t[2].is_empty() {
            if let Some(i) = open.iter().rposition(|(n, _)| *n == name) {
                open.truncate(i);
            }
            continue;
        }
        let lang = lang_of(attrs);
        if name == "html" {
            root_lang = lang;
        }
        if !VOID.contains(&name.as_str()) && !attrs.trim_end().ends_with('/') {
            open.push((name, lang));
        }
    }
    push(&mut runs, &html[at..], current(&open));
    runs.push((generated, root_lang));
    runs
}

/// The text of CSS `content:` values, CSS escapes decoded: it's laid out like document text.
fn generated_content(css: &str) -> String {
    static CONTENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)\bcontent\s*:\s*((?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^;}'"])+)"#)
            .expect("the pattern is a valid regex")
    });
    static CSS_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\([0-9a-fA-F]{1,6})(?:\r\n|[ \t\r\n\x0c])?").expect("the pattern is a valid regex")
    });
    let mut out = String::new();
    for content in CONTENT.captures_iter(css) {
        out.push_str(&CSS_ESCAPE.replace_all(&content[1], |c: &Captures| {
            u32::from_str_radix(&c[1], 16)
                .ok()
                .and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_default()
        }));
    }
    out
}

/// Numeric character references decoded; named ones never stand for the scripts checked here.
fn decode_entities(text: &str) -> String {
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&#([xX][0-9a-fA-F]+|[0-9]+);").expect("the pattern is a valid regex"));
    ENTITY
        .replace_all(text, |c: &Captures| {
            c[1].strip_prefix(['x', 'X'])
                .map(|n| u32::from_str_radix(n, 16))
                .unwrap_or_else(|| c[1].parse())
                .ok()
                .and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_default()
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts(html: &str) -> Vec<Noto> {
        plan(html, &[]).fonts.into_iter().collect()
    }

    #[test]
    fn a_chinese_lang_picks_simplified_chinese() {
        assert_eq!(
            fonts(r#"<html lang="zh"><body>北京</body></html>"#),
            [Noto::SimplifiedChinese]
        );
    }

    #[test]
    fn han_follows_the_nearest_lang_per_element() {
        let html = r#"<html lang="ja"><body><table><tr><td>東京</td><td lang="zh-TW">臺北</td><td lang="zh-Hans">北京</td><td lang="ko-KR">서울 漢字</td></tr></table></body></html>"#;
        assert_eq!(
            fonts(html),
            [
                Noto::Japanese,
                Noto::SimplifiedChinese,
                Noto::TraditionalChinese,
                Noto::Korean
            ]
        );
        let p = plan(html, &[]);
        assert!(p.note.is_none(), "{:?}", p.note);
        // A closed element's lang no longer applies.
        assert_eq!(
            fonts(r#"<p><span lang="ko">한</span></p><p lang="zh">中</p>"#),
            [Noto::SimplifiedChinese, Noto::Korean]
        );
    }

    #[test]
    fn without_lang_kana_means_japanese_hangul_korean_and_han_alone_chinese_with_a_note() {
        let p = plan("<p>東京 です</p>", &[]);
        assert_eq!(p.fonts.iter().copied().collect::<Vec<_>>(), [Noto::Japanese]);
        assert!(p.note.is_none());
        let p = plan("<p>漢字 한국어</p>", &[]);
        assert_eq!(p.fonts.iter().copied().collect::<Vec<_>>(), [Noto::Korean]);
        assert!(p.note.is_none());
        let p = plan(r#"<html lang="en"><p>北京</p></html>"#, &[]);
        assert_eq!(
            p.fonts.iter().copied().collect::<Vec<_>>(),
            [Noto::SimplifiedChinese]
        );
        let note = p.note.expect("Han without lang suggests lang");
        assert!(note.contains("`lang`") && !note.contains('\n'), "{note}");
    }

    #[test]
    fn each_script_needs_its_own_font_and_latin_needs_none() {
        assert_eq!(
            fonts("<p>שלום مرحبا नमस्ते สวัสดี &#x05D0;</p>"),
            [Noto::Arabic, Noto::Hebrew, Noto::Devanagari, Noto::Thai]
        );
        assert!(
            fonts("<title>東京</title><p>Hello, Ελληνικά, Кириллица</p><!-- 東京 --><script>'東京'</script>")
                .is_empty()
        );
        assert!(plan("<p>Hello</p>", &[]).note.is_none());
    }

    #[test]
    fn generated_content_uses_the_documents_lang() {
        let html = r#"<html lang="ja"><head><style>p::before { content: '\6771 \4EAC'; }</style></head><body><p></p></body></html>"#;
        assert_eq!(fonts(html), [Noto::Japanese]);
    }
}
