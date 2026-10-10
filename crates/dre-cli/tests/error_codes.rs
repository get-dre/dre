//! The error codes reference page and `dre explain`.

use std::path::Path;
use std::process::{Command, Output};

fn dre(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dre"))
        .args(args)
        .output()
        .unwrap()
}

/// `docs/reference-error-codes.md` is generated from the registry. After changing a code,
/// `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --test error_codes` rewrites it.
#[test]
fn the_error_codes_page_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference-error-codes.md");
    let page = dre_core::codes::reference_page();
    let (body, nav) = split_nav(&std::fs::read_to_string(&path).unwrap_or_default());
    if std::env::var_os("DRE_UPDATE_DOCS").is_some() {
        // Keep the page's previous/next links (.github/scripts/docs_sections.py writes them).
        let nav = if nav.is_empty() {
            String::new()
        } else {
            format!("\n{nav}")
        };
        std::fs::write(&path, format!("{page}{nav}")).unwrap();
    } else {
        assert_eq!(
            format!("{}\n", body.trim_end()),
            page,
            "docs/reference-error-codes.md is stale: run `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --test error_codes`"
        );
    }
}

/// A docs page's text before its previous/next block, and the block.
fn split_nav(text: &str) -> (String, String) {
    match text.find("\n<!-- docs-nav") {
        Some(i) => (text[..=i].to_string(), text[i + 1..].to_string()),
        None => (text.to_string(), String::new()),
    }
}

#[test]
fn explain_prints_a_codes_explanation() {
    let out = dre(&["explain", "unknown-key"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("unknown-key (config)\n\nA key DRE doesn't know."),
        "{text}"
    );
    assert!(text.contains("reference-error-codes/#unknown-key"), "{text}");
    // As printed in a diagnostic.
    assert!(dre(&["explain", "error[yaml-syntax]"]).status.success());
}

#[test]
fn explain_suggests_codes_for_an_unknown_one() {
    let out = dre(&["explain", "unknown"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("did you mean") && err.contains("`unknown-key`"),
        "{err}"
    );
}
