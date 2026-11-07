//! Catalogue discovery resolves provided source IDs independently of their package names.
mod common;

use common::TestProject;

#[test]
fn catalogue_uses_a_source_from_a_differently_named_local_package() {
    let executable = format!("bin/warehouse{}", std::env::consts::EXE_SUFFIX);
    let dependencies = format!("plugins:\n  - {{name: warehouse_bundle, local: {executable}}}\n");
    let p = TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: catalogue\ndefault_profile: warehouse\n"),
            ("dependencies.yml", &dependencies),
            ("tables.json", "{\"tables\":[\"fixture_table\"]}"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    );
    std::fs::create_dir_all(p.path("bin")).unwrap();
    std::fs::copy(common::workspace_bin("dre-source-fixture"), p.path(&executable)).unwrap();
    p.dre("deps", &[]).ok();
    let lock = p.read("dre.lock");
    assert!(
        lock.contains(&format!("warehouse_bundle: {executable}")),
        "{lock}"
    );
    p.dre(
        "catalog",
        &[
            "--profile",
            "warehouse",
            "--tables-file",
            "tables.json",
            "--output-file",
            "catalog.json",
        ],
    )
    .ok();
    let catalog = p.json("catalog.json");
    assert_eq!(catalog["source_type"], "fixture");
    assert_eq!(catalog["tables"][0]["description"], "Native fixture table");
    assert_eq!(
        catalog["tables"][0]["columns"][0]["description"],
        "Native fixture column"
    );
}

#[test]
fn catalogue_does_not_create_a_missing_duckdb_database() {
    let p = TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: catalogue\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb]\n"),
            ("tables.json", "{\"tables\":[\"orders\"]}"),
        ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: missing.duckdb}\n",
        &["dre-plugin-duckdb"],
    );
    p.dre(
        "catalog",
        &[
            "--profile",
            "warehouse",
            "--tables-file",
            "tables.json",
            "--output-file",
            "catalog.json",
        ],
    )
    .failed();
    assert!(!p.path("missing.duckdb").exists());
    assert!(!p.path("catalog.json").exists());
}
