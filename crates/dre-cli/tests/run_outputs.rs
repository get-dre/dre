//! Several outputs per report: the queries run once and each output formats its own subset.

mod common;

use common::TestProject;
use serde_json::Value;

fn profiles(rec: &str) -> String {
    format!(
        "connections:\n  warehouse:\n    targets:\n      dev: {{type: duckdb, path: data.duckdb}}\n\
         destinations:\n\
         \x20 inbox:\n    targets:\n      dev: {{type: local}}\n\
         \x20 rec:\n    targets:\n      dev: {{type: fixture, dir: \"{rec}\"}}\n"
    )
}

fn project_with(files: &[(&str, &str)]) -> (TestProject, std::path::PathBuf) {
    let rec_dir = tempfile::tempdir().unwrap().keep();
    let rec = rec_dir.to_string_lossy().replace('\\', "/");
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("dependencies.yml", "plugins: [duckdb, csv, fixture]\n"),
        ("reports/ops/daily/headline.sql", "select 42 as total"),
        (
            "reports/ops/daily/detail.sql",
            "select * from (values (1, 'a'), (2, 'b')) t(id, name)",
        ),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, &profiles(&rec));
    p.duckdb("data.duckdb", "select 1;");
    (p, rec_dir)
}

fn project(report_yml: &str) -> (TestProject, std::path::PathBuf) {
    project_with(&[("reports/ops/daily/daily.yml", report_yml)])
}

fn deliveries(rec: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_list_formats_each_output_from_one_run_of_the_queries() {
    let (p, rec) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: workbook\n    queries: [detail]\n    destination: {profile: inbox, path: out/detail.csv}\n\
         \x20 - name: summary\n    queries: [headline]\n    destination: {profile: rec, path: archive/summary.csv}\n",
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(p.read("out/detail.csv"), "id,name\r\n1,a\r\n2,b\r\n");
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["files"][0]["remote"], "archive/summary.csv");
    assert_eq!(d[0]["files"][0]["content"], "total\r\n42\r\n");

    let r = p.json("target/run/daily/default/run_results.json");
    // Each query ran once.
    assert_eq!(r["result_sets"].as_array().unwrap().len(), 2);
    let outs = r["output_results"].as_array().unwrap();
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0]["name"], "workbook");
    assert_eq!(outs[0]["format"], "csv");
    assert_eq!(outs[0]["status"], "delivered");
    assert_eq!(outs[0]["queries"], serde_json::json!(["detail"]));
    assert_eq!(outs[0]["deliveries"][0]["profile"], "inbox");
    assert_eq!(outs[1]["name"], "summary");
    assert_eq!(outs[1]["deliveries"][0]["profile"], "rec");
    // The top-level fields cover every output.
    assert_eq!(r["deliveries"].as_array().unwrap().len(), 2);
    assert_eq!(r["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(r["outputs"][0]["output"], "workbook");
}

#[test]
fn named_outputs_use_their_name_in_the_default_file_name() {
    let (p, _) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - {name: totals, queries: [headline]}\n\
         \x20 - {name: rows, queries: [detail]}\n",
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(p.read("target/run/daily/default/totals.csv"), "total\r\n42\r\n");
    assert_eq!(
        p.read("target/run/daily/default/rows.csv"),
        "id,name\r\n1,a\r\n2,b\r\n"
    );
}

#[test]
fn a_single_map_still_writes_report_named_files() {
    let (p, _) = project("queries: [headline]\noutput: {format: csv}\n");
    p.dre("run", &["daily"]).ok();
    assert_eq!(p.read("target/run/daily/default/daily.csv"), "total\r\n42\r\n");
    let r = p.json("target/run/daily/default/run_results.json");
    let outs = r["output_results"].as_array().unwrap();
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0]["name"], Value::Null);
    assert_eq!(outs[0]["status"], "kept");
}

#[test]
fn output_names_must_be_unique_and_queries_must_exist() {
    let (p, _) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - {name: a, queries: [headline]}\n\
         \x20 - {name: a, queries: [detial]}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("output `a` is declared twice")
        .says("`queries` names `detial`, which isn't one of this Binding's queries");
}

#[test]
fn a_query_feeding_no_output_is_a_warning() {
    let (p, _) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - {name: a, queries: [headline]}\n",
    );
    p.dre("validate", &[]).ok().says("query `detail` feeds no output");
}

#[test]
fn a_set_overrides_one_output_by_name() {
    let (p, rec) = project_with(&[
        ("sets.yml", "acme: {vars: {c: a}}\nglobex: {vars: {c: g}}\n"),
        (
            "reports/ops/daily/daily.yml",
            "queries: [headline, detail]\noutput:\n\
             \x20 - name: workbook\n    queries: [detail]\n    destination: {profile: inbox, path: out/detail.csv}\n\
             \x20 - name: summary\n    queries: [headline]\n    destination: {profile: rec, path: archive/summary.csv}\n\
             sets:\n\
             \x20 - name: acme\n    output: {name: summary, destination: {path: archive/acme.csv}}\n\
             \x20 - globex\n",
        ),
    ]);
    p.dre("run", &["daily", "--set", "acme"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d[0]["files"][0]["remote"], "archive/acme.csv");
    assert_eq!(p.read("out/detail.csv"), "id,name\r\n1,a\r\n2,b\r\n");
}

#[test]
fn a_set_override_without_a_name_is_ambiguous_with_several_outputs() {
    let (p, _) = project_with(&[
        ("sets.yml", "acme: {vars: {c: a}}\n"),
        (
            "reports/ops/daily/daily.yml",
            "queries: [headline, detail]\noutput:\n\
             \x20 - {name: a, queries: [headline]}\n\
             \x20 - {name: b, queries: [detail]}\n\
             sets:\n\
             \x20 - name: acme\n    output: {format: csv}\n",
        ),
    ]);
    p.dre("validate", &[])
        .failed()
        .says("it inherits 2 outputs, so it's unclear which one to change");
    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [headline, detail]\noutput:\n\
         \x20 - {name: a, queries: [headline]}\n\
         sets:\n\
         \x20 - name: acme\n    output: {name: zz}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("no inherited output is named `zz`");
}

#[test]
fn a_set_list_replaces_the_inherited_outputs() {
    let (p, _) = project_with(&[
        ("sets.yml", "acme: {vars: {c: a}}\n"),
        (
            "reports/ops/daily/daily.yml",
            "queries: [headline, detail]\noutput:\n\
             \x20 - {name: a, queries: [headline]}\n\
             \x20 - {name: b, queries: [detail]}\n\
             sets:\n\
             \x20 - name: acme\n    output:\n      - {name: only}\n",
        ),
    ]);
    p.dre("run", &["daily"]).ok();
    assert!(p.path("target/run/daily/acme/only_headline.csv").exists());
    assert!(!p.path("target/run/daily/acme/a.csv").exists());
}

#[test]
fn validate_lists_every_output_with_its_destinations() {
    let (p, _) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: workbook\n    queries: [detail]\n    destination: {profile: inbox, path: out/detail.csv}\n\
         \x20 - name: summary\n    queries: [headline]\n    destination: {profile: rec, path: archive/summary.csv}\n",
    );
    p.dre("validate", &["-s", "daily"])
        .ok()
        .says("target/run/daily/default/detail.csv (csv, output `workbook`)")
        .says("Delivers  inbox (local), target dev → out/detail.csv")
        .says("target/run/daily/default/summary.csv (csv, output `summary`)")
        .says("Delivers  rec (fixture), target dev → archive/summary.csv");
}

#[test]
fn output_name_and_path_flags_apply_to_the_first_output_only() {
    let (p, rec) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: workbook\n    queries: [detail]\n    destination: {profile: inbox, path: out/detail.csv}\n\
         \x20 - name: summary\n    queries: [headline]\n    destination: {profile: rec, path: archive/summary.csv}\n",
    );
    p.dre("run", &["daily", "--output-path", "out/other.csv"]).ok();
    assert!(p.path("out/other.csv").exists());
    assert_eq!(deliveries(&rec)[0]["files"][0]["remote"], "archive/summary.csv");
}
