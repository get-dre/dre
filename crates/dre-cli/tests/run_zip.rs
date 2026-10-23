//! `zip:` on destination entries: packs through `attach:`, passwords, per-row zips, names,
//! clashes, and held zips released byte for byte.

mod common;

use std::io::Read;
use std::path::Path;

use common::{TestProject, test_plugins};
use serde_json::Value;

fn project(report: &str) -> (TestProject, std::path::PathBuf) {
    let rec = tempfile::tempdir().unwrap().keep();
    let profiles = format!(
        "connections:\n  warehouse:\n    targets:\n      dev: {{type: duckdb, path: data.duckdb}}\n\
         destinations:\n\
         \x20 outbox:\n    targets:\n      dev: {{type: local}}\n\
         \x20 mail:\n    targets:\n      dev: {{type: fixture, dir: \"{}\"}}\n",
        rec.to_string_lossy().replace('\\', "/")
    );
    let mut p = TestProject::new(
        &[
            ("dre_project.yml", "name: packs\ndefault_profile: warehouse\n"),
            ("dependencies.yml", "plugins: [duckdb, csv, xlsx, fixture]\n"),
            ("reports/c/pack/pack.yml", report),
            (
                "reports/c/pack/summary.sql",
                "select 'acme' as client, 12.5 as total",
            ),
            ("reports/c/pack/fees.sql", "select 'wire' as fee, 3 as amount"),
            (
                "reports/c/pack/members.sql",
                "select * from members order by member_id",
            ),
            (
                "reports/c/pack/balances.sql",
                "select member_id, 10 * member_id as balance from members",
            ),
        ],
        &profiles,
    );
    p.plugins = test_plugins(common::ALL_PLUGINS);
    p.duckdb(
        "data.duckdb",
        "create table members as select * from (values (101, 'Ann-zip-7731'), (102, 'Bob-zip-2290')) t(member_id, zip_password);",
    );
    (p, rec)
}

fn entries(zip: &Path) -> Vec<String> {
    let a = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
    let mut v: Vec<String> = a.file_names().map(|n| n.unwrap().to_string()).collect();
    v.sort();
    v
}

fn read(zip: &Path, name: &str, pw: Option<&str>) -> Result<String, String> {
    let mut a = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).map_err(|e| e.to_string())?;
    let mut s = String::new();
    match pw {
        Some(pw) => a.by_name_decrypt(name, pw.as_bytes()),
        None => a.by_name(name),
    }
    .map_err(|e| e.to_string())?
    .read_to_string(&mut s)
    .map_err(|e| e.to_string())?;
    Ok(s)
}

fn run_id(p: &TestProject) -> String {
    p.json("target/run/pack/default/run_results.json")["run_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn deliveries(rec: &Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const PACK: &str = "queries: [summary, fees, members]\noutput:\n\
    \x20 - name: workbook\n    format: xlsx\n    queries: [summary]\n\
    \x20 - name: detail\n    format: csv\n    queries: [summary, fees]\n";

#[test]
fn a_pack_zips_the_output_and_its_attached_outputs_with_an_aes_password() {
    let (p, _) = project(&format!(
        "{PACK}    destination:\n      profile: outbox\n      path: \"out/pack-{{{{ run.date.yyyymmdd }}}}.zip\"\n      attach: [workbook]\n      zip: {{password: \"{{{{ env_var('PACK_PW') }}}}\"}}\n"
    ));
    let run = p.dre_env("run", &["pack", "-v"], &[("PACK_PW", "Pack-pw-5150")]);
    run.ok();
    let zip = p.path("out/pack-20260125.zip");
    assert_eq!(
        entries(&zip),
        vec!["detail_fees.csv", "detail_summary.csv", "workbook.xlsx"]
    );
    assert!(
        read(&zip, "detail_fees.csv", Some("Pack-pw-5150"))
            .unwrap()
            .contains("wire")
    );
    assert!(read(&zip, "detail_fees.csv", None).is_err());
    assert!(read(&zip, "detail_fees.csv", Some("nope")).is_err());
    // The password read through env_var() never shows.
    let mut seen = format!("{}{}", run.stdout, run.stderr);
    seen.push_str(&p.read("target/run/pack/default/run_results.json"));
    assert!(!seen.contains("Pack-pw-5150"), "{seen}");
    // Loose files aren't delivered.
    assert!(!p.path("out/detail_fees.csv").exists());
}

#[test]
fn zip_true_names_the_zip_after_the_output_and_zipcrypto_warns() {
    let (p, rec) = project(&format!(
        "{PACK}    destination:\n      - {{profile: mail, to: a@example.com, zip: true}}\n      - {{profile: outbox, path: out/legacy.zip, zip: {{password: Old-pw-1, encryption: zipcrypto}}}}\n"
    ));
    p.dre("validate", &[])
        .ok()
        .says("`encryption: zipcrypto` is weak");
    p.dre("run", &["pack"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    let files = d[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let local = files[0]["local"].as_str().unwrap();
    assert!(local.replace('\\', "/").ends_with("/detail.zip"), "{local}");
    assert_eq!(
        entries(Path::new(local)),
        vec!["detail_fees.csv", "detail_summary.csv"]
    );
    let legacy = p.path("out/legacy.zip");
    assert!(
        read(&legacy, "detail_fees.csv", Some("Old-pw-1"))
            .unwrap()
            .contains("wire")
    );
    assert!(read(&legacy, "detail_fees.csv", None).is_err());
}

#[test]
fn entries_with_the_same_zip_share_one() {
    let (p, _) = project(&format!(
        "{PACK}    destination:\n      - {{profile: outbox, path: out/a/pack.zip, zip: true}}\n      - {{profile: outbox, path: out/b/pack.zip, zip: true}}\n"
    ));
    p.dre("run", &["pack"]).ok();
    assert_eq!(
        std::fs::read(p.path("out/a/pack.zip")).unwrap(),
        std::fs::read(p.path("out/b/pack.zip")).unwrap()
    );
    let zips = std::fs::read_dir(p.path("target/run/pack/default/zips"))
        .unwrap()
        .count();
    assert_eq!(zips, 1);
}

#[test]
fn each_row_gets_its_own_zip_and_password() {
    let (p, _) = project(
        "queries: [{query: members, secret_columns: [zip_password]}, balances]\noutput:\n\
         \x20 - name: statement\n    format: csv\n    queries: [members, balances]\n    for_each: {query: members, key: member_id}\n\
         \x20   destination: {profile: outbox, path: \"out/{{ each.member_id }}.zip\", zip: {password: \"{{ each.zip_password }}\"}}\n",
    );
    let run = p.dre("run", &["pack", "-v", "--row-concurrency", "1"]);
    run.ok().says("2 succeeded");
    for (id, pw) in [("101", "Ann-zip-7731"), ("102", "Bob-zip-2290")] {
        let zip = p.path(&format!("out/{id}.zip"));
        assert_eq!(entries(&zip), vec![format!("{id}.csv")]);
        assert!(
            read(&zip, &format!("{id}.csv"), Some(pw))
                .unwrap()
                .contains(&format!("{id},{}", 10 * id.parse::<i32>().unwrap()))
        );
        assert!(read(&zip, &format!("{id}.csv"), None).is_err());
    }
    let seen = format!(
        "{}{}{}",
        run.stdout,
        run.stderr,
        p.read("target/run/pack/default/statement.rows.jsonl")
    );
    assert!(!seen.contains("Ann-zip-7731"), "{seen}");
}

#[test]
fn a_held_zip_is_released_byte_for_byte_and_a_tampered_one_is_refused() {
    let report = format!(
        "{PACK}    destination: {{profile: mail, to: a@example.com, attach: workbook, zip: {{password: Hold-pw-9}}, hold: true}}\n"
    );
    let (p, rec) = project(&report);
    p.dre("run", &["pack"]).ok();
    assert!(deliveries(&rec).is_empty());
    let held = p.path("target/run/pack/default/zips/0/detail.zip");
    let bytes = std::fs::read(&held).unwrap();
    p.dre("release", &["pack", "--run", &run_id(&p)]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    let sent = d[0]["files"][0]["local"].as_str().unwrap();
    assert_eq!(std::fs::read(sent).unwrap(), bytes);
    assert_eq!(
        entries(Path::new(sent)),
        vec!["detail_fees.csv", "detail_summary.csv", "workbook.xlsx"]
    );
    // Run again, change the zip, and release: refused.
    let (p, rec) = project(&report);
    p.dre("run", &["pack"]).ok();
    std::fs::write(p.path("target/run/pack/default/zips/0/detail.zip"), "changed").unwrap();
    p.dre("release", &["pack", "--run", &run_id(&p)])
        .failed()
        .says("refused");
    assert!(deliveries(&rec).is_empty());
}

#[test]
fn zip_mistakes_are_validate_errors_or_run_errors() {
    let bad_ext = format!("{PACK}    destination: {{profile: outbox, path: out/pack.csv, zip: true}}\n");
    let (p, _) = project(&bad_ext);
    p.dre("validate", &[]).failed().says("must end in `.zip`");
    let empty =
        format!("{PACK}    destination: {{profile: outbox, path: out/p.zip, zip: {{password: \"\"}}}}\n");
    let (p, _) = project(&empty);
    p.dre("validate", &[]).failed().says("`zip.password` is empty");
    let message = format!(
        "{PACK}    destination: {{profile: outbox, path: out/p.zip, zip: true, attach: note}}\n  - name: note\n    format: message\n    text: hi\n    destination: {{profile: mail, to: a@example.com, zip: true}}\n"
    );
    let (p, _) = project(&message);
    let v = p.dre("validate", &[]);
    v.failed()
        .says("`attach: note`: `note` is a message, which can't go in a zip")
        .says("a message can't be zipped");
    let no_zip_attach =
        format!("{PACK}    destination: {{profile: outbox, path: out/p.csv, attach: workbook}}\n");
    let (p, _) = project(&no_zip_attach);
    p.dre("validate", &[]).failed().says("or to an entry with `zip:`");
    // Two files of one name: the run fails before delivering anything (zip_pack's unit tests
    // cover a clash only the zip sees).
    let clash = "queries: [summary, fees]\noutput:\n\
        \x20 - name: detail_summary\n    format: csv\n    queries: [summary]\n\
        \x20 - name: detail\n    format: csv\n    queries: [summary, fees]\n\
        \x20   destination: {profile: outbox, path: out/p.zip, zip: true, attach: detail_summary}\n";
    let (p, _) = project(clash);
    p.dre("run", &["pack"])
        .failed()
        .says("both write detail_summary.csv");
    assert!(!p.path("out/p.zip").exists());
}
