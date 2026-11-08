//! Per-row messages, their attachments, and masking of per-row values.

mod common;

use common::{TestProject, test_plugins};
use serde_json::Value;

fn profiles(rec: &str) -> String {
    format!(
        "connections:\n  warehouse:\n    targets:\n      dev: {{type: duckdb, path: data.duckdb}}\n\
         destinations:\n\
         \x20 outbox:\n    targets:\n      dev: {{type: local}}\n\
         \x20 mail:\n    targets:\n      dev: {{type: fixture, dir: \"{rec}\"}}\n"
    )
}

// The members live in the database, so no address is in the SQL (which `-v` logs).
const MEMBERS: (&str, &str) = (
    "reports/m/statements/members.sql",
    "select * from members order by member_id",
);
const HOLDINGS: (&str, &str) = (
    "reports/m/statements/holdings.sql",
    "select * from (values (101, 'Growth', 1000.5), (102, 'Cash', 20.0)) t(member_id, fund, amount)",
);

fn project(report: &str, extra: &[(&str, &str)]) -> (TestProject, std::path::PathBuf) {
    let rec = tempfile::tempdir().unwrap().keep();
    let mut files = vec![
        ("dre_project.yml", "name: fund\ndefault_profile: warehouse\n"),
        ("dependencies.yml", "plugins: [duckdb, csv, fixture, document]\n"),
        ("reports/m/statements/statements.yml", report),
        MEMBERS,
        HOLDINGS,
    ];
    files.extend_from_slice(extra);
    let mut p = TestProject::new(&files, &profiles(&rec.to_string_lossy().replace('\\', "/")));
    let mut bins = common::ALL_PLUGINS.to_vec();
    bins.push("dre-plugin-document");
    p.plugins = test_plugins(&bins);
    p.duckdb(
        "data.duckdb",
        "create table members as select * from (values (101, 'Ann Lee', 'ann.lee@example.com'), (102, 'Bob Ray', 'bob.ray@example.com')) t(member_id, name, email);",
    );
    (p, rec)
}

fn deliveries(rec: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const STATEMENT: &str = "  - name: statement\n    format: csv\n    queries: [members, holdings]\n    for_each: {query: members, key: member_id}\n";

#[test]
fn each_member_gets_a_message_with_only_their_statement() {
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}\
             \x20 - name: email\n    format: message\n    for_each: {{query: members, key: member_id}}\n    title: \"Your statement, {{{{ each.name }}}}\"\n    text: \"Hi {{{{ each.name }}}}, your balance is {{{{ results.holdings.first.amount }}}}.\"\n\
             \x20   destination: {{profile: mail, to: \"{{{{ each.email }}}}\", attach: statement}}\n"
        ),
        &[],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"])
        .ok()
        .says("2 succeeded");
    let mut d = deliveries(&rec);
    d.sort_by_key(|x| x["options"]["to"].as_str().unwrap().to_string());
    assert_eq!(d.len(), 2);
    assert_eq!(d[0]["options"]["to"], "ann.lee@example.com");
    assert_eq!(d[0]["message"]["title"], "Your statement, Ann Lee");
    assert!(d[0]["message"]["text"].as_str().unwrap().contains("1000.5"));
    let files = d[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert!(
        files[0]["local"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("statement/101.csv")
    );
    assert!(
        files[0]["content"].as_str().unwrap().contains("Growth")
            && !files[0]["content"].as_str().unwrap().contains("Cash")
    );
}

#[test]
fn a_failed_attachment_skips_that_rows_message() {
    // Ann's statement fails to deliver (its path is a folder), so it's not attached: her message
    // is skipped, Bob's goes out.
    let (p, rec) = project(
        "queries: [members, holdings]\noutput:\n  - name: statement\n    format: csv\n    queries: [members, holdings]\n    for_each: {query: members, key: member_id}\n    destination: {profile: outbox, path: \"out/{{ each.member_id }}.csv\"}\n\
             \x20 - name: email\n    format: message\n    for_each: {query: members, key: member_id}\n    text: hi\n    destination: {profile: mail, to: \"{{ each.email }}\", attach: statement}\n",
        &[("out/101.csv/keep", "")],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"]).failed();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["options"]["to"], "bob.ray@example.com");
    let rows = p.read("target/run/statements/default/email.rows.jsonl");
    assert!(rows.contains("\"reason\":\"attachment_failed\""), "{rows}");
}

#[test]
fn a_shared_file_is_attached_to_every_row() {
    let (p, rec) = project(
        "queries: [members, holdings]\noutput:\n  - name: summary\n    format: csv\n    queries: [holdings]\n\
             \x20 - name: email\n    format: message\n    for_each: {query: members, key: member_id}\n    text: hi\n    destination: {profile: mail, to: \"{{ each.email }}\", attach: summary}\n",
        &[],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 2);
    assert!(
        d.iter()
            .all(|x| x["files"][0]["local"].as_str().unwrap().ends_with("summary.csv"))
    );
}

#[test]
fn per_row_values_are_masked_in_logs_and_results() {
    let report = format!(
        "queries: [members, holdings]\noutput:\n{STATEMENT}    destination: {{profile: mail, to: \"{{{{ each.email }}}}\", subject: \"For {{{{ each.name }}}}\"}}\n"
    );
    let (p, rec) = project(&report, &[]);
    // One row at a time: the fixture appends its records from each worker's process.
    let run = p.dre("run", &["statements", "-v", "--row-concurrency", "1"]);
    run.ok();
    let run_dir = p.path("target/run/statements/default");
    let mut seen = String::new();
    for e in walkdir(&run_dir) {
        let name = e.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with(".json") || name.ends_with(".jsonl") || name == "dre.log" {
            let text = std::fs::read_to_string(&e).unwrap();
            assert!(
                !text.contains("ann.lee@example.com"),
                "leaked in {}: {text}",
                e.display()
            );
            seen.push_str(&text);
        }
    }
    seen.push_str(&run.stdout);
    seen.push_str(&run.stderr);
    for secret in ["ann.lee@example.com", "bob.ray@example.com"] {
        assert!(!seen.contains(secret), "`{secret}` leaked");
    }
    assert!(seen.contains("a***@example.com"), "masked form missing");
    // The delivery itself has the real address; the files are the data, unmasked.
    assert_eq!(deliveries(&rec).len(), 2);
    // Opted out: shown as they are.
    let (p, _) = project(
        &report.replace("key: member_id}", "key: member_id, mask_row_values: false}"),
        &[],
    );
    p.dre("run", &["statements"]).ok();
    assert!(
        p.read("target/run/statements/default/statement.rows.jsonl")
            .contains("ann.lee@example.com")
    );
}

#[test]
fn a_document_per_row_reads_each() {
    let (p, _) = project(
        "queries: [members, holdings]\noutput:\n  name: statement\n  format: html\n  for_each: {query: members, key: member_id}\n  template: {file: statement.html}\n",
        &[(
            "reports/m/statements/statement.html",
            "<h1>{{ each.name }}</h1>{{ table(results.holdings) }}<p>{{ results | length }} section(s)</p>",
        )],
    );
    p.dre("run", &["statements"]).ok();
    let html = p.read("target/run/statements/default/statement/102.html");
    assert!(
        html.contains("<h1>Bob Ray</h1>") && html.contains("Cash") && !html.contains("Growth"),
        "{html}"
    );
    // The driver isn't a section.
    assert!(html.contains("<p>1 section(s)</p>"), "{html}");
}

fn walkdir(d: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn attaching_a_per_row_output_needs_the_same_driver_and_key() {
    let (p, _) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}\
             \x20 - name: email\n    format: message\n    for_each: {{query: holdings, key: fund}}\n    text: hi\n    destination: {{profile: mail, to: x@example.com, attach: statement}}\n"
        ),
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("give them the same `for_each` driver and key");
    let (p, _) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}\
             \x20 - name: email\n    format: message\n    text: hi\n    destination: {{profile: mail, to: x@example.com, attach: statement}}\n"
        ),
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("makes a file per row; attach it from a message with the same `for_each`");
}

fn run_id(p: &TestProject) -> String {
    p.json("target/run/statements/default/run_results.json")["run_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn held_deliveries_are_released_later_byte_for_byte_and_once() {
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}    destination: {{profile: mail, to: \"{{{{ each.email }}}}\", hold: true}}\n"
        ),
        &[],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"])
        .ok()
        .says("2 held");
    assert!(deliveries(&rec).is_empty());
    let r = p.json("target/run/statements/default/run_results.json");
    assert_eq!(r["status"], "succeeded");
    assert_eq!(r["output_results"][0]["status"], "held");
    assert_eq!(r["output_results"][0]["for_each"]["held"], 2);
    let id = run_id(&p);
    // A tampered file is refused; the other row goes out.
    let ann = p.path("target/run/statements/default/statement/101.csv");
    std::fs::write(&ann, "member_id,fund,amount\r\n101,Growth,9999999\r\n").unwrap();
    p.dre("release", &["statements", "--run", &id, "--by", "checker"])
        .failed()
        .says("refused")
        .lacks("Progress");
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["options"]["to"], "bob.ray@example.com");
    assert!(d[0]["files"][0]["content"].as_str().unwrap().contains("102,Cash"));
    // Releasing again sends nothing new (Ann's stays refused until re-rendered).
    p.dre("release", &["statements", "--run", &id])
        .failed()
        .says("1 already released");
    assert_eq!(deliveries(&rec).len(), 1);
}

#[test]
fn release_by_keys_and_a_plain_output() {
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}    destination: {{profile: mail, to: \"{{{{ each.email }}}}\"}}\n  - name: summary\n    format: csv\n    queries: [holdings]\n    destination: {{profile: mail, to: boss@example.com}}\n"
        ),
        &[],
    );
    // `--hold` holds every destination, of every output.
    p.dre("run", &["statements", "--hold", "--row-concurrency", "1"])
        .ok();
    assert!(deliveries(&rec).is_empty());
    let id = run_id(&p);
    p.dre("release", &["statements", "--run", &id, "--only-keys", "102"])
        .ok()
        .says("1 delivered");
    assert_eq!(deliveries(&rec).len(), 1);
    p.dre("release", &["statements", "--run", &id, "--exclude-keys", "101"])
        .ok();
    // The summary (not per row) and nothing for 101.
    let d = deliveries(&rec);
    assert_eq!(d.len(), 2);
    assert!(d.iter().any(|x| x["options"]["to"] == "boss@example.com"));
    p.dre("release", &["statements", "--run", &id])
        .ok()
        .says("1 delivered");
    assert_eq!(deliveries(&rec).len(), 3);
    // The release is its own run, recording who and what.
    let runs = p.path("target/run/statements/default/runs");
    let releases: Vec<serde_json::Value> = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("run_results.json")).ok())
        .map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap())
        .filter(|r| r["release_of"] == id.as_str())
        .collect();
    assert_eq!(releases.len(), 3);
    assert!(releases.iter().all(|r| r["released_by"].is_string()));
    for release in &releases {
        assert_eq!(release["status"], "succeeded");
        assert_eq!(release["schema_version"], "dre/run-results/v2");
    }
    p.dre("release", &["statements", "--run", "20990101T000000Z-zzzz"])
        .failed()
        .says("has no run");
}

#[test]
fn a_tampered_attachment_is_refused_at_release() {
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}\
             \x20 - name: email\n    format: message\n    for_each: {{query: members, key: member_id}}\n    text: hi\n    destination: {{profile: mail, to: \"{{{{ each.email }}}}\", attach: statement, hold: true}}\n"
        ),
        &[],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"]).ok();
    let id = run_id(&p);
    std::fs::write(
        p.path("target/run/statements/default/statement/101.csv"),
        "changed",
    )
    .unwrap();
    p.dre("release", &["statements", "--run", &id])
        .failed()
        .says("refused");
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["options"]["to"], "bob.ray@example.com");
}

#[test]
fn secret_columns_are_masked_everywhere_dre_reports_but_not_in_what_it_delivers() {
    let secret = |p: &TestProject| {
        p.duckdb(
            "data.duckdb",
            "create or replace table members as select * from (values (101, 'Ann Lee', 'ann.lee@example.com', 'Ann-pw-7731'), (102, 'Bob Ray', 'bob.ray@example.com', 'Bob-pw-2290')) t(member_id, name, email, pdf_password);",
        );
    };
    let report = "queries:\n  - {query: members, secret_columns: [pdf_password]}\n  - holdings\n\
        output:\n\
        \x20 - name: statement\n    format: pdf\n    queries: [members, holdings]\n    for_each: {query: members, key: member_id, mask_row_values: false}\n    password: \"{{ each.pdf_password }}\"\n\
        \x20 - name: password_mail\n    format: message\n    for_each: {query: members, key: member_id, mask_row_values: false}\n    title: \"Password for {{ each.name }}\"\n    text: \"Your password is {{ each.pdf_password }}.\"\n\
        \x20   destination: {profile: mail, to: \"{{ each.email }}\", subject: \"pw {{ each.pdf_password }}\"}\n";
    let (p, rec) = project(report, &[]);
    secret(&p);
    let run = p.dre(
        "run",
        &[
            "statements",
            "-v",
            "--row-concurrency",
            "1",
            "--output-json",
            "saved",
        ],
    );
    run.ok();
    let mut seen = format!("{}{}", run.stdout, run.stderr);
    for dir in ["target/run/statements/default", "saved", "logs"] {
        for f in walkdir(&p.path(dir)) {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            if [".json", ".jsonl", ".log"].iter().any(|x| name.ends_with(x)) {
                seen.push_str(&std::fs::read_to_string(&f).unwrap());
            }
        }
    }
    let preview = p.dre("run", &["statements", "--preview", "2"]);
    seen.push_str(&preview.stdout);
    seen.push_str(&preview.stderr);
    for pw in ["Ann-pw-7731", "Bob-pw-2290"] {
        assert!(!seen.contains(pw), "`{pw}` leaked:\n{seen}");
    }
    // Masked, while the row values that aren't secret show (mask_row_values: false).
    assert!(
        seen.contains("*****") && seen.contains("ann.lee@example.com"),
        "{seen}"
    );
    let saved = p.read("saved/statements/default/members.json");
    assert!(saved.contains("*****") && saved.contains("Ann Lee"), "{saved}");
    // The delivered messages and their options have the real password; the PDFs are encrypted.
    let d = deliveries(&rec);
    assert_eq!(d.len(), 2);
    assert!(
        d.iter()
            .any(|x| x["message"]["text"] == "Your password is Ann-pw-7731."),
        "{d:?}"
    );
    assert!(
        d.iter().any(|x| x["options"]["subject"] == "pw Bob-pw-2290"),
        "{d:?}"
    );
    let pdf = std::fs::read(p.path("target/run/statements/default/statement/101.pdf")).unwrap();
    assert!(String::from_utf8_lossy(&pdf).contains("/Encrypt"));
}

#[test]
fn validate_warns_when_a_password_column_isnt_secret() {
    let report = "queries: [members, holdings]\noutput:\n\
        \x20 - name: statement\n    format: pdf\n    queries: [members, holdings]\n    for_each: {query: members, key: member_id}\n    password: \"{{ each.email }}\"\n";
    let (p, _) = project(report, &[]);
    p.dre("validate", &[])
        .ok()
        .says("`password` reads `each.email`, which isn't in `secret_columns` of query `members`");
    let (p, _) = project(
        &report.replace(
            "queries: [members, holdings]\noutput",
            "queries: [{query: members, secret_columns: [email]}, holdings]\noutput",
        ),
        &[],
    );
    let v = p.dre("validate", &[]);
    v.ok();
    assert!(!v.stdout.contains("secret_columns"), "{}", v.stdout);
}

#[test]
fn validate_warns_about_passwords_dre_would_show() {
    let base = "queries: [members, holdings]\noutput:\n";
    let cases = [
        (
            "  - name: statement\n    format: pdf\n    queries: [holdings]\n    password: Literal-pw-1\n",
            "`password` is written in the YAML",
        ),
        (
            "  - name: statement\n    format: pdf\n    queries: [holdings]\n    password: \"{{ var('pw') }}\"\n",
            "`password` reads `var()`",
        ),
        (
            "  - name: statement\n    format: pdf\n    queries: [holdings]\n    password: \"{{ results.members.first.email }}\"\n",
            "`password` reads `results.members`, but `email` isn't in `secret_columns` of query `members`",
        ),
        (
            "  - name: pin_mail\n    format: message\n    for_each: {query: members, key: member_id}\n    text: \"PIN {{ each.pin }}\"\n    destination: {profile: mail, to: \"{{ each.email }}\"}\n",
            "`each.pin` looks like a password but isn't in `secret_columns` of query `members`",
        ),
    ];
    for (output, want) in cases {
        let (p, _) = project(&format!("{base}{output}"), &[]);
        p.dre("validate", &[]).ok().says(want);
    }
    // Marked secret: no warning.
    let (p, _) = project(
        "queries: [{query: members, secret_columns: [email]}, holdings]\noutput:\n  - name: statement\n    format: pdf\n    queries: [holdings]\n    password: \"{{ results.members.first.email }}\"\n",
        &[],
    );
    let v = p.dre("validate", &[]);
    v.ok();
    assert!(!v.stdout.contains("secret_columns"), "{}", v.stdout);
}

#[test]
fn short_secret_values_are_masked_where_they_stand_alone() {
    let (p, rec) = project(
        "queries: [{query: members, secret_columns: [pin]}, holdings]\noutput:\n  - name: pin_mail\n    format: message\n    queries: [members]\n    title: \"PIN {{ results.members.first.pin }}\"\n    text: \"Your PIN is {{ results.members.first.pin }}, from 2073.\"\n    destination: {profile: mail, to: a@example.com}\n",
        &[],
    );
    p.duckdb(
        "data.duckdb",
        "create or replace table members as select * from (values (101, 'Ann Lee', 'ann.lee@example.com', '73')) t(member_id, name, email, pin);",
    );
    let run = p.dre("run", &["statements", "-v"]);
    run.ok();
    let results = p.read("target/run/statements/default/run_results.json");
    let seen = format!("{}{}{results}", run.stdout, run.stderr);
    assert!(!seen.contains("PIN 73") && !seen.contains("is 73,"), "{seen}");
    // Masked where it stands alone; the year that contains it stays readable.
    assert!(results.contains("Your PIN is *****, from 2073."), "{results}");
    // Delivered as is.
    let d = deliveries(&rec);
    assert!(
        d.iter()
            .any(|x| x["message"]["text"] == "Your PIN is 73, from 2073."),
        "{d:?}"
    );
}

// Two composite keys that print the same (`acme_us_1`): each is its own row everywhere.
const ORGS: &str = "queries: [orgs, books]\noutput:\n\
    \x20 - name: book\n    format: csv\n    queries: [orgs, books]\n    for_each: {query: orgs, key: [org, region]}\n    file_name: \"{{ each.name }}.csv\"\n    destination: {profile: outbox, path: \"out/{{ each.name }}.csv\"}\n\
    \x20 - name: email\n    format: message\n    for_each: {query: orgs, key: [org, region]}\n    file_name: \"{{ each.name }}.md\"\n    text: hi\n    destination: {profile: mail, to: \"{{ each.email }}\", attach: book}\n";

fn orgs_project(extra: &[(&str, &str)]) -> (TestProject, std::path::PathBuf) {
    let mut files = vec![
        (
            "reports/m/statements/orgs.sql",
            "select * from (values ('acme_us', '1', 'Ann', 'ann@example.com'), ('acme', 'us_1', 'Bob', 'bob@example.com')) t(org, region, name, email)",
        ),
        (
            "reports/m/statements/books.sql",
            "select * from (values ('acme_us', '1', 'Growth'), ('acme', 'us_1', 'Cash')) t(org, region, fund)",
        ),
    ];
    files.extend_from_slice(extra);
    project(ORGS, &files)
}

#[test]
fn composite_keys_that_print_alike_keep_their_own_attachments_and_retries() {
    // Bob's book fails to deliver (its path is a folder): his message is skipped, never sent
    // with Ann's book.
    let (p, rec) = orgs_project(&[("out/Bob.csv/keep", "")]);
    p.dre("run", &["statements", "--row-concurrency", "1"]).failed();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["options"]["to"], "ann@example.com");
    let ann = d[0]["files"][0]["content"].as_str().unwrap();
    assert!(ann.contains("Growth") && !ann.contains("Cash"), "{ann}");
    // The retry redoes Bob's row, though Ann's (which prints the same key) succeeded.
    std::fs::remove_dir_all(p.path("out/Bob.csv")).unwrap();
    let first = run_id(&p);
    p.dre(
        "run",
        &["statements", "--retry-failed", &first, "--keep-runs", "5"],
    )
    .ok();
    assert!(p.read("out/Bob.csv").contains("Cash"));
    let d = deliveries(&rec);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!(d[1]["options"]["to"], "bob@example.com");
    let bob = d[1]["files"][0]["content"].as_str().unwrap();
    assert!(bob.contains("Cash") && !bob.contains("Growth"), "{bob}");
}

#[test]
fn a_separator_in_a_key_value_never_matches_another_rows_details() {
    // The driver's (`alpha<US>beta`, `gamma`) and the detail's (`alpha`, `beta<US>gamma`) are
    // different tuples: the detail row is in no copy.
    let (p, _rec) = orgs_project(&[
        (
            "reports/m/statements/orgs.sql",
            "select 'alpha' || chr(31) || 'beta' as org, 'gamma' as region, 'Ann' as name, 'ann@example.com' as email",
        ),
        (
            "reports/m/statements/books.sql",
            "select 'alpha' as org, 'beta' || chr(31) || 'gamma' as region, 'Lost' as fund",
        ),
    ]);
    p.dre("run", &["statements", "--no-deliver"])
        .says("isn't in the driver");
}

#[test]
fn two_unnamed_per_row_outputs_keep_their_own_journals_and_held_deliveries() {
    let (p, rec) = project(
        "queries: [members, holdings]\noutput:\n\
         \x20 - format: csv\n    queries: [members, holdings]\n    for_each: {query: members, key: member_id}\n    destination: {profile: mail, to: \"{{ each.email }}\", hold: true}\n\
         \x20 - format: message\n    for_each: {query: members, key: member_id}\n    text: hi\n    destination: {profile: mail, to: \"{{ each.email }}\", hold: true}\n",
        &[],
    );
    p.dre("run", &["statements", "--row-concurrency", "1"]).ok();
    for journal in ["statements-csv", "statements-md"] {
        let rows = p.read(&format!("target/run/statements/default/{journal}.rows.jsonl"));
        assert_eq!(rows.lines().count(), 2, "{journal}: {rows}");
    }
    let held = p.json("target/run/statements/default/held.json");
    let mut ids: Vec<&str> = held["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4, "{ids:?}");
    let id = run_id(&p);
    p.dre("release", &["statements", "--run", &id])
        .ok()
        .says("4 delivered");
    assert_eq!(deliveries(&rec).len(), 4);
}

#[test]
fn a_release_of_a_busy_binding_exits_75_and_sends_nothing() {
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}    destination: {{profile: mail, to: \"{{{{ each.email }}}}\", hold: true}}\n"
        ),
        &[],
    );
    p.dre("run", &["statements"]).ok();
    let id = run_id(&p);
    let runs = dre_core::runs::BindingRuns::new(&p.path("target/run/statements/default"));
    let guard = runs.lock("busy", chrono::Utc::now(), "release-test").unwrap();
    let out = p.dre_env(
        "release",
        &["statements", "--run", &id],
        &[("DRE_HOST_NAME", "release-test")],
    );
    assert_eq!(out.code, 75, "{out:?}");
    out.says("is busy");
    assert!(deliveries(&rec).is_empty());
    drop(guard);
}

#[cfg(unix)]
#[test]
fn a_terminated_release_is_cancelled_and_exits_143() {
    use std::time::{Duration, Instant};
    let (p, rec) = project(
        &format!(
            "queries: [members, holdings]\noutput:\n{STATEMENT}    destination: {{profile: mail, to: \"{{{{ each.email }}}}\", hold: true}}\n"
        ),
        &[],
    );
    p.dre("run", &["statements"]).ok();
    let id = run_id(&p);
    // One a minute: the second waits for its turn when the signal comes.
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_dre"))
        .args([
            "release",
            "statements",
            "--run",
            &id,
            "--rate",
            "1/minute",
            "--project-dir",
        ])
        .arg(p.root())
        .arg("--profiles-dir")
        .arg(p.dir.path().join("profiles"))
        .env("DRE_PLUGINS_DIR", &p.plugins)
        .env("HOME", p.dir.path().join("home"))
        .env_remove("DRE_PROFILES_DIR")
        .env_remove("DRE_TARGET")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while deliveries(&rec).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(deliveries(&rec).len(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{text}");
    assert_eq!(deliveries(&rec).len(), 1);
    let runs = p.path("target/run/statements/default/runs");
    let release: serde_json::Value = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("run_results.json")).ok())
        .map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap())
        .find(|r| r["release_of"] == id.as_str())
        .unwrap();
    assert_eq!(release["status"], "cancelled", "{release}");
    assert_eq!(release["released"].as_array().unwrap().len(), 1, "{release}");
    // The next release sends only the one left.
    p.dre("release", &["statements", "--run", &id])
        .ok()
        .says("1 already released");
    assert_eq!(deliveries(&rec).len(), 2);
}
