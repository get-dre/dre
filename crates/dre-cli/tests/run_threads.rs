//! Bindings running at once: a connection entry's `threads:` and `--threads`.

mod common;

use std::time::Instant;

use common::TestProject;

fn profiles(threads: u32) -> String {
    format!(
        "connections:\n  fixture:\n    targets:\n      dev: {{type: fixture, threads: {threads}}}\n\
         destinations:\n  inbox:\n    targets:\n      dev: {{type: local}}\n"
    )
}

/// Three reports that each wait `secs` seconds on the fixture source, then write a file.
fn project(threads: u32, secs: u32, paths: [&str; 3]) -> TestProject {
    let sleep = format!("sleep {secs}");
    let yml = |p: &str| {
        format!(
            "queries:\n  - {{query: wait_{p}, tab: false}}\n  - rows_{p}\noutput:\n  format: csv\n  destination: {{profile: inbox, path: {p}}}\n"
        )
    };
    let names = ["a", "b", "c"];
    // Query names are unique project-wide: name them after the report.
    let mut files: Vec<(String, String)> = vec![
        (
            "dre_project.yml".into(),
            "name: acme\ndefault_profile: fixture\n".into(),
        ),
        ("dependencies.yml".into(), "plugins: [fixture, csv]\n".into()),
    ];
    for (n, path) in names.iter().zip(paths) {
        let key = n.to_string();
        files.push((
            format!("reports/{n}/{n}.yml"),
            yml(path)
                .replace(&format!("wait_{path}"), &format!("wait_{key}"))
                .replace(&format!("rows_{path}"), &format!("rows_{key}")),
        ));
        files.push((format!("reports/{n}/wait_{key}.sql"), sleep.clone()));
        files.push((format!("reports/{n}/rows_{key}.sql"), "rows 2".into()));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    TestProject::new(&refs, &profiles(threads))
}

#[test]
fn bindings_on_a_target_with_threads_run_at_once() {
    let p = project(3, 2, ["out/a.csv", "out/b.csv", "out/c.csv"]);
    let t = Instant::now();
    p.dre("run", &["--threads", "1"]).ok();
    let one = t.elapsed().as_secs_f64();
    let t = Instant::now();
    p.dre("run", &[]).ok();
    let at_once = t.elapsed().as_secs_f64();
    // The first Binding signs in alone, then the other two run together: one 2s wait saved.
    assert!(at_once < one - 1.5, "one at a time {one}s, at once {at_once}s");
    for f in ["out/a.csv", "out/b.csv", "out/c.csv"] {
        assert!(p.path(f).is_file(), "{f}");
    }
    // run_results keep declaration order.
    let r = p.json("target/run/a/default/run_results.json");
    assert_eq!(r["status"], "success");
}

#[test]
fn threads_one_and_the_flag_run_one_at_a_time() {
    let p = project(1, 1, ["out/a.csv", "out/b.csv", "out/c.csv"]);
    let t = Instant::now();
    p.dre("run", &[]).ok();
    assert!(t.elapsed().as_secs_f64() >= 3.0);
    let p = project(3, 1, ["out/a.csv", "out/b.csv", "out/c.csv"]);
    let t = Instant::now();
    p.dre("run", &["--threads", "1"]).ok();
    assert!(t.elapsed().as_secs_f64() >= 3.0);
    p.dre("run", &["--threads", "0"])
        .failed()
        .says("--threads must be a whole number of 1 or more");
}

#[test]
fn two_bindings_at_once_never_deliver_to_the_same_file() {
    let p = project(3, 1, ["out/a.csv", "out/same.csv", "out/same.csv"]);
    let r = p.dre("run", &[]);
    r.failed()
        .says("delivers to `out/same.csv` on destination `inbox` in this run too");
    assert!(p.path("out/a.csv").is_file());
}

#[test]
fn bindings_at_once_keep_their_own_temp_tables_and_a_failure_stops_only_itself() {
    let p = TestProject::new(
        &[
            ("dre_project.yml", "name: acme\ndefault_profile: mem\n"),
            ("dependencies.yml", "plugins: [duckdb, csv]\n"),
            (
                "reports/r/r.yml",
                "queries:\n  - {query: make_t, tab: false}\n  - read_t\noutput:\n  format: csv\n  destination: {profile: inbox, path: \"out/{{ var('v') }}.csv\"}\n\
                 sets:\n  - {name: one, vars: {v: 1}}\n  - {name: two, vars: {v: 2}}\n  - {name: three, vars: {v: 3}}\n",
            ),
            (
                "reports/r/make_t.sql",
                "create temp table t as select {{ var('v') }} as v",
            ),
            ("reports/r/read_t.sql", "select v from t"),
            (
                "reports/broken/broken.yml",
                "queries: [nope]\noutput: {format: csv}\n",
            ),
            ("reports/broken/nope.sql", "select * from no_such_table"),
        ],
        "connections:\n  mem:\n    targets:\n      dev: {type: duckdb, path: \":memory:\", threads: 4}\n\
         destinations:\n  inbox:\n    targets:\n      dev: {type: local}\n",
    );
    p.dre("run", &["--set", "all"]).failed().says("no_such_table");
    for v in ["1", "2", "3"] {
        assert_eq!(p.read(&format!("out/{v}.csv")), format!("v\r\n{v}\r\n"));
    }
}
