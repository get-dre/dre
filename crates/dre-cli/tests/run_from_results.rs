//! `dre run --from-results`: re-render a report's outputs from results saved with
//! `--output-json`, with no query and no connection.

mod common;

use common::TestProject;

const PROFILES: &str = "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n\
destinations:\n  outbox:\n    targets:\n      dev: {type: local}\n";

/// The same profile, pointing at a database that doesn't exist (and can't be created).
const UNREACHABLE: &str = "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: no/such/dir/data.duckdb, read_only: true}\n\
destinations:\n  outbox:\n    targets:\n      dev: {type: local}\n";

fn project(report: &str) -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, csv, xlsx]\n"),
            ("reports/fin/r/r.yml", report),
            (
                "reports/fin/r/accounts.sql",
                "select id as account_id, 'Client ' || id as name, (id * 100.25)::decimal(18,2) as balance, date '2026-09-30' as as_of from range(1, 4) t(id) order by id",
            ),
            (
                "reports/fin/r/total.sql",
                "select sum(id) as n from range(1, 4) t(id)",
            ),
        ],
        PROFILES,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

fn unreachable(p: &TestProject) {
    std::fs::write(p.dir.path().join("profiles/profiles.yml"), UNREACHABLE).unwrap();
}

#[test]
fn saves_streams_and_replays_byte_identical_with_no_source() {
    let p = project(
        "queries: [accounts, total]\noutput:\n  - {name: book, format: xlsx}\n  - {name: flat, format: csv, queries: [accounts]}\n",
    );
    p.dre("run", &["r", "--no-deliver", "--output-json", "saved"])
        .ok();
    assert!(p.path("saved/r/default/accounts.arrows").exists());
    assert!(p.path("saved/r/default/accounts.json").exists());
    let index = p.json("saved/r/default/results.json");
    assert_eq!(index["schema_version"], "dre/saved-results/v1");
    assert_eq!(index["results"][0]["rows"], 3);
    let live_csv = std::fs::read(p.path("saved/run/r/default/flat.csv")).ok();
    // A normal run for the live files to compare with.
    p.dre("run", &["r", "--no-deliver"]).ok();
    let csv = std::fs::read(p.path("target/run/r/default/flat.csv")).unwrap();
    let xlsx = std::fs::read(p.path("target/run/r/default/book.xlsx")).unwrap();
    if let Some(saved) = live_csv {
        assert_eq!(saved, csv);
    }

    unreachable(&p);
    p.dre("run", &["r", "--from-results", "saved"])
        .ok()
        .says("no query ran");
    assert_eq!(
        std::fs::read(p.path("target/run/r/default/flat.csv")).unwrap(),
        csv
    );
    let replayed = std::fs::read(p.path("target/run/r/default/book.xlsx")).unwrap();
    assert_eq!(sheet_values(&replayed), sheet_values(&xlsx));
    // Not compared byte for byte: a workbook records when it was created.
    // Nothing delivered: `--from-results` implies `--no-deliver`.
    let r = p.json("target/run/r/default/run_results.json");
    assert!(r["deliveries"].as_array().unwrap().is_empty());
    assert_eq!(r["output_results"][0]["status"], "kept");
}

fn sheet_values(bytes: &[u8]) -> Vec<Vec<String>> {
    use calamine::{Reader, Xlsx};
    let mut wb: Xlsx<_> = Xlsx::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let mut out = Vec::new();
    for name in wb.sheet_names() {
        let range = wb.worksheet_range(&name).unwrap();
        out.extend(range.rows().map(|r| r.iter().map(|c| c.to_string()).collect()));
    }
    out
}

#[test]
fn a_missing_saved_result_is_an_error_and_changed_sql_a_warning() {
    let p = project("queries: [accounts]\noutput: {format: csv}\n");
    p.dre("run", &["r", "--no-deliver", "--output-json", "saved"])
        .ok();
    // The SQL changed since: a warning, the saved result is used.
    p.write("reports/fin/r/accounts.sql", "select 1 as account_id");
    p.dre("run", &["r", "--from-results", "saved"])
        .ok()
        .says("`accounts`'s SQL changed since its results were saved");
    assert!(p.read("target/run/r/default/r.csv").contains("Client 3"));
    // A query with nothing saved fails before rendering.
    p.write(
        "reports/fin/r/r.yml",
        "queries: [accounts, total]\noutput: {format: csv}\n",
    );
    unreachable(&p);
    p.dre("run", &["r", "--from-results", "saved"])
        .failed()
        .says("no saved result for `total`");
    // No saved results at all.
    p.dre("run", &["r", "--from-results", "elsewhere"])
        .failed()
        .says("save them with `dre run r --no-deliver --output-json elsewhere`");
}

#[test]
fn from_results_conflicts_with_saving() {
    let p = project("queries: [accounts]\noutput: {format: csv}\n");
    p.dre("run", &["r", "--from-results", "a", "--output-json", "b"])
        .failed();
}
