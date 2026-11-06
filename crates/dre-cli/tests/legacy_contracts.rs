//! Removed pre-freeze configuration cannot silently change a project's meaning.

mod common;

use common::TestProject;

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", "name: legacy\ndefault_profile: fixture\n"),
            ("dependencies.yml", "plugins: [fixture]\n"),
            ("reports/r/r.yml", "queries: [data]\noutput: {format: message}\n"),
            ("reports/r/data.sql", "rows 2"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    )
}

#[test]
fn profiles_sources_is_rejected_instead_of_loading_connections() {
    let p = project();
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        "sources:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
    )
    .unwrap();
    p.dre("validate", &["--no-auto-install"])
        .failed()
        .says("sources")
        .says("connections");
}

#[test]
fn plugins_must_be_declared_in_root_dependencies() {
    for file in ["extra.yml", "reports/r/plugins.yml", "dre_project.yml"] {
        let p = project();
        let text = if file == "dre_project.yml" {
            "name: legacy\ndefault_profile: fixture\nplugins: [fixture]\n"
        } else {
            "plugins: [fixture]\n"
        };
        p.write(file, text);
        p.dre("validate", &["--no-auto-install"])
            .failed()
            .says("plugins")
            .says("dependencies.yml");
    }
}

#[test]
fn packages_filename_alias_requires_an_explicit_rename() {
    let p = project();
    p.write("packages.yml", "packages: []\n");
    p.dre("validate", &["--no-auto-install"])
        .failed()
        .says("packages.yml")
        .says("dependencies.yml");
}

#[test]
fn a_new_run_does_not_move_files_from_before_run_folders() {
    let p = project();
    let old = p.root().join("target/run/r/default/retained.txt");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "retain this file").unwrap();
    p.dre("run", &["r"]).ok();
    assert_eq!(std::fs::read_to_string(old).unwrap(), "retain this file");
}
