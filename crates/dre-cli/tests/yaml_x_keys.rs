//! Top-level `x-*` keys hold YAML anchors to reuse; DRE ignores them in every file it reads.

mod common;

use common::TestProject;

#[test]
fn x_keys_share_blocks_through_anchors_and_are_ignored() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: acme\ndefault_profile: duck\nx-notes: anything at all\n"),
            ("dependencies.yml", "plugins: [duckdb, csv]\nx-why: shared settings\n"),
            (
                "reports/a/a.yml",
                "x-out: &out {format: csv, destination: {profile: inbox, path: out/a.csv}}\nqueries: [qa]\noutput:\n  <<: *out\n",
            ),
            ("reports/a/qa.sql", "select 1 as n"),
        ],
        "x-duck: &duck {type: duckdb, path: a.duckdb}\nconnections:\n  duck:\n    targets:\n      dev: *duck\n\
         destinations:\n  inbox:\n    targets:\n      dev: {type: local}\n",
    );
    p.duckdb("a.duckdb", "select 1;");
    let v = p.dre("validate", &["--strict"]);
    v.ok();
    assert!(!v.stdout.contains("x-"), "{}", v.stdout);
    p.dre("run", &["a"]).ok();
    assert_eq!(p.read("out/a.csv"), "n\r\n1\r\n");
}
