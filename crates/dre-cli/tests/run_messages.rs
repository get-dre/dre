//! The built-in `message` format, `when:` and skipped outputs.

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

const HEADLINE: (&str, &str) = ("reports/ops/daily/headline.sql", "select 12340.5 as revenue");
const SUMMARY: (&str, &str) = (
    "reports/ops/daily/summary.sql",
    "select 1200 as orders, 0.0412 as change, 'acme_corp*' as top",
);
const DETAIL: (&str, &str) = (
    "reports/ops/daily/detail.sql",
    "select i as id, 'r' || i as region from range(1, 13) t(i) order by i",
);

fn project(report_yml: &str, extra: &[(&str, &str)]) -> (TestProject, std::path::PathBuf) {
    let rec_dir = tempfile::tempdir().unwrap().keep();
    let rec = rec_dir.to_string_lossy().replace('\\', "/");
    let mut files = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("dependencies.yml", "plugins: [duckdb, csv, fixture]\n"),
        ("reports/ops/daily/daily.yml", report_yml),
        HEADLINE,
        SUMMARY,
        DETAIL,
    ];
    files.extend_from_slice(extra);
    let p = TestProject::new(&files, &profiles(&rec));
    p.duckdb("data.duckdb", "select 1;");
    (p, rec_dir)
}

fn results(p: &TestProject) -> Value {
    p.json("target/run/daily/default/run_results.json")
}

fn deliveries(rec: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn the_default_template_writes_one_block_per_query() {
    let (p, _) = project(
        "queries: [headline, summary, detail]\noutput: {format: message}\n",
        &[],
    );
    p.dre("run", &["daily"])
        .ok()
        .says("Message  daily: 2026-01-25 — ");
    assert_eq!(
        p.read("target/run/daily/default/daily.md"),
        "# daily: 2026-01-25\n\n\
         revenue: 12,340.50\n\n\
         orders: 1,200\nchange: 0.04\ntop: acme\\_corp\\*\n\n\
         **detail**\n- 1, r1\n- 2, r2\n- 3, r3\n- 4, r4\n- 5, r5\n- 6, r6\n- 7, r7\n- 8, r8\n- 9, r9\n- 10, r10\n+ 2 more\n"
    );
    let r = results(&p);
    let out = &r["output_results"][0];
    assert_eq!(out["format"], "message");
    assert_eq!(out["status"], "kept");
    assert_eq!(out["message"]["title"], "daily: 2026-01-25");
    assert!(
        out["message"]["text"]
            .as_str()
            .unwrap()
            .starts_with("revenue: 12,340.50")
    );
}

#[test]
fn a_custom_text_reads_results_with_filters_and_escapes_values() {
    let (p, _) = project(
        "queries: [headline, summary, detail]\noutput:\n  format: message\n  title: \"Revenue {{ run.date }}\"\n  text: |\n\
         \x20   Revenue: **{{ results.headline.value | currency('EUR') }}** ({{ results.summary.first.change | percent | signed }})\n\
         \x20   Top: {{ results.summary.first.top }}\n\
         \x20   {% for r in results.detail.rows[:2] %}- {{ r.region }}\n    {% endfor %}{{ results.detail.row_count }} regions, columns {{ results.detail.columns | join(', ') }}\n\
         \x20   Last set has {{ results.detail.sets[-1].row_count }} rows\n",
        &[],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        p.read("target/run/daily/default/daily.md"),
        "# Revenue 2026-01-25\n\n\
         Revenue: **€12,341** (+4.1%)\nTop: acme\\_corp\\*\n- r1\n- r2\n12 regions, columns id, region\nLast set has 12 rows\n"
    );
}

#[test]
fn a_text_file_and_max_rows() {
    let (p, _) = project(
        "queries: [detail]\noutput: {format: message, file: messages/daily.md, max_rows: 5}\n",
        &[(
            "messages/daily.md",
            "{{ results.detail.rows | length }} of {{ results.detail.row_count }}\n",
        )],
    );
    p.dre("run", &["daily"])
        .ok()
        .says("results.detail.rows holds the first 5 of 12 rows (`max_rows`)");
    assert_eq!(
        p.read("target/run/daily/default/daily.md"),
        "# daily: 2026-01-25\n\n5 of 12\n"
    );
}

#[test]
fn file_destinations_deliver_the_md_file() {
    let (p, rec) = project(
        "queries: [headline]\noutput:\n  format: message\n  text: \"Revenue {{ results.headline.value | number }}\"\n  destination:\n    - {profile: inbox, path: out/headline.md}\n    - {profile: rec, path: archive/headline.txt}\n  extension: txt\n",
        &[],
    );
    p.dre_env("run", &["daily"], &[("DRE_FIXTURE_FILES_ONLY", "1")])
        .ok();
    assert_eq!(
        p.read("out/headline.md"),
        "# daily: 2026-01-25\n\nRevenue 12,341\n"
    );
    let d = deliveries(&rec);
    assert_eq!(d[0]["files"][0]["remote"], "archive/headline.txt");
    // The local file is named after the first destination path.
    assert!(p.path("target/run/daily/default/headline.md").exists());
    assert_eq!(results(&p)["output_results"][0]["status"], "delivered");
}

#[test]
fn preview_prints_the_message_and_delivers_nothing() {
    let (p, rec) = project(
        "queries: [headline]\noutput:\n  name: headline\n  format: message\n  text: \"Revenue {{ results.headline.value | number }}\"\n  when: \"results.headline.value > 0\"\n  destination: {profile: rec}\n",
        &[],
    );
    p.dre("run", &["daily", "--preview", "5"])
        .ok()
        .says("Message  daily: 2026-01-25 (output `headline`)")
        .says("Revenue 12,341")
        .says("numbers come from a sample of at most 5 rows per query")
        .says("`when` passed");
    assert!(deliveries(&rec).is_empty());
}

#[test]
fn when_false_skips_the_output_and_the_binding_succeeds() {
    let (p, rec) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: rows\n    queries: [detail]\n    when: \"results.detail.row_count > 100\"\n    destination: {profile: rec, path: rows.csv}\n\
         \x20 - name: alert\n    format: message\n    queries: [headline]\n    when: \"results.headline.value < 0\"\n    destination: {profile: rec}\n\
         \x20 - name: always\n    format: message\n    queries: [headline]\n    text: \"{% if results.headline.value < 0 %}Down{% endif %}\"\n",
        &[],
    );
    p.dre("run", &["daily"])
        .ok()
        .says("Skipped  output `rows`: `when` is false")
        .says("Skipped  output `alert`: `when` is false")
        .says("Skipped  output `always`: the message is empty");
    assert!(deliveries(&rec).is_empty());
    let r = results(&p);
    assert_eq!(r["status"], "success");
    let outs = r["output_results"].as_array().unwrap();
    assert_eq!(outs[0]["status"], "skipped");
    assert_eq!(outs[0]["when"], false);
    assert_eq!(outs[1]["status"], "skipped");
    assert_eq!(outs[2]["status"], "skipped");
}

#[test]
fn when_true_delivers() {
    let (p, rec) = project(
        "queries: [headline]\noutput:\n  format: message\n  when: \"results.headline.value > 1000\"\n  text: Up\n  destination: {profile: rec}\n",
        &[],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(deliveries(&rec).len(), 1);
    assert_eq!(results(&p)["output_results"][0]["when"], true);
}

#[test]
fn validate_checks_message_options_and_names() {
    let (p, _) = project(
        "queries: [headline]\noutput:\n  format: message\n  text: \"{{ results.headlin.value }}\"\n  when: \"results.nope.value > 1\"\n  max_rows: 0\n  colour: red\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("`text` reads `results.headlin`, but `headlin` isn't one of this output's queries (headline)")
        .says("`when` reads `results.nope`")
        .says("`max_rows` must be a positive whole number")
        .says("`colour` isn't an option of the `message` format");
    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [headline]\noutput: {format: message, text: hi, file: m.md}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("use either `text:` or `file:`, not both")
        .says("message file `m.md` doesn't exist");
    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [headline]\noutput: {format: message, text: \"{{ results.headline.value \"}\n",
    );
    p.dre("validate", &[]).failed().says("`text` doesn't compile");
}

#[test]
fn a_message_goes_to_a_message_destination_as_a_message() {
    let (p, rec) = project(
        "queries: [headline]\noutput:\n  format: message\n  title: Daily\n  text: \"Revenue **{{ results.headline.value | number }}**\"\n  destination:\n    - {profile: rec, subject: Hi}\n    - {profile: inbox, path: out/daily.md}\n",
        &[],
    );
    p.dre("run", &["daily"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["message"]["title"], "Daily");
    assert_eq!(d[0]["message"]["text"], "Revenue **12,341**");
    assert_eq!(d[0]["message"]["html"], Value::Null);
    assert!(d[0]["message"]["path"].as_str().unwrap().ends_with("daily.md"));
    assert_eq!(d[0]["files"], serde_json::json!([]));
    assert_eq!(d[0]["options"]["subject"], "Hi");
    assert_eq!(p.read("out/daily.md"), "# Daily\n\nRevenue **12,341**\n");
    let r = results(&p);
    assert_eq!(
        r["output_results"][0]["deliveries"][0]["location"],
        "fixture:message"
    );
}

#[test]
fn a_file_only_destination_gets_the_md_file() {
    let (p, rec) = project(
        "queries: [headline]\noutput:\n  format: message\n  text: Hello\n  destination: {profile: rec, path: archive/daily.md}\n",
        &[],
    );
    p.dre_env("run", &["daily"], &[("DRE_FIXTURE_FILES_ONLY", "1")])
        .ok();
    let d = deliveries(&rec);
    assert_eq!(d[0]["message"], Value::Null);
    assert_eq!(d[0]["files"][0]["remote"], "archive/daily.md");
    assert_eq!(d[0]["files"][0]["content"], "# daily: 2026-01-25\n\nHello\n");
}

#[test]
fn a_file_output_to_a_message_only_destination_is_refused() {
    let (p, _) = project(
        "queries: [headline]\noutput: {format: csv, destination: {profile: rec}}\n",
        &[],
    );
    p.dre_env("validate", &[], &[("DRE_FIXTURE_MESSAGE_ONLY", "1")])
        .failed()
        .says("`fixture` only takes messages, but this output is `csv`")
        .says("link it from a message output with `outputs.<name>.location`");
}

#[test]
fn preview_shows_each_destinations_limit() {
    let (p, _) = project(
        "queries: [headline]\noutput:\n  format: message\n  text: Hello\n  destination:\n    - {profile: rec}\n    - {profile: inbox, path: out/x.md}\n",
        &[],
    );
    p.dre("run", &["daily", "--preview"])
        .ok()
        .says("rec (fixture): 5 of 3,000 characters")
        .says("inbox (local): delivers the .md file");
}

#[test]
fn a_message_links_and_attaches_the_file_outputs() {
    let (p, rec) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: headline\n    format: message\n    queries: [headline]\n    text: \"Full report: {{ outputs.rows.location }} ({{ outputs.rows.status }})\"\n    destination: {profile: rec, attach: [rows]}\n\
         \x20 - name: rows\n    queries: [detail]\n    destination: {profile: inbox, path: out/rows_a.csv}\n",
        &[],
    );
    p.dre("run", &["daily"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    let loc = p.path("out/rows_a.csv").to_string_lossy().replace('_', "\\_");
    assert_eq!(d[0]["message"]["text"], format!("Full report: {loc} (delivered)"));
    assert_eq!(d[0]["files"].as_array().unwrap().len(), 1);
    assert!(
        d[0]["files"][0]["local"]
            .as_str()
            .unwrap()
            .ends_with("rows_a.csv")
    );
    // The file output was delivered first, though declared second.
    let r = results(&p);
    assert_eq!(r["deliveries"][0]["profile"], "inbox");
}

#[test]
fn validate_checks_outputs_and_attach() {
    let (p, _) = project(
        "queries: [headline, detail]\noutput:\n\
         \x20 - name: headline\n    format: message\n    queries: [headline]\n    text: \"{{ outputs.rowz.location }}\"\n    destination: {profile: rec, attach: [nope, headline, note]}\n\
         \x20 - name: note\n    format: message\n    queries: [headline]\n\
         \x20 - name: rows\n    queries: [detail]\n    destination: {profile: inbox, path: out/x.csv, attach: [headline]}\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("reads `outputs.rowz`, but no output of this report is named `rowz`")
        .says("`attach: nope`: no output of this report is named `nope`")
        .says("`attach: headline`: `headline` is this output itself")
        .says("`attach: note`: `note` is a message")
        .says("`attach: headline`: `attach` only applies to a message output")
        .says("`attach:` needs a destination that takes messages and files, but `local` takes files only");
}
