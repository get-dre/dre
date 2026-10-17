//! `on_empty` and the empty-file contract: what a run does when queries return no rows.

mod common;

use calamine::{Reader, Xlsx, open_workbook};
use common::TestProject;

const PROFILES: &str =
    "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n";

fn project(report: &str) -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            (
                "dependencies.yml",
                "plugins: [duckdb, csv, parquet, fixed_width, xlsx]\n",
            ),
            ("reports/ops/r/r.yml", report),
            (
                "reports/ops/r/accounts.sql",
                "select id as account_id, 'Client ' || id as name from range(1, 3) t(id)",
            ),
            (
                "reports/ops/r/exceptions.sql",
                "select 1 as id, 'x' as reason where false",
            ),
            ("reports/ops/r/late.sql", "select 1 as id where false"),
            (
                "reports/ops/r/zero.sql",
                "select count(*) as n from range(0) t(id)",
            ),
        ],
        PROFILES,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

fn output_result(p: &TestProject) -> serde_json::Value {
    p.json("target/run/r/default/run_results.json")["output_results"][0].clone()
}

#[test]
fn default_continue_writes_an_empty_file_with_headers_and_warns() {
    let p = project("queries: [exceptions]\noutput: {format: csv}\n");
    p.dre("run", &["r"])
        .ok()
        .says("`exceptions` returned no rows; the output is written empty");
    assert_eq!(p.read("target/run/r/default/r.csv"), "id,reason\r\n");
    let o = output_result(&p);
    assert_eq!(o["status"], "kept");
    assert_eq!(o["no_data"], serde_json::json!(["exceptions"]));
}

#[test]
fn output_skip_writes_and_delivers_nothing() {
    let p = project("queries: [exceptions, late]\noutput: {format: csv, on_empty: skip}\n");
    p.dre("run", &["r"])
        .ok()
        .says("no data: `exceptions`, `late` returned no rows");
    let o = output_result(&p);
    assert_eq!(o["status"], "skipped");
    assert_eq!(o["reason"], "no_data");
    assert!(o["files"].as_array().unwrap().is_empty());
}

#[test]
fn output_fail_fails_the_run() {
    let p = project("queries: [exceptions]\noutput: {format: csv, on_empty: fail}\n");
    p.dre("run", &["r"])
        .failed()
        .says("returned no rows (`on_empty: fail`)");
    let r = p.json("target/run/r/default/run_results.json");
    assert_eq!(r["error_code"], "no-data");
    assert_eq!(r["output_results"][0]["reason"], "no_data");
}

#[test]
fn count_star_of_zero_is_a_row_not_empty() {
    let p = project("queries: [zero]\noutput: {format: csv, on_empty: fail}\n");
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.csv"), "n\r\n0\r\n");
}

#[test]
fn a_query_skip_drops_its_xlsx_tab_and_the_rest_goes_out() {
    let p = project("queries: [accounts, {query: exceptions, on_empty: skip}]\noutput: {format: xlsx}\n");
    p.dre("run", &["r"]).ok().says("left out (`on_empty: skip`)");
    let wb: Xlsx<_> = open_workbook(p.path("target/run/r/default/r.xlsx")).unwrap();
    assert_eq!(wb.sheet_names(), vec!["accounts".to_string()]);
    assert_eq!(output_result(&p)["dropped"], serde_json::json!(["exceptions"]));
}

#[test]
fn a_query_fail_fails_even_when_others_have_data() {
    let p = project("queries: [accounts, {query: exceptions, on_empty: fail}]\noutput: {format: xlsx}\n");
    p.dre("run", &["r"])
        .failed()
        .says("query `exceptions` returned no rows");
}

#[test]
fn a_query_skip_drops_its_csv_file() {
    let p = project("queries: [accounts, {query: exceptions, on_empty: skip}]\noutput: {format: csv}\n");
    p.dre("run", &["r"]).ok();
    assert!(
        p.path("target/run/r/default/r_accounts.csv").exists()
            || p.path("target/run/r/default/r.csv").exists()
    );
    assert!(!p.path("target/run/r/default/r_exceptions.csv").exists());
}

#[test]
fn every_tab_skipped_although_the_output_continues_is_skipped_with_a_warning() {
    let p = project(
        "queries: [{query: exceptions, on_empty: skip}, {query: late, on_empty: skip}]\noutput: {format: xlsx, on_empty: continue}\n",
    );
    p.dre("run", &["r"])
        .ok()
        .says("although it says `on_empty: continue`");
    assert_eq!(output_result(&p)["status"], "skipped");
}

#[test]
fn a_set_overrides_on_empty() {
    let p = project(
        "queries: [exceptions]\noutput: {format: csv, on_empty: skip}\nsets:\n  - {name: strict, output: {on_empty: fail}}\n  - {name: lax}\n",
    );
    p.dre("run", &["r", "--set", "lax"]).ok();
    p.dre("run", &["r", "--set", "strict"])
        .failed()
        .says("`on_empty: fail`");
}

#[test]
fn empty_files_keep_their_schema() {
    // parquet: a valid file with the full schema and no rows.
    let p = project("queries: [exceptions]\noutput: {format: parquet}\n");
    p.dre("run", &["r"]).ok();
    let f = std::fs::File::open(p.path("target/run/r/default/r.parquet")).unwrap();
    let b = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(f).unwrap();
    let names: Vec<String> = b.schema().fields().iter().map(|f| f.name().clone()).collect();
    assert_eq!(names, vec!["id", "reason"]);
    assert_eq!(
        b.build().unwrap().map(|b| b.unwrap().num_rows()).sum::<usize>(),
        0
    );
    // xlsx: the tab with its column names.
    let p = project("queries: [exceptions]\noutput: {format: xlsx}\n");
    p.dre("run", &["r"]).ok();
    let mut wb: Xlsx<_> = open_workbook(p.path("target/run/r/default/r.xlsx")).unwrap();
    let range = wb.worksheet_range("exceptions").unwrap();
    assert_eq!(
        range
            .rows()
            .next()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>(),
        vec!["id", "reason"]
    );
    assert_eq!(range.height(), 1);
    // fixed_width: the header record with `header: true`, else empty.
    let p = project(
        "queries: [exceptions]\noutput:\n  format: fixed_width\n  header: true\n  line_ending: \"\\n\"\n  columns:\n    - {name: id, width: 3}\n    - {name: reason, width: 6}\n",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.txt"), "id reason\n");
}

#[test]
fn validate_warns_that_a_query_skip_has_no_effect_on_a_message() {
    let p = project(
        "queries: [{query: exceptions, on_empty: skip}]\noutput: {format: message, text: \"{{ results.exceptions.row_count }}\"}\n",
    );
    p.dre("validate", &[])
        .says("has `on_empty: skip`, which has no effect on a message output");
}

#[test]
fn an_unknown_on_empty_is_an_error() {
    let p = project("queries: [exceptions]\noutput: {format: csv, on_empty: maybe}\n");
    p.dre("validate", &[])
        .failed()
        .says("`on_empty` must be one of continue, skip, fail");
}
