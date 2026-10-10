//! `dre validate` asks each profile entry's plugin to check its settings without connecting:
//! the entries the run would use by default, every entry with `--all-targets`.

mod common;

use common::TestProject;

fn project(profiles: &str) -> TestProject {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: acme\ndefault_profile: duck\n"),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            ("reports/daily/daily.yml", "queries: [q]\n"),
            ("reports/daily/q.sql", "select 1 as n"),
        ],
        profiles,
    );
    p.duckdb("a.duckdb", "select 1;");
    p
}

#[test]
fn a_bad_setting_fails_validate_and_run_with_the_same_message() {
    let p = project(
        "connections:\n  duck:\n    targets:\n      dev: {type: duckdb, path: a.duckdb, threads: many}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("invalid-connection-setting")
        .says("connection `duck` (entry `dev`): `threads` must be a whole number");
    p.dre("run", &["daily"])
        .failed()
        .says("`threads` must be a whole number");
}

#[test]
fn an_unknown_key_warns_and_fails_only_with_strict() {
    let p = project(
        "connections:\n  duck:\n    targets:\n      dev: {type: duckdb, path: a.duckdb, thread: 2}\n",
    );
    p.dre("validate", &[])
        .ok()
        .says("unknown key `thread` for source `duckdb`, ignored; did you mean `threads`?");
    p.dre("validate", &["--strict"]).failed();
    p.dre("run", &["daily"]).ok();
}

#[test]
fn only_the_entries_the_run_uses_unless_all_targets() {
    let p = project(
        "connections:\n  duck:\n    targets:\n      dev: {type: duckdb, path: a.duckdb}\n      prod: {type: duckdb, path: a.duckdb, threads: many}\n",
    );
    p.dre("validate", &[]).ok();
    p.dre("validate", &["--all-targets"])
        .failed()
        .says("connection `duck` (entry `prod`): `threads` must be a whole number");
    p.dre("validate", &["--target", "prod"])
        .failed()
        .says("`threads` must be a whole number");
}

#[test]
fn an_unset_env_var_warns_once_not_as_a_bad_value() {
    let p = project(
        "connections:\n  duck:\n    targets:\n      dev: {type: duckdb, path: a.duckdb, threads: \"{{ env_var('DRE_TEST_NO_SUCH_THREADS') }}\"}\n",
    );
    let r = p.dre("validate", &[]);
    r.ok().says("unset-env-var");
    let out = format!("{}{}", r.stdout, r.stderr);
    assert_eq!(out.matches("unset-env-var").count(), 1, "{out}");
    assert!(!out.contains("whole number"), "{out}");
}
