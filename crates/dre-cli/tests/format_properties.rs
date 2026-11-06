//! Format names carry no file or template semantics; their plugins describe those properties.
mod common;
use common::TestProject;

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: properties\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture, folio]\n"),
            (
                "reports/r/r.yml",
                "queries: [north, south]\noutput: {format: folio}\n",
            ),
            ("reports/r/north.sql", "rows 2"),
            ("reports/r/south.sql", "rows 1"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture", "dre-format-folio"],
    )
}

#[test]
fn unfamiliar_format_properties_control_files() {
    let p = project();
    p.dre("validate", &["--no-auto-install"]).ok();
    p.dre("run", &["--no-auto-install"]).ok();
    assert_eq!(
        p.json("target/run/r/default/r_north.data")["sets"],
        serde_json::json!(["north"])
    );
    assert_eq!(
        p.json("target/run/r/default/r_south.data")["sets"],
        serde_json::json!(["south"])
    );
    assert!(!p.path("target/run/r/default/r.folio").exists());
}

#[test]
fn unfamiliar_document_receives_template_context() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries: [north, south]\noutput: {format: folio, template: {file: report.html}}\n",
    );
    p.write("templates/report.html", "{{ results.north.row_count }}");
    let env = [("DRE_FIXTURE_DOCUMENT", "1")];
    p.dre_env("validate", &["--no-auto-install"], &env).ok();
    p.dre_env("run", &["--no-auto-install"], &env).ok();
    let written = p.json("target/run/r/default/r.data");
    assert_eq!(written["sets"], serde_json::json!(["north", "south"]));
    assert_eq!(written["template"]["run"]["report"], "r", "{written}");
    assert!(written["template"]["vars"].is_object(), "{written}");
    assert!(written["template"]["project_root"].is_string(), "{written}");
}

#[test]
fn missing_properties_warn_and_omit_file_predictions() {
    let p = project();
    let env = [("DRE_FIXTURE_NO_PROPERTIES", "1")];
    let result = p.dre_env("validate", &["--no-auto-install", "--json"], &env);
    result.ok();
    let value: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    assert!(value.to_string().contains("properties unavailable"), "{value}");
    assert!(
        value["compiled"][0]["outputs"].as_array().unwrap().is_empty(),
        "{value}"
    );
    p.dre_env("run", &["--no-auto-install"], &env).failed();
    assert!(!p.path("target/run/r/default/r").exists());
}

#[test]
fn unfamiliar_workbook_applies_sheet_rules_to_its_sections_only() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries:\n  - {query: north, tab_name: 'invalid/name'}\n  - south\noutput: {format: folio}\n",
    );
    let env = [("DRE_FIXTURE_WORKBOOK", "1")];
    p.dre_env("run", &["--no-auto-install"], &env).failed();
    assert!(!p.path("target/run/r/default/r.data").exists());
}

#[test]
fn unfamiliar_multiple_table_format_keeps_one_file() {
    let p = project();
    let env = [("DRE_FIXTURE_MULTIPLE_TABLES", "1")];
    p.dre_env("run", &["--no-auto-install"], &env).ok();
    assert_eq!(
        p.json("target/run/r/default/r.data")["sets"],
        serde_json::json!(["north", "south"])
    );
}

#[test]
fn invalid_plugin_extension_cannot_make_a_path() {
    let p = project();
    let env = [("DRE_FIXTURE_EXTENSION", "../escape")];
    let result = p.dre_env("validate", &["--no-auto-install"], &env);
    result.ok();
    assert!(
        result.stdout.contains("format_properties.extension"),
        "{}",
        result.stdout
    );
    p.dre_env("run", &["--no-auto-install"], &env).failed();
}

#[test]
fn workbook_driver_is_not_a_sheet() {
    let p = project();
    p.write("reports/r/r.yml", "queries:\n  - {query: north, tab_name: 'invalid/name'}\n  - south\noutput:\n  format: folio\n  for_each: {query: north, key: n}\n");
    let env = [("DRE_FIXTURE_WORKBOOK", "1")];
    p.dre_env("run", &["--no-auto-install"], &env).ok();
}

#[test]
fn local_format_metadata_resolves_before_validation_and_manifest() {
    let mut p = project();
    let source = p
        .plugins
        .join(format!("dre-source-fixture{}", std::env::consts::EXE_SUFFIX));
    let format = p
        .plugins
        .join(format!("dre-format-folio{}", std::env::consts::EXE_SUFFIX));
    p.write(
        "dependencies.yml",
        &format!(
            "plugins:\n  - {{name: fixture, local: {}}}\n  - {{name: folio, local: {}}}\n",
            serde_json::to_string(&source.to_string_lossy()).unwrap(),
            serde_json::to_string(&format.to_string_lossy()).unwrap(),
        ),
    );
    p.plugins = p.dir.path().join("installed");
    std::fs::create_dir_all(&p.plugins).unwrap();
    p.write(
        "reports/r/r.yml",
        "queries: [north, south]\noutput: {format: folio, template: {file: report.html}}\n",
    );
    p.write("templates/report.html", "{{ results.north.row_count }}");
    let env = [("DRE_FIXTURE_DOCUMENT", "1")];
    p.dre_env("validate", &[], &env).ok();
    assert_eq!(
        p.json("target/manifest.json")["reports"]["r"]["bindings"][0]["outputs"][0]["format"],
        "folio"
    );
    p.dre_env("run", &[], &env).ok();
    assert!(p.path("target/run/r/default/r.data").exists());
}
