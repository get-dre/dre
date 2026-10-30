//! The `document` package's html and pdf formats through `dre run` and `dre validate`: the
//! template context core sends, template lookup by name, and Set overrides.

mod common;

use common::{TestProject, test_plugins};

const PROFILES: &str =
    "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n";

fn project(report: &str, extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("dependencies.yml", "plugins: [duckdb, document]\n"),
        ("reports/clients/statement/statement.yml", report),
        (
            "reports/clients/statement/holdings.sql",
            "select 'Fund ' || id as fund, id * 1000.5 as amount from range(1, 4) t(id) order by id",
        ),
        (
            "reports/clients/statement/total.sql",
            "select sum(id * 1000.5) as total from range(1, 4) t(id)",
        ),
        (
            "reports/clients/statement/statement.html",
            "<html><head><title>{{ var('client') }}</title></head><body><h1>{{ var('client') }}</h1>\n<p>Total {{ results.total.value | number(2) }}</p><p>Terms: {% include 'terms.html' %}</p>{{ table(results.holdings) }}</body></html>",
        ),
        ("reports/terms.html", "{{ var('terms') }}"),
        (
            "templates/alternate.html",
            "<p>Alternate for {{ var('client') }} on {{ run.date }}</p>",
        ),
    ];
    files.extend_from_slice(extra);
    let mut p = TestProject::new(&files, PROFILES);
    p.plugins = test_plugins(&["dre-plugin-duckdb", "dre-plugin-document"]);
    p.duckdb("data.duckdb", "select 1;");
    p
}

const REPORT: &str = "queries: [total, holdings]\nvars: {client: Acme, terms: Net 30 days}\n\
sets:\n  - {name: acme}\n  - {name: beta, vars: {client: Beta}, output: {template: {file: alternate.html}}}\n\
output: {format: html, template: {file: statement.html}}\n";

#[test]
fn html_from_a_template_found_by_name_with_vars_and_set_overrides() {
    let p = project(REPORT, &[]);
    p.dre("run", &["statement", "--set", "all"]).ok();
    let html = p.read("target/run/statement/acme/statement.html");
    assert!(html.contains("<h1>Acme</h1>"), "{html}");
    // A var no query uses, through an include found in a parent folder.
    assert!(html.contains("Terms: Net 30 days"), "{html}");
    assert!(html.contains("Total 6,003.00"), "{html}");
    assert!(html.contains("Fund 3"), "{html}");
    // The Set overrides the template file, found anywhere in the project by its name.
    let alt = p.read("target/run/statement/beta/statement.html");
    assert!(alt.starts_with("<p>Alternate for Beta on "), "{alt}");
}

#[test]
fn pdf_through_dre_run() {
    let p = project(&REPORT.replace("format: html", "format: pdf"), &[]);
    p.dre("run", &["statement", "--set", "acme"]).ok();
    let pdf = std::fs::read(p.path("target/run/statement/acme/statement.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(String::from_utf8_lossy(&pdf).contains("/Title (Acme)"));
}

#[test]
fn validate_reports_template_problems() {
    let p = project(
        "queries: [total, holdings]\noutput: {format: pdf, template: {file: bad.html}}\n",
        &[(
            "reports/clients/statement/bad.html",
            "{{ results.totl.value }}{% include 'nowhere.html' %}",
        )],
    );
    p.dre("validate", &[])
        .failed()
        .says("`results.totl` isn't a query of this output")
        .says("did you mean `results.total`")
        .says("nowhere.html");
    let p = project(
        "queries: [total]\noutput: {format: html, template: {file: missing.html}}\n",
        &[("templates/a/x.html", ""), ("shared/x.html", "")],
    );
    p.dre("validate", &[])
        .failed()
        .says("template file `missing.html` doesn't exist");
    let p = project(
        "queries: [total]\noutput: {format: html, template: {file: x.html}}\n",
        &[("templates/a/x.html", ""), ("shared/x.html", "")],
    );
    p.dre("validate", &[])
        .failed()
        .says("matches 2 files")
        .says("`shared/x.html`");
}

#[test]
fn template_bindings_are_for_xlsx_only() {
    let p = project(
        "queries: [total]\noutput: {format: html, template: {file: statement.html, bindings: [{sheet: S, cell: A1, value: x}]}}\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("`output.template.bindings` is for xlsx templates");
}

#[test]
fn a_relative_project_dir_works() {
    let p = project(REPORT, &[]);
    let mut c = assert_cmd::Command::cargo_bin("dre").unwrap();
    c.current_dir(p.root())
        .args(["run", "statement", "--set", "acme", "--project-dir", "."])
        .arg("--profiles-dir")
        .arg(p.dir.path().join("profiles"))
        .env("DRE_PLUGINS_DIR", &p.plugins)
        .env("HOME", p.dir.path().join("home"))
        .env_remove("DRE_PROFILES_DIR")
        .env_remove("DRE_TARGET")
        .env("DRE_RUN_DATE", "2026-09-30");
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        p.read("target/run/statement/acme/statement.html")
            .contains("<h1>Acme</h1>")
    );
}

const PER_ROW_JA: &str = "queries: [members, total, holdings]\noutput:\n  name: letter\n  format: pdf\n  queries: [members]\n  template: {file: letter.html}\n  for_each: {query: members, key: member_id}\n";

fn offline_letters(options: &str) -> (TestProject, common::Run) {
    let report = format!("{PER_ROW_JA}{options}");
    let p = project(
        &report,
        &[
            (
                "reports/clients/statement/members.sql",
                "select id as member_id from range(1, 4) t(id) order by id",
            ),
            (
                "reports/clients/statement/letter.html",
                "<html lang=\"ja\"><body><p>東京 {{ each.member_id }}</p></body></html>",
            ),
        ],
    );
    // Offline, nothing cached: the registry copy it points at isn't there.
    let missing = p.path("no-registry/packages.json");
    let run = p.dre_env(
        "run",
        &["statement"],
        &[("DRE_REGISTRY_URL", missing.to_str().unwrap())],
    );
    (p, run)
}

#[test]
fn an_offline_per_row_pdf_warns_once_per_script_for_the_run() {
    let (p, run) = offline_letters("");
    run.ok();
    for key in 1..=3 {
        let pdf = std::fs::read(p.path(&format!("target/run/statement/default/letter/{key}.pdf")));
        assert!(pdf.is_ok_and(|b| b.starts_with(b"%PDF-")), "row {key}");
    }
    let all = format!("{}{}", run.stdout, run.stderr);
    assert_eq!(all.matches("no Japanese font for this PDF").count(), 1, "{all}");
    assert!(all.contains("dre fonts install ja"), "{all}");
}

#[test]
fn missing_fonts_error_fails_every_row_instead() {
    let (p, run) = offline_letters("  missing_fonts: error\n");
    run.failed();
    let all = format!("{}{}", run.stdout, run.stderr);
    assert!(all.contains("`missing_fonts: error`"), "{all}");
    assert!(!p.path("target/run/statement/default/letter/1.pdf").exists());
}

#[test]
fn validate_rejects_a_bad_missing_fonts_value() {
    let p = project(
        "queries: [total, holdings]\nvars: {client: Acme, terms: Net 30 days}\noutput: {format: pdf, template: {file: statement.html}, missing_fonts: maybe}\n",
        &[],
    );
    p.dre("validate", &[])
        .failed()
        .says("missing_fonts")
        .says("`missing_fonts` must be `warn` or `error`")
        .lacks("undeclared-plugin");
}
