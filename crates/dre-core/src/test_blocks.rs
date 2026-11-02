//! Test blocks share MiniJinja's lexer and parser with ordinary macro definitions.

use std::collections::BTreeMap;

use minijinja::machinery::{
    self, Token,
    ast::{Expr, Stmt},
};

pub(crate) type Problem = (Option<usize>, String);

/// Rewrite only real block keywords and names, keeping all original newlines and trivia.
pub(crate) fn normalize(src: &str) -> Result<String, Problem> {
    let tokens: Vec<_> = machinery::tokenize(src, false, Default::default())
        .collect::<Result<_, _>>()
        .map_err(|e| (e.line(), e.detail().unwrap_or("invalid Jinja").into()))?;
    let mut edits = Vec::new();
    let mut blocks = Vec::new();
    for (index, window) in tokens.windows(2).enumerate() {
        if !matches!(window[0].0, Token::BlockStart) {
            continue;
        }
        let (Token::Ident(keyword), span) = &window[1] else {
            continue;
        };
        match *keyword {
            "test" | "macro" => blocks.push(*keyword),
            "endtest" | "endmacro" => {
                let expected = if *keyword == "endtest" { "test" } else { "macro" };
                if blocks.pop() != Some(expected) {
                    return Err((
                        Some(usize::from(span.start_line)),
                        format!("unexpected `{keyword}`; test blocks require matching test/endtest tags"),
                    ));
                }
            }
            _ => {}
        }
        let replacement = match *keyword {
            "test" => "macro",
            "endtest" => "endmacro",
            _ => continue,
        };
        edits.push((
            span.start_offset as usize,
            span.end_offset as usize,
            replacement.to_string(),
        ));
        if *keyword == "test" {
            let next = tokens.get(index + 2);
            if let Some((Token::Ident(name), name_span)) = next {
                edits.push((
                    name_span.start_offset as usize,
                    name_span.end_offset as usize,
                    format!("test_{name}"),
                ));
            }
        }
    }
    if let Some(block) = blocks.last() {
        return Err((Some(src.lines().count()), format!("unclosed `{block}` block")));
    }
    let mut normalized = src.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        normalized.replace_range(start..end, &replacement);
    }
    Ok(normalized)
}

#[derive(Debug)]
pub(crate) struct Definition {
    pub line: usize,
    pub arguments: Vec<String>,
    pub required: usize,
}

/// Parse without evaluating bodies, defaults, SQL, or introspection functions.
pub(crate) fn definitions(src: &str) -> Result<BTreeMap<String, Definition>, Problem> {
    let tree = machinery::parse(src, "macros", Default::default())
        .map_err(|e| (e.line(), e.detail().unwrap_or("invalid Jinja").into()))?;
    let mut definitions = BTreeMap::new();
    if let Stmt::Template(template) = tree {
        for statement in &template.children {
            let Stmt::Macro(m) = statement else { continue };
            let line = usize::from(m.span().start_line);
            let span = m.span();
            // Parser spans omit block delimiters; restore them for the lexer.
            let definition_source = &src[span.start_offset as usize..span.end_offset as usize];
            if m.name.starts_with("test_")
                && crate::preflight::Scan::new(&format!("{{% {definition_source} %}}"))
                    .called_names()
                    .iter()
                    .any(|name| name == "config")
            {
                return Err((
                    Some(line),
                    "runtime config() is unsupported in data tests; configure the declaration in YAML".into(),
                ));
            }
            let arguments: Vec<String> = m
                .args
                .iter()
                .filter_map(|arg| match arg {
                    Expr::Var(v) => Some(v.id.to_string()),
                    _ => None,
                })
                .collect();
            let definition = Definition {
                line,
                required: arguments.len() - m.defaults.len(),
                arguments,
            };
            if definitions.insert(m.name.to_string(), definition).is_some() {
                return Err((
                    Some(line),
                    format!("duplicate macro/test definition `{}`", m.name),
                ));
            }
        }
    }
    Ok(definitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_tags_change_and_line_numbers_survive() {
        let source = "{# {% test fake(x) %} #}\n{% raw %}{% test raw(x) %}{% endraw %}\n{{ '{% test string(x) %}' }}\n{%- test positive(relation, column='n') -%}\nselect * from {{ relation }}\n{%- endtest -%}";
        let normalized = normalize(source).unwrap();
        assert!(normalized.contains("{%- macro test_positive(relation"));
        assert!(normalized.contains("{%- endmacro -%}"));
        assert!(normalized.contains("{% test raw(x) %}"));
        assert!(normalized.contains("{% test string(x) %}"));
        let definitions = definitions(&normalized).unwrap();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions["test_positive"].line, 4);
        assert_eq!(definitions["test_positive"].required, 1);
    }

    #[test]
    fn collisions_and_runtime_config_are_errors() {
        for source in [
            "{% test x(relation) %}{% endtest %}{% macro test_x(relation) %}{% endmacro %}",
            "{% test x(relation) %}{{ config(severity='warn') }}{% endtest %}",
            "{% test x(relation) %}{% endmacro %}{% endtest %}",
            "{% test x(relation, severity=config()) %}{% endtest %}",
        ] {
            assert!(
                normalize(source).and_then(|source| definitions(&source)).is_err(),
                "{source}"
            );
        }
    }
}
