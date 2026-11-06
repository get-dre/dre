//! Author mistakes fail loudly instead of delivering the wrong thing: unknown keys, names and
//! values are errors, not silent defaults.

mod common;

use common::TestProject;

const PROJECT: &str = "name: strict\ndefault_profile: fixture\nvars: {region: ALL}\n";

fn project() -> TestProject {
    TestProject::with_plugins(
        &[
            ("dre_project.yml", PROJECT),
            ("dependencies.yml", "plugins: [fixture]\n"),
            ("reports/r/r.yml", "queries: [data]\noutput: {format: message}\n"),
            ("reports/r/data.sql", "rows 2"),
        ],
        "connections:\n  fixture:\n    targets:\n      dev: {type: fixture}\n",
        &["dre-source-fixture"],
    )
}

#[test]
fn unmanaged_sql_requires_project_opt_in() {
    let p = project();
    p.write("reports/r/scratch.sql", "select 1");
    let out = p.dre("run", &[]);
    assert_eq!(out.code, 2, "{out:?}");
    out.says("unmanaged-report").says("allow_unmanaged");
    assert!(!p.path("target/run/r/default/current").exists());
    p.write(
        "dre_project.yml",
        &format!("{PROJECT}allow_unmanaged: true\ndefault_output: {{format: message}}\n"),
    );
    p.dre("validate", &[]).ok().says("unmanaged report");
}

#[test]
fn an_unknown_set_is_an_error_unless_it_is_ad_hoc() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries: [data]\noutput: {format: message}\nsets: [{name: eu}, {name: us}]\n",
    );
    p.dre("run", &["r", "--set", "euu"]).failed().says("euu");
    assert!(!p.path("target/run/r/euu").exists());
    p.dre("run", &["r", "--set", "trial", "--var", "region=AU"]).ok();
}

#[test]
fn unknown_keys_are_errors() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "reports/r/r.yml",
            "queries: [data]\noutput: {format: message}\nsets: [{name: eu, queries: [{query: data, tab_nmae: x}]}]\n",
            "tab_nmae",
        ),
        (
            "reports/r/r.yml",
            "queries: [data]\noutput: {format: message, template: {file: t.html, binding: []}}\n",
            "binding",
        ),
        (
            "sets.yml",
            "us: {vars: {region: US}, exclude: [data]}\n",
            "exclude",
        ),
        (
            "dre_project.yml",
            "name: strict\ndefault_profile: fixture\nformat_options: {cvs: {delimiter: ';'}}\n",
            "cvs",
        ),
        (
            "dre_project.yml",
            "name: strict\ndefault_profile: fixture\ndispatch: [{macro_namespace: x, search_order: [strict], serch: 1}]\n",
            "serch",
        ),
    ];
    for (file, text, key) in cases {
        let p = project();
        p.write("reports/r/t.html", "<p></p>");
        p.write(file, text);
        p.dre("validate", &[]).failed().says(key);
    }
    let p = project();
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        "connections:\n  fixture:\n    taget: prod\n    targets:\n      dev: {type: fixture}\n",
    )
    .unwrap();
    p.dre("validate", &[]).failed().says("taget");
}

#[test]
fn a_bare_selector_matching_several_kinds_is_an_error_and_dead_terms_too() {
    let p = project();
    p.write(
        "reports/a/a.yml",
        "queries: [a]\ntags: [monthly]\noutput: {format: message}\n",
    );
    p.write("reports/a/a.sql", "rows 1");
    p.write(
        "reports/b/b.yml",
        "queries: [b]\ntags: [monthly]\noutput: {format: message}\n",
    );
    p.write("reports/b/b.sql", "rows 1");
    p.dre("ls", &["-s", "monthly"]).ok().says("a").says("b");
    p.write(
        "reports/monthly/monthly.yml",
        "queries: [m]\noutput: {format: message}\n",
    );
    p.write("reports/monthly/m.sql", "rows 1");
    p.dre("ls", &["-s", "monthly"])
        .failed()
        .says("tag:monthly")
        .says("report:monthly");
    p.dre("ls", &["-s", "report:monthly"]).ok();
    p.dre("ls", &["-s", "tag:monthly"]).ok();
    // A report in a folder of its own name is one reading, not two.
    p.dre("ls", &["-s", "r"]).ok();
    p.dre("ls", &["-s", "a tag:montly"]).failed().says("tag:montly");
    p.dre("ls", &["-s", "a nothing.here"])
        .failed()
        .says("nothing.here");
}

#[test]
fn unrelated_yaml_with_a_name_is_ignored() {
    let p = project();
    p.write(
        "docker-compose.yml",
        "name: reports-stack\nservices: {db: {image: postgres}}\n",
    );
    p.dre("validate", &[]).ok();
}

#[test]
fn an_invalid_run_date_refuses_to_start() {
    let p = project();
    let out = p.dre_env("run", &["r"], &[("DRE_RUN_DATE", "2026-02-30")]);
    assert_eq!(out.code, 2, "{out:?}");
    out.says("DRE_RUN_DATE");
    assert!(!p.path("target/run/r/default/current").exists());
}

#[test]
fn output_path_needs_exactly_one_binding_and_destination() {
    let p = project();
    p.write(
        "reports/r/r.yml",
        "queries: [data]\noutput: {format: message}\nsets: [{name: eu}, {name: us}]\n",
    );
    p.dre("run", &["r", "--set", "all", "--output-path", "x.txt"])
        .failed()
        .says("--output-path");
    assert!(!p.path("target/run/r/eu/current").exists());
}

#[test]
fn an_unused_var_warns() {
    let p = project();
    p.dre("run", &["r", "--var", "regoin=EU"]).ok().says("regoin");
    p.dre("run", &["r", "--var", "region=EU"])
        .ok()
        .lacks("--var region");
}
