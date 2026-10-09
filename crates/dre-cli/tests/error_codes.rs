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
    if std::env::var_os("DRE_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &page).unwrap();
    } else {
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            page,
            "docs/reference-error-codes.md is stale: run `DRE_UPDATE_DOCS=1 cargo test -p dre-cli --test error_codes`"
        );
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
