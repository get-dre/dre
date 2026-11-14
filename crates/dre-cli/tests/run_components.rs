//! Component lists in destination options: `component: <name>.json` entries rendered after the
//! queries, with `results` and `args`, and sent to the plugin as parsed JSON.

mod common;

use std::path::{Path, PathBuf};

use common::TestProject;
use serde_json::{Value, json};

fn profiles(rec: &str) -> String {
    format!(
        "connections:\n  warehouse:\n    targets:\n      dev: {{type: duckdb, path: data.duckdb}}\n\
         destinations:\n\
         \x20 rec:\n    targets:\n      dev: {{type: fixture, dir: \"{rec}\"}}\n"
    )
}

const REVENUE: (&str, &str) = (
    "reports/finance/daily/revenue.sql",
    "select * from (values ('mon', 10), ('tue', 20)) t(day, revenue) order by day",
);

const HEADER: (&str, &str) = (
    "components/header.json",
    r#"{"type": "header", "text": {"type": "plain_text", "text": {{ args.text | tojson }}}}"#,
);

fn project(report_yml: &str, extra: &[(&str, &str)]) -> (TestProject, PathBuf) {
    let rec_dir = tempfile::tempdir().unwrap().keep();
    let rec = rec_dir.to_string_lossy().replace('\\', "/");
    let mut files = vec![
        ("dre_project.yml", "name: acme\ndefault_profile: warehouse\n"),
        ("dependencies.yml", "plugins: [duckdb, csv, fixture]\n"),
        ("reports/finance/daily/daily.yml", report_yml),
        REVENUE,
    ];
    files.extend_from_slice(extra);
    let p = TestProject::new(&files, &profiles(&rec));
    p.duckdb("data.duckdb", "select 1;");
    (p, rec_dir)
}

fn blocks(rec: &Path) -> Value {
    let text = std::fs::read_to_string(rec.join("deliveries.jsonl")).unwrap_or_default();
    let line = text.lines().last().expect("a delivery");
    serde_json::from_str::<Value>(line).unwrap()["options"]["blocks"].clone()
}

/// A message output whose `rec` entry has a `blocks:` component list.
fn report(entries: &str) -> String {
    format!(
        "queries: [revenue]\noutput:\n  format: message\n  text: \"Revenue\"\n  destination:\n    profile: rec\n    blocks:\n{entries}"
    )
}

#[test]
fn components_render_after_the_queries_with_results_and_args_in_order() {
    let (p, rec) = project(
        &report(
            "      - component: header.json\n        with: {text: \"Daily {{ var('team', 'KPIs') }}\"}\n\
             \x20     - {type: divider, note: \"{{ run.report }}\"}\n\
             \x20     - component: rows.json\n        keep: last\n        with: {query: revenue}\n",
        ),
        &[
            HEADER,
            (
                "components/rows.json",
                r#"[{% for r in results[args.query].rows %}{"type": "section", "text": {{ (r.day ~ ': ' ~ r.revenue) | tojson }}}{% if not loop.last %},{% endif %}{% endfor %}]"#,
            ),
        ],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        blocks(&rec),
        json!([
            {"component": "components/header.json", "blocks": [
                {"type": "header", "text": {"type": "plain_text", "text": "Daily KPIs"}}
            ]},
            {"blocks": [{"type": "divider", "note": "daily"}]},
            {"component": "components/rows.json", "keep": "last", "blocks": [
                {"type": "section", "text": "mon: 10"},
                {"type": "section", "text": "tue: 20"}
            ]}
        ])
    );
}

fn all_blocks(rec: &Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["options"]["blocks"].clone())
        .collect()
}

/// A component whose one block names where it was found.
fn marker(file: &'static str, at: &'static str) -> (&'static str, &'static str) {
    (file, at)
}

#[test]
fn the_nearest_component_wins_and_a_path_is_from_the_root() {
    let (p, rec) = project(
        &report("      - component: kpi.json\n      - component: components/kpi.json\n"),
        &[
            marker("components/kpi.json", r#"{"at": "root"}"#),
            marker("reports/finance/kpi.json", r#"{"at": "finance"}"#),
        ],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        blocks(&rec),
        json!([
            {"component": "reports/finance/kpi.json", "blocks": [{"at": "finance"}]},
            {"component": "components/kpi.json", "blocks": [{"at": "root"}]}
        ])
    );
}

/// A local `dre_utils` package with `components/slack/kpi.json` and `components/slack/bar.json`.
fn with_package(p: &TestProject) {
    let dir = p.root().join("../shared/dre_utils");
    std::fs::create_dir_all(dir.join("components/slack")).unwrap();
    std::fs::write(dir.join("dre_package.yml"), "name: dre_utils\n").unwrap();
    std::fs::write(dir.join("components/slack/kpi.json"), r#"{"at": "package"}"#).unwrap();
    std::fs::write(
        dir.join("components/slack/bar.json"),
        r#"{"type": "bar", "title": {{ args.title | tojson }}, "n": {{ results.revenue.row_count }}}"#,
    )
    .unwrap();
    p.write(
        "dependencies.yml",
        "plugins: [duckdb, csv, fixture]\npackages:\n  - local: ../shared/dre_utils\n",
    );
}

#[test]
fn packages_serve_components_by_package_path_or_a_bare_name_the_project_lacks() {
    let (p, rec) = project(
        &report(
            "      - component: dre_utils/components/slack/bar.json\n        with: {title: \"Revenue {{ 1 + 1 }}\"}\n\
             \x20     - component: kpi.json\n",
        ),
        &[],
    );
    with_package(&p);
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        blocks(&rec),
        json!([
            {"component": "dre_utils/components/slack/bar.json", "blocks": [{"type": "bar", "title": "Revenue 2", "n": 2}]},
            {"component": "dre_utils/components/slack/kpi.json", "blocks": [{"at": "package"}]}
        ])
    );
}

#[test]
fn the_project_overrides_a_package_component() {
    let (p, rec) = project(
        &report("      - component: kpi.json\n"),
        &[marker("components/kpi.json", r#"{"at": "project"}"#)],
    );
    with_package(&p);
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        blocks(&rec),
        json!([{"component": "components/kpi.json", "blocks": [{"at": "project"}]}])
    );
}

#[test]
fn validate_rejects_a_bad_missing_or_ambiguous_component_name() {
    let (p, _rec) = project(
        &report(
            "      - component: header.yml\n      - component: nope.json\n      - component: twice.json\n",
        ),
        &[
            marker("components/twice.json", "{}"),
            marker("shared/twice.json", "{}"),
        ],
    );
    p.dre("validate", &[])
        .failed()
        .says("component `header.yml` must name a `.json` file")
        .says("component `nope.json` doesn't exist")
        .says("component `twice.json` matches 2 files (`components/twice.json`, `shared/twice.json`)");
}

#[test]
fn a_component_that_renders_invalid_json_fails_naming_the_file_and_the_line() {
    let (p, rec) = project(
        &report("      - component: broken.json\n"),
        &[marker(
            "components/broken.json",
            "{\n  \"text\": \"{{ results.revenue.rows[0].day }}\" \"oops\"\n}",
        )],
    );
    p.dre("run", &["daily"])
        .failed()
        .says("component `components/broken.json` didn't render to valid JSON")
        .says("line 2")
        .says("near `\"text\": \"mon\" \"oops\"`");
    assert!(all_blocks(&rec).is_empty());
}

#[test]
fn a_per_row_output_renders_its_components_for_each_row() {
    let (p, rec) = project(
        "queries: [revenue]\noutput:\n  format: message\n  for_each: {query: revenue, key: day}\n  text: hi\n  destination:\n    profile: rec\n    blocks:\n\
         \x20     - component: row.json\n        with: {day: \"{{ each.day }}\"}\n",
        &[marker(
            "components/row.json",
            r#"{"day": {{ args.day | tojson }}, "revenue": {{ each.revenue }}}"#,
        )],
    );
    p.dre("run", &["daily", "--row-concurrency", "1"]).ok();
    let mut got = all_blocks(&rec);
    got.sort_by_key(|b| b[0]["blocks"][0]["day"].as_str().unwrap().to_string());
    assert_eq!(
        got,
        vec![
            json!([{"component": "components/row.json", "blocks": [{"day": "mon", "revenue": 10}]}]),
            json!([{"component": "components/row.json", "blocks": [{"day": "tue", "revenue": 20}]}]),
        ]
    );
}

#[test]
fn preview_prints_the_rendered_blocks_and_posts_nothing() {
    let (p, rec) = project(
        &report("      - component: header.json\n        with: {text: Daily}\n      - {type: divider}\n"),
        &[HEADER],
    );
    p.dre("run", &["daily", "--preview"])
        .ok()
        .says("Blocks")
        .says("\"blocks\": [")
        .says("\"text\": \"Daily\"")
        .says("\"type\": \"divider\"");
    assert!(all_blocks(&rec).is_empty());
}

#[test]
fn a_list_of_inline_blocks_alone_is_rendered_late_and_wrapped() {
    let (p, rec) = project(
        &report("      - {type: section, text: \"{{ results.revenue.row_count }} days\"}\n"),
        &[],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        blocks(&rec),
        json!([{"blocks": [{"type": "section", "text": "2 days"}]}])
    );
}
