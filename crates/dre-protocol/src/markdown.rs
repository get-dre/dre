//! The portable Markdown subset a message's text is written in, and its translations.
//!
//! Inline: `**bold**`, `*italic*` or `_italic_`, `` `code` ``, `[text](url)` and backslash
//! escapes (`\*`). Lines: a line starting with `- ` or `* ` is a bullet; other lines are text,
//! kept as written (line breaks matter in chat). There are no headings and no tables: the title
//! is separate.
//!
//! Core escapes every value a template interpolates ([`escape`]), so a value containing `*` or
//! `_` stays literal. Each destination translates with one of the `to_*` functions.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Code(String),
    Link { text: Vec<Inline>, url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Text(Vec<Inline>),
    Bullet(Vec<Inline>),
    Blank,
}

const ESCAPABLE: &[char] = &['\\', '*', '_', '`', '[', ']'];

/// `s` with every character that means something in the subset backslash-escaped.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if ESCAPABLE.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Parse `text` line by line.
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.is_empty() {
                Line::Blank
            } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                Line::Bullet(inlines(rest.trim_start()))
            } else {
                Line::Text(inlines(l.trim_end()))
            }
        })
        .collect()
}

fn inlines(s: &str) -> Vec<Inline> {
    let chars: Vec<char> = s.chars().collect();
    parse_span(&chars)
}

fn push_text(out: &mut Vec<Inline>, c: char) {
    if let Some(Inline::Text(t)) = out.last_mut() {
        t.push(c);
    } else {
        out.push(Inline::Text(c.to_string()));
    }
}

/// The index of the next unescaped `pat` at or after `from`.
fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    let mut i = from;
    while i + pat.len() <= chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i..i + pat.len()] == *pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn unescape(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            out.push(chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn parse_span(chars: &[char]) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut i = 0;
    let word = |j: usize| chars.get(j).is_some_and(|c| c.is_alphanumeric());
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            push_text(&mut out, chars[i + 1]);
            i += 2;
            continue;
        }
        // Values printed inside a code span were escaped too: unescape them.
        if c == '`'
            && let Some(end) = find(chars, i + 1, &['`'])
        {
            out.push(Inline::Code(unescape(&chars[i + 1..end])));
            i = end + 1;
            continue;
        }
        if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(end) = find(chars, i + 2, &['*', '*'])
            && end > i + 2
        {
            out.push(Inline::Bold(parse_span(&chars[i + 2..end])));
            i = end + 2;
            continue;
        }
        // `_` only opens and closes at word boundaries, so snake_case stays text.
        if (c == '*' || (c == '_' && !word(i.wrapping_sub(1))))
            && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
        {
            let mut from = i + 1;
            let close = loop {
                match find(chars, from, &[c]) {
                    Some(e) if c == '_' && word(e + 1) => from = e + 1,
                    Some(e) if c == '*' && chars.get(e + 1) == Some(&'*') => from = e + 2,
                    other => break other,
                }
            };
            if let Some(end) = close
                && end > i + 1
            {
                out.push(Inline::Italic(parse_span(&chars[i + 1..end])));
                i = end + 1;
                continue;
            }
        }
        if c == '['
            && let Some(mid) = find(chars, i + 1, &[']', '('])
            && let Some(end) = find(chars, mid + 2, &[')'])
        {
            out.push(Inline::Link {
                text: parse_span(&chars[i + 1..mid]),
                url: unescape(&chars[mid + 2..end]),
            });
            i = end + 1;
            continue;
        }
        push_text(&mut out, c);
        i += 1;
    }
    out
}

/// How one dialect writes each piece.
struct Dialect {
    text: fn(&str) -> String,
    bold: (&'static str, &'static str),
    italic: (&'static str, &'static str),
    code: fn(&str) -> String,
    link: fn(&str, &str) -> String,
    bullet: &'static str,
}

fn span(items: &[Inline], d: &Dialect, out: &mut String) {
    for it in items {
        match it {
            Inline::Text(t) => out.push_str(&(d.text)(t)),
            Inline::Bold(x) => {
                out.push_str(d.bold.0);
                span(x, d, out);
                out.push_str(d.bold.1);
            }
            Inline::Italic(x) => {
                out.push_str(d.italic.0);
                span(x, d, out);
                out.push_str(d.italic.1);
            }
            Inline::Code(c) => out.push_str(&(d.code)(c)),
            Inline::Link { text, url } => {
                let mut t = String::new();
                span(text, d, &mut t);
                out.push_str(&(d.link)(&t, url));
            }
        }
    }
}

fn render(text: &str, d: &Dialect) -> String {
    let lines: Vec<String> = parse(text)
        .iter()
        .map(|l| {
            let mut out = String::new();
            if let Line::Bullet(_) = l {
                out.push_str(d.bullet);
            }
            out.push_str(&render_line(l, d));
            out
        })
        .collect();
    lines.join("\n")
}

/// Slack's `mrkdwn`, which has no escapes: `&`, `<` and `>` become entities, and literal `*`, `~`
/// and `` ` `` become look-alikes so they can't start formatting.
pub fn to_slack(text: &str) -> String {
    render(text, &CHAT)
}

/// Google Chat's text formatting (the same marks as Slack's).
pub fn to_google_chat(text: &str) -> String {
    render(text, &CHAT)
}

/// Literal text where `*`, `_` (at a word's edge), `~` and `` ` `` would start formatting: each
/// becomes a look-alike, since chat services have no escapes.
fn defused(t: &str) -> String {
    let chars: Vec<char> = t.chars().collect();
    let word = |j: Option<usize>| j.and_then(|j| chars.get(j)).is_some_and(|c| c.is_alphanumeric());
    let mut out = String::with_capacity(t.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '*' => out.push('\u{2217}'),
            '~' => out.push('\u{223c}'),
            '`' => out.push('\u{02cb}'),
            '_' if !(word(i.checked_sub(1)) && word(Some(i + 1))) => out.push('\u{02cd}'),
            c => out.push(c),
        }
    }
    out
}

fn chat_text(t: &str) -> String {
    defused(&t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"))
}

const CHAT: Dialect = Dialect {
    text: chat_text,
    bold: ("*", "*"),
    italic: ("_", "_"),
    code: |c| format!("`{}`", c.replace('`', "\u{02cb}")),
    link: |t, u| format!("<{}|{t}>", u.replace('|', "%7C").replace('>', "%3E")),
    bullet: "• ",
};

/// The Markdown Microsoft Teams renders in an Adaptive Card text block.
pub fn to_teams(text: &str) -> String {
    render(
        text,
        &Dialect {
            // Teams' card Markdown has no reliable escapes either: look-alikes, as for chat.
            text: defused,
            bold: ("**", "**"),
            italic: ("_", "_"),
            code: |c| format!("`{}`", c.replace('`', "\u{02cb}")),
            link: |t, u| format!("[{t}]({})", u.replace(')', "%29")),
            bullet: "- ",
        },
    )
}

/// `t` safe as HTML text or an attribute value.
pub fn html_escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// An HTML fragment: lines become paragraphs (consecutive lines joined by `<br>`), bullets a
/// list.
pub fn to_html(text: &str) -> String {
    let d = Dialect {
        text: html_escape,
        bold: ("<strong>", "</strong>"),
        italic: ("<em>", "</em>"),
        code: |c| format!("<code>{}</code>", html_escape(c)),
        link: |t, u| format!("<a href=\"{}\">{t}</a>", html_escape(u)),
        bullet: "",
    };
    let parsed = parse(text);
    let mut out = String::new();
    let mut i = 0;
    while i < parsed.len() {
        match &parsed[i] {
            Line::Blank => i += 1,
            Line::Bullet(_) => {
                out.push_str("<ul>\n");
                while let Some(Line::Bullet(_)) = parsed.get(i) {
                    out.push_str(&format!("<li>{}</li>\n", render_line(&parsed[i], &d)));
                    i += 1;
                }
                out.push_str("</ul>\n");
            }
            Line::Text(_) => {
                let mut lines = Vec::new();
                while let Some(Line::Text(_)) = parsed.get(i) {
                    lines.push(render_line(&parsed[i], &d));
                    i += 1;
                }
                out.push_str(&format!("<p>{}</p>\n", lines.join("<br>\n")));
            }
        }
    }
    out
}

fn render_line(l: &Line, d: &Dialect) -> String {
    let mut out = String::new();
    if let Line::Text(x) | Line::Bullet(x) = l {
        span(x, d, &mut out);
    }
    out
}

/// Plain text: marks removed, links as `text (url)`, bullets as `- `.
pub fn to_plain(text: &str) -> String {
    render(
        text,
        &Dialect {
            text: |t| t.to_string(),
            bold: ("", ""),
            italic: ("", ""),
            code: |c| c.to_string(),
            link: |t, u| {
                if t == u {
                    u.to_string()
                } else {
                    format!("{t} ({u})")
                }
            },
            bullet: "- ",
        },
    )
}

/// `text` cut to at most `max` characters including `marker`, at a line break when one is close
/// enough; `None` when it already fits.
pub fn truncate(text: &str, max: usize, marker: &str) -> Option<String> {
    if text.chars().count() <= max {
        return None;
    }
    let room = max.saturating_sub(marker.chars().count());
    let cut: String = text.chars().take(room).collect();
    // Prefer the last line break in the second half of what's kept.
    let kept = match cut.rfind('\n') {
        Some(i) if i >= cut.len() / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    Some(format!("{}{marker}", kept.trim_end()))
}

/// `text` (portable Markdown) translated with `translate`, fitting in `max` characters: when the
/// translation is too long, whole lines are dropped from the end of the source before
/// translating, so a cut never lands inside the dialect's markup, and `marker` (already in the
/// dialect) is appended. The second value says whether it was cut.
pub fn fit(text: &str, max: usize, marker: &str, translate: impl Fn(&str) -> String) -> (String, bool) {
    let full = translate(text);
    if full.chars().count() <= max {
        return (full, false);
    }
    let room = max.saturating_sub(marker.chars().count());
    let lines: Vec<&str> = text.lines().collect();
    for n in (1..lines.len()).rev() {
        let t = translate(&lines[..n].join("\n"));
        if t.chars().count() <= room {
            return (format!("{}{marker}", t.trim_end()), true);
        }
    }
    // Even the first line is too long: cut the translation itself.
    (truncate(&full, max, marker).unwrap_or(full), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_the_subset() {
        let src =
            "Revenue **€12,340** (_+4.1%_)\n- [Report](https://x.test/a_b) `code`\n- acme\\_corp has 2\\*3";
        assert_eq!(
            to_slack(src),
            "Revenue *€12,340* (_+4.1%_)\n• <https://x.test/a_b|Report> `code`\n• acme_corp has 2\u{2217}3"
        );
        assert_eq!(
            to_plain(src),
            "Revenue €12,340 (+4.1%)\n- Report (https://x.test/a_b) code\n- acme_corp has 2*3"
        );
        assert_eq!(
            to_html(src),
            "<p>Revenue <strong>€12,340</strong> (<em>+4.1%</em>)</p>\n<ul>\n<li><a href=\"https://x.test/a_b\">Report</a> <code>code</code></li>\n<li>acme_corp has 2*3</li>\n</ul>\n"
        );
        assert_eq!(
            to_teams(src),
            "Revenue **€12,340** (_+4.1%_)\n- [Report](https://x.test/a_b) `code`\n- acme_corp has 2\u{2217}3"
        );
    }

    #[test]
    fn escaped_values_stay_literal_in_code_and_at_word_edges() {
        // A template's `{{ v }}` arrives escaped: `a\_b` inside a code span, `\_pending\_` as text.
        assert_eq!(to_plain("id `a\\_b`"), "id a_b");
        assert_eq!(
            to_slack("state \\_pending\\_ ok"),
            "state \u{02cd}pending\u{02cd} ok"
        );
        assert_eq!(to_slack("snake_case"), "snake_case");
    }

    #[test]
    fn snake_case_and_lone_marks_stay_text() {
        assert_eq!(to_plain("total_rows is 3 * 4"), "total_rows is 3 * 4");
        assert_eq!(escape("a_b*[c]"), "a\\_b\\*\\[c\\]");
    }

    #[test]
    fn fit_drops_whole_source_lines() {
        let t = "**a & b**\n**c & d**\n**e & f**";
        assert_eq!(fit(t, 100, "…", to_slack), (to_slack(t), false));
        // `*a &amp; b*` is 11 characters; two lines would be 23 with the newline.
        assert_eq!(fit(t, 15, "\n…", to_slack), ("*a &amp; b*\n…".to_string(), true));
    }

    #[test]
    fn truncates_at_a_line_break() {
        let t = "line one\nline two\nline three";
        assert_eq!(truncate(t, 100, "…"), None);
        assert_eq!(truncate(t, 20, "\n…").unwrap(), "line one\nline two\n…");
    }
}
