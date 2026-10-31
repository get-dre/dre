//! `dre fonts install` and `dre fonts ls` against an offline registry copy.

use serde_json::json;

/// `dre fonts <args>` with HOME/USERPROFILE and the registry isolated: exit code and output.
/// Not `TestProject::dre_env`: `dre fonts` takes no project, so no `--project-dir`.
fn dre_fonts(home: &std::path::Path, registry: &std::path::Path, args: &[&str]) -> (i32, String) {
    let out = assert_cmd::Command::cargo_bin("dre")
        .unwrap()
        .arg("fonts")
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("DRE_REGISTRY_URL", registry)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn install_then_ls_shows_the_cached_font_and_its_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("font.otf");
    std::fs::write(&file, b"font bytes").unwrap();
    // Independent SHA-256 literal of the fixture bytes.
    let sum = "ee2e88308610019dce3856035fb572b7675dc9d4c990091374349476b5beecb1";
    let a = json!({"url": file, "sha256": sum});
    let registry = dir.path().join("packages.json");
    std::fs::write(
        &registry,
        json!({"schema":2,"plugins":[],"fonts":[{"name":"noto_sans_arabic","license":"OFL-1.1",
            "scripts":["Arab"],"versions":[{"version":"2.13.0","regular":a,"bold":a,"license":a}]}]})
        .to_string(),
    )
    .unwrap();
    let home = dir.path().join("home");

    let (code, out) = dre_fonts(&home, &registry, &["ls"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No fonts cached"), "{out}");

    let (code, out) = dre_fonts(&home, &registry, &["install", "ar"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Installed  noto_sans_arabic 2.13.0 (Arab)"), "{out}");
    assert!(
        out.contains(&home.display().to_string()),
        "the font cache escaped the test home: {out}"
    );

    let (code, out) = dre_fonts(&home, &registry, &["ls"]);
    assert_eq!(code, 0, "{out}");
    let row = out
        .lines()
        .find(|l| l.starts_with("noto_sans_arabic"))
        .unwrap_or_default();
    let cells: Vec<_> = row.split_whitespace().collect();
    assert_eq!(
        cells,
        ["noto_sans_arabic", "2.13.0", "Arab", "30", "B", "ok"],
        "{out}"
    );

    let (code, out) = dre_fonts(&home, &registry, &["install", "xx"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("no font in the registry covers `xx`; known scripts: `Arab` (also `ar`)"),
        "{out}"
    );
}

#[test]
fn help_explains_the_lang_rule_and_the_offline_routes() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = dre_fonts(dir.path(), &dir.path().join("none.json"), &["--help"]);
    assert_eq!(code, 0, "{out}");
    for needle in [
        "`lang`",
        "DRE_REGISTRY_URL",
        "dre fonts install",
        "@font-face",
        "missing_fonts: error",
    ] {
        assert!(out.contains(needle), "{needle}: {out}");
    }
}
