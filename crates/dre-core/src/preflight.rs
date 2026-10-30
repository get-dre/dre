//! Jinja pre-flight: syntax checks and static scans of what a template reads and calls
//! (`var()`, `env_var()`, `ref()`, `run.*`, macros). Nothing is rendered with live values here.
//!
//! Templates are read by MiniJinja itself, the engine [`crate::render`] renders with: its parser
//! checks the syntax, and its lexer finds the call sites, so strings, comments and `{% raw %}`
//! blocks never count as Jinja. Call sites are found from the token stream rather than the syntax
//! tree, so a template with a syntax error still yields the `var()` and `ref()` calls before and
//! after the error, as the checks that run alongside the syntax error expect.

use minijinja::machinery::Token;
use minijinja::machinery::{self};

/// `run.*` attributes the runtime context provides.
pub const RUN_ATTRS: &[&str] = &[
    "report",
    "set",
    "target",
    "schedule",
    "date",
    "date_format",
    "now",
    "scheduled_at",
    "timezone",
];
/// Attributes and methods of `run.date`.
pub use crate::dates::DATE_ATTRS;

/// Whether a string contains Jinja at all.
pub fn is_templated(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// Compile-check a template with MiniJinja's parser; returns `(line, message)` on a syntax error.
pub fn check_syntax(name: &str, src: &str) -> Result<(), (Option<usize>, String)> {
    match machinery::parse(src, name, Default::default()) {
        Ok(_) => Ok(()),
        Err(e) => Err((e.line(), e.detail().unwrap_or("syntax error").to_string())),
    }
}

/// A `var('x')` or `env_var('X')` call with a literal name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// `var` or `env_var`.
    pub func: &'static str,
    pub name: String,
    pub has_default: bool,
    /// 1-based line within the scanned source.
    pub line: usize,
}

/// One step after a name: `.attr`, `['key']` or `[0]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Attr(String),
    Key(String),
    Index(u64),
}

impl Step {
    /// The attribute or key name; `None` for an index.
    pub fn name(&self) -> Option<&str> {
        match self {
            Step::Attr(s) | Step::Key(s) => Some(s),
            Step::Index(_) => None,
        }
    }
}

/// A global name and what's read from it: `run.date.yyyymmdd`, `results['q'].value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    pub root: String,
    /// At least one step.
    pub steps: Vec<Step>,
    /// 1-based line of the root name.
    pub line: usize,
    /// Its token's index, to keep findings in source order.
    at: usize,
}

impl Path {
    /// The name of the step at `i` when it's an `.attr`.
    pub fn attr(&self, i: usize) -> Option<&str> {
        match self.steps.get(i) {
            Some(Step::Attr(a)) => Some(a),
            _ => None,
        }
    }
}

/// A `{% macro name(...) %} ... {% endmacro %}` and what its body reads and calls.
#[derive(Debug, Clone, Default)]
pub struct MacroDef {
    pub name: String,
    /// The body's scan; its lines count from the start of the file.
    pub body: Scan,
}

/// What a template reads and calls, from MiniJinja's lexer. Dynamic access isn't seen.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    calls: Vec<Call>,
    /// `ref('name')` with a literal name, with its line.
    refs: Vec<(String, usize)>,
    /// Names called like functions: `name(`, not `x.name(`.
    called: Vec<String>,
    paths: Vec<Path>,
    /// `role='source'` arguments: their lines and token indexes.
    source_roles: Vec<(usize, usize)>,
    macros: Vec<MacroDef>,
}

/// A string literal's value.
fn string<'t>(t: Option<&'t (Token<'_>, usize)>) -> Option<&'t str> {
    match t {
        Some((Token::Str(s), _)) => Some(s),
        Some((Token::String(s), _)) => Some(s),
        _ => None,
    }
}

/// Whether the token is `want`, a token without a value (`Token` has no `PartialEq`).
fn is(t: Option<&(Token<'_>, usize)>, want: &Token<'_>) -> bool {
    t.is_some_and(|(t, _)| std::mem::discriminant(t) == std::mem::discriminant(want))
}

impl Scan {
    /// Scan `src`. A lexing error (an unclosed string, say) ends the scan there; the syntax
    /// error itself is [`check_syntax`]'s to report.
    pub fn new(src: &str) -> Scan {
        let tokens: Vec<(Token<'_>, usize)> = machinery::tokenize(src, false, Default::default())
            .map_while(Result::ok)
            .map(|(t, span)| (t, usize::from(span.start_line)))
            .collect();
        let mut scan = Scan::from_tokens(&tokens);
        scan.macros = macro_bodies(&tokens)
            .into_iter()
            .map(|(name, body)| MacroDef {
                name,
                body: Scan::from_tokens(&tokens[body]),
            })
            .collect();
        scan
    }

    fn from_tokens(tokens: &[(Token<'_>, usize)]) -> Scan {
        let mut scan = Scan::default();
        for (i, (tok, line)) in tokens.iter().enumerate() {
            let Token::Ident(name) = tok else {
                continue;
            };
            if *name == "role"
                && is(tokens.get(i + 1), &Token::Assign)
                && string(tokens.get(i + 2)) == Some("source")
            {
                scan.source_roles.push((*line, i));
            }
            if i > 0 && is(tokens.get(i - 1), &Token::Dot) {
                continue;
            }
            if is(tokens.get(i + 1), &Token::ParenOpen) {
                scan.called.push(name.to_string());
                let arg = string(tokens.get(i + 2));
                let close = tokens.get(i + 3).map(|(t, _)| t);
                match (*name, arg, close) {
                    ("var" | "env_var", Some(arg), Some(Token::Comma | Token::ParenClose)) => {
                        scan.calls.push(Call {
                            func: if *name == "var" { "var" } else { "env_var" },
                            name: arg.to_string(),
                            has_default: matches!(close, Some(Token::Comma)),
                            line: *line,
                        });
                    }
                    ("ref", Some(arg), Some(Token::ParenClose)) => {
                        scan.refs.push((arg.to_string(), tokens[i + 2].1));
                    }
                    _ => {}
                }
                continue;
            }
            let steps = steps(tokens, i + 1);
            if !steps.is_empty() {
                scan.paths.push(Path {
                    root: name.to_string(),
                    steps,
                    line: *line,
                    at: i,
                });
            }
        }
        scan
    }

    /// Every `var('x')` / `env_var('X')` call with a literal name.
    pub fn calls(&self) -> &[Call] {
        &self.calls
    }

    /// Every `ref('name')` with a literal name, with its 1-based line.
    pub fn refs(&self) -> impl Iterator<Item = &str> {
        self.refs.iter().map(|(n, _)| n.as_str())
    }

    /// Every `ref('name')` with its line.
    pub fn refs_at(&self) -> &[(String, usize)] {
        &self.refs
    }

    /// Names called like functions (`name(`), not methods (`x.name(`).
    pub fn called_names(&self) -> &[String] {
        &self.called
    }

    /// Every read of the global `root`: `root.a.b`, `root['a']`.
    pub fn paths(&self, root: &str) -> impl Iterator<Item = &Path> {
        self.paths.iter().filter(move |p| p.root == root)
    }

    /// The attributes a template reads from the global `name` (`results`, `outputs`):
    /// `name.<attr>` and `name['attr']`, with their 1-based lines.
    pub fn attributes(&self, name: &str) -> Vec<(String, usize)> {
        self.paths(name)
            .filter_map(|p| Some((p.steps[0].name()?.to_string(), p.line)))
            .collect()
    }

    /// The macros the template defines.
    pub fn macros(&self) -> &[MacroDef] {
        &self.macros
    }

    /// Template names DRE 0.2 removed: `(name, what to write instead, line)`, in source order.
    pub fn removed_names(&self) -> Vec<(String, String, usize)> {
        let mut out: Vec<(usize, (String, String, usize))> = Vec::new();
        for p in self.paths("target") {
            let Some(field) = p.attr(0) else { continue };
            if field == "name" {
                continue;
            }
            let instead = match field {
                "type" => "`connection.type` (the query's connection)".to_string(),
                "profile" => "`connection.name` (the query's connection)".to_string(),
                f => format!(
                    "`connection.{f}` (the query's connection) or `profile('<name>').{f}`; `target` is now only the environment, `target.name`"
                ),
            };
            out.push((p.at, (format!("target.{field}"), instead, p.line)));
        }
        for p in self.paths("run") {
            let (Some(attr), Some(instead)) = (
                p.attr(0),
                match p.attr(0) {
                    Some("profile") => Some("`connection.name` (the query's connection)"),
                    Some("source_type") => Some("`connection.type` (the query's connection)"),
                    _ => None,
                },
            ) else {
                continue;
            };
            out.push((p.at, (format!("run.{attr}"), instead.to_string(), p.line)));
        }
        for (line, at) in &self.source_roles {
            out.push((*at, ("role='source'".into(), "`role='connection'`".into(), *line)));
        }
        out.sort_by_key(|(at, _)| *at);
        out.into_iter().map(|(_, r)| r).collect()
    }

    /// `run.*` references that aren't part of the runtime context.
    pub fn unknown_run_refs(&self) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for p in self.paths("run") {
            let Some(attr) = p.attr(0) else { continue };
            if matches!(attr, "profile" | "source_type") {
                // Reported by `removed_names`, with the replacement.
                continue;
            }
            if !RUN_ATTRS.contains(&attr) {
                out.push((format!("run.{attr}"), p.line));
            } else if attr == "date"
                && let Some(sub) = p.attr(1)
                && !DATE_ATTRS.contains(&sub)
            {
                out.push((format!("run.date.{sub}"), p.line));
            }
        }
        out
    }
}

/// Each `{% macro name(...) %} ... {% endmacro %}`: its name and the token range of its body.
/// Macros don't nest.
fn macro_bodies(tokens: &[(Token<'_>, usize)]) -> Vec<(String, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut open: Option<(String, usize)> = None;
    for (i, (tok, _)) in tokens.iter().enumerate() {
        if i == 0 || !is(tokens.get(i - 1), &Token::BlockStart) {
            continue;
        }
        match tok {
            Token::Ident("macro") => {
                if let Some((Token::Ident(name), _)) = tokens.get(i + 1) {
                    open = Some((name.to_string(), i + 2));
                }
            }
            Token::Ident("endmacro") => {
                if let Some((name, start)) = open.take() {
                    out.push((name, start..i));
                }
            }
            _ => {}
        }
    }
    out
}

/// The steps after a name at `tokens[from]`: `.attr`, `['key']`, `[0]`, as far as they go.
fn steps(tokens: &[(Token<'_>, usize)], from: usize) -> Vec<Step> {
    let mut out = Vec::new();
    let mut j = from;
    loop {
        match (tokens.get(j).map(|t| &t.0), tokens.get(j + 1).map(|t| &t.0)) {
            (Some(Token::Dot), Some(Token::Ident(a))) => {
                out.push(Step::Attr(a.to_string()));
                j += 2;
            }
            (Some(Token::BracketOpen), _) if is(tokens.get(j + 2), &Token::BracketClose) => {
                match (string(tokens.get(j + 1)), tokens.get(j + 1).map(|t| &t.0)) {
                    (Some(k), _) => out.push(Step::Key(k.to_string())),
                    (None, Some(Token::Int(n))) => out.push(Step::Index(*n)),
                    _ => break,
                }
                j += 3;
            }
            _ => break,
        }
    }
    out
}

/// Every `var('x')` / `env_var('X')` call inside Jinja blocks.
pub fn calls(src: &str) -> Vec<Call> {
    Scan::new(src).calls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_var_calls_with_and_without_defaults() {
        let src = "select {{ var('a') }},\n {{ var(\"b\", 1) }}, {{ env_var('HOME') }} from t";
        let c = calls(src);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].name.as_str(), c[0].has_default, c[0].line), ("a", false, 1));
        assert_eq!((c[1].name.as_str(), c[1].has_default, c[1].line), ("b", true, 2));
        assert_eq!((c[2].func, c[2].name.as_str()), ("env_var", "HOME"));
    }

    #[test]
    fn ignores_sql_outside_jinja_and_method_like_names() {
        assert!(calls("select var('x') from t").is_empty());
        assert!(calls("{{ my.var('x') }} {{ env_var_x('y') }}").is_empty());
        assert!(
            Scan::new("select run.dat from runs run")
                .unknown_run_refs()
                .is_empty()
        );
    }

    #[test]
    fn ignores_strings_comments_and_raw_blocks() {
        let src = "{{ \"var('a')\" }} {# {{ var('b') }} #}\n{% raw %}{{ var('c') }}{% endraw %}";
        assert!(calls(src).is_empty());
    }

    #[test]
    fn flags_unknown_run_attributes() {
        let src = "{{ run.report }} {{ run.dat }}\n{{ run.date.yyyymmdd }} {{ run.date.yymm }}";
        assert_eq!(
            Scan::new(src).unknown_run_refs(),
            vec![("run.dat".to_string(), 1), ("run.date.yymm".to_string(), 2)]
        );
    }

    #[test]
    fn flags_names_removed_in_0_2_in_source_order() {
        let src = "{{ run.profile }} {{ target.schema }}\n{{ target.name }} {{ profile('pg', role='source').host }}";
        let names: Vec<(String, usize)> = Scan::new(src)
            .removed_names()
            .into_iter()
            .map(|(n, _, l)| (n, l))
            .collect();
        assert_eq!(
            names,
            vec![
                ("run.profile".to_string(), 1),
                ("target.schema".to_string(), 1),
                ("role='source'".to_string(), 2),
            ]
        );
    }

    #[test]
    fn finds_refs_and_called_names() {
        let scan =
            Scan::new("{{ ref('a') }} {{ x.ref('b') }}\n{% call wrap() %}{{ helper(1) }}{% endcall %}");
        assert_eq!(scan.refs_at(), &[("a".to_string(), 1)]);
        assert_eq!(scan.called_names(), &["ref", "wrap", "helper"]);
    }

    #[test]
    fn a_macro_body_keeps_its_own_calls_with_file_lines() {
        let src = "{% macro a() %}{{ var('x') }}{% endmacro %}\n{% macro b(n) %}\n{{ a() }} {{ var('y', 1) }}\n{% endmacro %}";
        let scan = Scan::new(src);
        type Def<'a> = (&'a str, Vec<(&'a str, usize)>, &'a [String]);
        let defs: Vec<Def<'_>> = scan
            .macros()
            .iter()
            .map(|d| {
                (
                    d.name.as_str(),
                    d.body.calls().iter().map(|c| (c.name.as_str(), c.line)).collect(),
                    d.body.called_names(),
                )
            })
            .collect();
        assert_eq!(defs[0].0, "a");
        assert_eq!(defs[0].1, vec![("x", 1)]);
        assert_eq!(defs[1].0, "b");
        assert_eq!(defs[1].1, vec![("y", 3)]);
        assert_eq!(defs[1].2, &["a".to_string(), "var".to_string()]);
        // The whole file's scan sees every body.
        assert_eq!(scan.calls().len(), 2);
    }

    #[test]
    fn reads_paths_with_attributes_keys_and_indexes() {
        let scan = Scan::new("{{ results.q.rows[0].pw }} {{ results['r'].value }} {{ outputs.x.location }}");
        let results: Vec<&Path> = scan.paths("results").collect();
        assert_eq!(
            results[0].steps,
            vec![
                Step::Attr("q".into()),
                Step::Attr("rows".into()),
                Step::Index(0),
                Step::Attr("pw".into())
            ]
        );
        assert_eq!(
            scan.attributes("results"),
            vec![("q".to_string(), 1), ("r".to_string(), 1)]
        );
        assert_eq!(scan.attributes("outputs"), vec![("x".to_string(), 1)]);
    }

    #[test]
    fn still_scans_a_template_with_a_syntax_error() {
        let src = "{{ var('a') }}\n{% if %}{{ ref('b') }}";
        assert!(check_syntax("t", src).is_err());
        let scan = Scan::new(src);
        assert_eq!(scan.calls()[0].name, "a");
        assert_eq!(scan.refs().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn syntax_errors_match_the_renderers() {
        let src = "select 1\nwhere {{ var('x', 1) }";
        let env = minijinja::Environment::new();
        let e = env.template_from_named_str("t", src).unwrap_err();
        assert_eq!(
            check_syntax("t", src),
            Err((e.line(), e.detail().unwrap_or_default().to_string()))
        );
        assert_eq!(check_syntax("t", "{{ a }}"), Ok(()));
    }
}
