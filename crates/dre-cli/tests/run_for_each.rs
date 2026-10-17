//! Per-row outputs (`for_each`): one copy of an output per driver row, with the csv format and
//! the local destination.

mod common;

use common::TestProject;

const PROFILES: &str = "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n\
destinations:\n  outbox:\n    targets:\n      dev: {type: local}\n";

fn project(report: &str, extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        ("dre_project.yml", "name: fund\ndefault_profile: warehouse\n"),
        ("dependencies.yml", "plugins: [duckdb, csv, xlsx]\n"),
        ("reports/members/statements/statements.yml", report),
        (
            "reports/members/statements/members.sql",
            "select * from (values (101, 'Ann', 'ann@example.com'), (102, 'Bob', 'bob@example.com'), (103, 'Cat', 'cat@example.com')) t(member_id, name, email) order by member_id",
        ),
        (
            "reports/members/statements/holdings.sql",
            "select * from (values (101, 'Growth', 1000.5), (101, 'Cash', 20.0), (102, 'Growth', 300.0), (999, 'Lost', 1.0)) t(member_id, fund, amount)",
        ),
        (
            "reports/members/statements/transactions.sql",
            "select * from (values (101, date '2026-09-01', 50.0), (102, date '2026-09-02', 70.0)) t(member_id, on_date, amount)",
        ),
    ];
    files.extend_from_slice(extra);
    let p = TestProject::new(&files, PROFILES);
    p.duckdb("data.duckdb", "select 1;");
    p
}

const REPORT: &str = "queries: [members, holdings]\noutput:\n  name: statement\n  format: csv\n  for_each: {query: members, key: member_id}\n\
\x20 destination: {profile: outbox, path: \"out/{{ each.member_id }}-{{ each.name | lower }}.csv\"}\n";

fn results(p: &TestProject) -> serde_json::Value {
    p.json("target/run/statements/default/run_results.json")
}

fn rows(p: &TestProject) -> Vec<serde_json::Value> {
    p.read("target/run/statements/default/statement.rows.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn one_file_per_driver_row_with_only_its_rows() {
    let p = project(REPORT, &[]);
    p.dre("run", &["statements"])
        .ok()
        .says("3 succeeded, 0 skipped, 0 failed");
    // Each file holds only its member's holdings; the driver isn't a section.
    assert_eq!(
        p.read("target/run/statements/default/statement/101.csv"),
        "member_id,fund,amount\r\n101,Growth,1000.5\r\n101,Cash,20.0\r\n"
    );
    assert_eq!(
        p.read("target/run/statements/default/statement/102.csv"),
        "member_id,fund,amount\r\n102,Growth,300.0\r\n"
    );
    // A member with no holdings still gets their (empty) file: `on_empty: continue`.
    assert_eq!(
        p.read("target/run/statements/default/statement/103.csv"),
        "member_id,fund,amount\r\n"
    );
    // Delivered to a path rendered with `each.*`.
    assert!(p.path("out/101-ann.csv").exists() && p.path("out/103-cat.csv").exists());
    let r = results(&p);
    let fe = &r["output_results"][0]["for_each"];
    assert_eq!(fe["rows"], 3);
    assert_eq!(fe["succeeded"], 3);
    assert_eq!(fe["orphan_rows"], 1);
    assert_eq!(r["output_results"][0]["status"], "delivered");
    // Per-row files are left out of `outputs`; the sidecar has them.
    assert!(r["outputs"].as_array().unwrap().is_empty());
    let rows = rows(&p);
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|r| r["status"] == "succeeded" && r["files"][0]["sha256"].as_str().unwrap().len() == 64)
    );
}

#[test]
fn orphan_rows_warn_or_fail() {
    let p = project(REPORT, &[]);
    p.dre("run", &["statements"]).ok().says("1 row(s) of its queries have a key that isn't in the driver `members`, so they're in no copy (keys: 999)");
    let p = project(
        &REPORT.replace("key: member_id}", "key: member_id, on_orphan_rows: fail}"),
        &[],
    );
    p.dre("run", &["statements"])
        .failed()
        .says("`on_orphan_rows: fail`");
}

#[test]
fn file_name_and_composite_keys() {
    let p = project(
        "queries: [members, holdings]\noutput:\n  name: statement\n  format: csv\n  for_each: {query: members, key: [member_id, name]}\n  file_name: \"{{ each.name }}_statement.csv\"\n",
        &[(
            "reports/members/statements/holdings.sql",
            "select * from (values (101, 'Ann', 'Growth'), (102, 'Bob', 'Cash')) t(member_id, name, fund)",
        )],
    );
    p.dre("run", &["statements"]).ok();
    assert!(
        p.path("target/run/statements/default/statement/Ann_statement.csv")
            .exists()
    );
    // Rows finish in any order; the keys are the composite ones.
    let mut keys: Vec<String> = rows(&p)
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_string())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["101_Ann", "102_Bob", "103_Cat"]);
}

#[test]
fn duplicate_keys_and_colliding_names_fail_before_anything_renders() {
    let p = project(
        REPORT,
        &[(
            "reports/members/statements/members.sql",
            "select * from (values (101, 'Ann'), (101, 'Ann2')) t(member_id, name)",
        )],
    );
    p.dre("run", &["statements"])
        .failed()
        .says("has the key `101` twice (rows 1 and 2)");
    let p = project(
        "queries: [members, holdings]\noutput:\n  name: statement\n  format: csv\n  for_each: {query: members, key: member_id}\n  file_name: same.csv\n",
        &[],
    );
    p.dre("run", &["statements"])
        .failed()
        .says("would both write `same.csv`");
}

#[test]
fn several_data_queries_make_a_file_each_or_a_workbook() {
    let p = project(
        "queries: [members, holdings, transactions]\noutput:\n  name: statement\n  format: csv\n  for_each: {query: members, key: member_id}\n",
        &[],
    );
    p.dre("run", &["statements"]).ok();
    assert!(
        p.path("target/run/statements/default/statement/101_holdings.csv")
            .exists()
    );
    assert!(
        p.path("target/run/statements/default/statement/101_transactions.csv")
            .exists()
    );
    let p = project(
        "queries: [members, holdings, transactions]\noutput:\n  name: statement\n  format: xlsx\n  for_each: {query: members, key: member_id}\n",
        &[],
    );
    p.dre("run", &["statements"]).ok();
    assert!(
        p.path("target/run/statements/default/statement/102.xlsx")
            .exists()
    );
}

#[test]
fn a_set_switches_for_each_on_and_off() {
    let p = project(
        &format!(
            "{REPORT}sets:\n  - {{name: per_member}}\n  - {{name: one_file, output: {{for_each: false, destination: {{profile: outbox, path: out/all.csv}}}}}}\n"
        ),
        &[],
    );
    let run = p.dre("run", &["statements", "--set", "all"]);
    run.ok();
    assert!(p.path("out/101-ann.csv").exists(), "{}", run.stdout);
    // Without `for_each`: one file per query, named after the path, the driver included.
    let all = p.read("out/all_holdings.csv");
    assert!(all.contains("101,Growth") && all.contains("102,Growth"), "{all}");
    assert!(p.path("out/all_members.csv").exists());
}

#[test]
fn a_failed_row_is_recorded_and_the_others_carry_on() {
    // Row 102's path is a folder that exists: the local delivery fails for it alone.
    let p = project(REPORT, &[("out/102-bob.csv/keep", "")]);
    let run = p.dre("run", &["statements"]);
    run.failed()
        .says("2 succeeded, 0 skipped, 1 failed")
        .says("failed keys: 102");
    let r = results(&p);
    assert_eq!(r["error_code"], "rows-failed");
    let fe = &r["output_results"][0]["for_each"];
    assert_eq!(
        (fe["succeeded"].as_u64(), fe["failed"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(fe["failed_keys"][0]["key"], "102");
    assert!(p.path("out/101-ann.csv").exists() && p.path("out/103-cat.csv").exists());
    let failed: Vec<_> = rows(&p).into_iter().filter(|r| r["status"] == "failed").collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["deliveries"][0]["status"], "failed");
}

#[test]
fn on_row_error_fail_delivers_nothing_when_a_row_fails() {
    let p = project(
        &REPORT
            .replace("key: member_id}", "key: member_id, on_row_error: fail}")
            .replace("format: csv", "format: csv\n  on_empty: fail"),
        &[],
    );
    // Member 103 has no holdings: with `on_empty: fail` that row fails, so nobody's is delivered.
    p.dre("run", &["statements"]).failed();
    assert!(!p.path("out/101-ann.csv").exists());
    assert!(rows(&p).iter().all(|r| r["status"] == "failed"));
}

#[test]
fn on_empty_applies_per_row() {
    let p = project(
        &REPORT.replace("format: csv", "format: csv\n  on_empty: skip"),
        &[],
    );
    p.dre("run", &["statements"])
        .ok()
        .says("2 succeeded, 1 skipped, 0 failed");
    assert!(!p.path("out/103-cat.csv").exists());
    let skipped: Vec<_> = rows(&p)
        .into_iter()
        .filter(|r| r["status"] == "skipped")
        .collect();
    assert_eq!(skipped[0]["key"], "103");
    assert_eq!(skipped[0]["reason"], "no_data");
}

#[test]
fn an_empty_driver_makes_nothing() {
    let p = project(
        &REPORT.replace("format: csv", "format: csv\n  on_empty: skip"),
        &[(
            "reports/members/statements/members.sql",
            "select 1 as member_id, 'x' as name where false",
        )],
    );
    p.dre("run", &["statements"]).ok();
    assert_eq!(results(&p)["output_results"][0]["status"], "skipped");
}

#[test]
fn the_population_guard_fails_before_rendering() {
    let p = project(
        &REPORT.replace("key: member_id}", "key: member_id, max_population_rows: 5}"),
        &[],
    );
    p.dre("run", &["statements"])
        .failed()
        .says("more than `for_each.max_population_rows` (5)");
}

#[test]
fn concurrency_and_pacing() {
    let p = project(
        &REPORT
            .replace("key: member_id}", "key: member_id, concurrency: 3}")
            .replace("path: \"out/", "rate: 120/minute, path: \"out/"),
        &[],
    );
    let t = std::time::Instant::now();
    p.dre("run", &["statements"]).ok().says("3 at a time");
    // 120 a minute: one every half second, so three deliveries take at least a second.
    assert!(
        t.elapsed() >= std::time::Duration::from_millis(1000),
        "{:?}",
        t.elapsed()
    );
    // The CLI overrides the YAML.
    p.dre("run", &["statements", "--row-concurrency", "1"])
        .ok()
        .says("1 at a time");
    p.dre_env("run", &["statements"], &[("DRE_ROW_CONCURRENCY", "2")])
        .ok()
        .says("2 at a time");
}

#[test]
fn validate_checks_for_each() {
    let p = project(
        "queries: [members, holdings]\noutput:\n  format: csv\n  for_each: {query: nope, key: member_id}\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("`for_each.query` `nope` isn't a query of this report");
    let p = project(
        "queries: [members, holdings]\noutput:\n  format: csv\n  for_each: {query: members, key: member_id}\n  destination: {profile: outbox, path: out/same.csv}\n",
        &[],
    );
    p.dre("validate", &[]).failed().says("doesn't use `each.*`");
    let p = project(
        "queries: [members, holdings]\noutput:\n  format: csv\n  for_each: {query: members, key: email}\n",
        &[],
    );
    p.dre("validate", &[]).says("looks like personal data");
    let p = project(
        "queries: [members]\noutput: {format: csv, file_name: x.csv}\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("`file_name` names a per-row output's files; it needs `for_each`");
}

#[test]
fn retry_failed_redoes_only_the_failed_rows_from_the_saved_results() {
    let p = project(REPORT, &[("out/102-bob.csv/keep", "")]);
    p.dre("run", &["statements"]).failed();
    let first = results(&p)["run_id"].as_str().unwrap().to_string();
    assert!(
        p.path(&format!(
            "target/run/statements/default/runs/{first}/results/results.json"
        ))
        .exists()
    );
    // Fix the cause; the warehouse is gone, the saved results are used.
    std::fs::remove_dir_all(p.path("out/102-bob.csv")).unwrap();
    std::fs::remove_file(p.path("out/101-ann.csv")).unwrap();
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        PROFILES.replace("path: data.duckdb", "path: no/such/dir/x.duckdb, read_only: true"),
    )
    .unwrap();
    p.dre("run", &["statements", "--retry-failed", &first])
        .ok()
        .says("1 succeeded, 0 skipped, 0 failed");
    assert!(p.path("out/102-bob.csv").is_file());
    // Rows that succeeded the first time aren't sent again.
    assert!(!p.path("out/101-ann.csv").exists());
    assert_eq!(
        p.read("out/102-bob.csv"),
        "member_id,fund,amount\r\n102,Growth,300.0\r\n"
    );
    // The retry is its own run, which doesn't become current; the original's sidecar is untouched.
    let runs = p.path("target/run/statements/default/runs");
    let retry = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|id| *id != first)
        .unwrap();
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runs.join(&retry).join("run_results.json")).unwrap())
            .unwrap();
    assert_eq!(r["retry_of"], first.as_str());
    assert_eq!(r["output_results"][0]["for_each"]["partial"], true);
    assert_eq!(p.read("target/run/statements/default/current").trim(), first);
    assert_eq!(
        std::fs::read_to_string(runs.join(&first).join("statement.rows.jsonl"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    p.dre("history", &["statements"])
        .ok()
        .says(&format!("(retry of {first})"));
    // A second retry (of either run) has nothing left to do.
    std::fs::remove_file(p.path("out/102-bob.csv")).unwrap();
    p.dre(
        "run",
        &["statements", "--retry-failed", &retry, "--keep-runs", "5"],
    )
    .ok()
    .says("no row left to make");
    assert!(!p.path("out/102-bob.csv").exists());
}

#[test]
fn retry_of_a_missing_run_is_a_clear_error() {
    let p = project(REPORT, &[]);
    p.dre("run", &["statements", "--retry-failed", "20990101T000000Z-zzzz"])
        .failed()
        .says("has no run `20990101T000000Z-zzzz`");
}

#[test]
fn only_keys_and_limit_rows_make_some_rows() {
    let p = project(REPORT, &[]);
    p.dre("run", &["statements", "--only-keys", "102,103"])
        .ok()
        .says("2 succeeded");
    assert!(!p.path("out/101-ann.csv").exists() && p.path("out/102-bob.csv").exists());
    assert_eq!(results(&p)["output_results"][0]["for_each"]["partial"], true);
    p.dre("run", &["statements", "--limit-rows", "1"])
        .ok()
        .says("1 succeeded");
}

#[test]
fn preview_shows_three_rows_and_the_total_and_sends_nothing() {
    let p = project(
        "queries: [members, holdings]\noutput:\n  name: statement\n  format: csv\n  for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"out/{{ each.email }}.csv\"}\n",
        &[],
    );
    p.dre("run", &["statements", "--preview"])
        .ok()
        .says("row `101`")
        .says("outbox → out/a***@example.com.csv")
        .says("would make 3 row(s) and 3 deliveries; nothing was sent");
    assert!(!p.path("out").exists());
    p.dre(
        "run",
        &["statements", "--preview", "--unmask", "--only-keys", "102"],
    )
    .ok()
    .says("outbox → out/bob@example.com.csv");
}

#[test]
fn a_timed_out_run_keeps_its_finished_rows() {
    // 10 a minute, one at a time: a row every 6 seconds; the run stops at 8.
    let p = project(
        &REPORT
            .replace("key: member_id}", "key: member_id, concurrency: 1}")
            .replace("path: \"out/", "rate: 10/minute, path: \"out/"),
        &[],
    );
    let run = p.dre("run", &["statements", "--timeout", "8"]);
    assert_eq!(run.code, 124, "{}\n{}", run.stdout, run.stderr);
    let r = results(&p);
    assert_eq!(r["status"], "timed_out");
    // A slow machine may start late; whatever finished is recorded, and never all three.
    let sidecar = p.path("target/run/statements/default/statement.rows.jsonl");
    let done = if sidecar.exists() { rows(&p) } else { Vec::new() };
    assert!(done.len() < 3, "{done:?}");
    assert!(done.iter().all(|r| r["status"] == "succeeded"));
    assert!(
        r["output_results"][0]["for_each"]["not_started"]
            .as_u64()
            .unwrap_or(3)
            > 0
            || done.is_empty()
    );
}
