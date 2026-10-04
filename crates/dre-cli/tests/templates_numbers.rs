//! The number filters (`number`, `percent`, `signed`, `currency`, `compact`) and `locale:`.

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

const SQL: &str = "select\n\
  '{{ 12340.5 | number }}' as n,\n\
  '{{ 1234.5678 | number(2) }}' as n2,\n\
  '{{ -1234.5 | number(decimals=1) }}' as neg,\n\
  '{{ 0.0412 | percent }}' as p,\n\
  '{{ 0.0412 | percent | signed }}' as ps,\n\
  '{{ -320 | signed }}' as s,\n\
  '{{ 12340 | currency(\"EUR\") }}' as c,\n\
  '{{ 1234567 | compact }}' as k,\n\
  '{{ none | number }}' as nul\n";

fn project(project_yml: &str, extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        ("dre_project.yml", project_yml),
        ("dependencies.yml", PLUGINS_YML),
        ("reports/kpi/kpi.yml", "queries: [kpi]\n"),
        ("reports/kpi/kpi.sql", SQL),
    ];
    files.extend_from_slice(extra);
    let p = TestProject::new(&files, DUCK_PROFILES);
    p.duckdb("data.duckdb", "select 1;");
    p
}

fn compiled(p: &TestProject, set: &str) -> String {
    p.read(&format!("target/compiled/kpi/{set}/kpi.sql"))
}

#[test]
fn english_is_the_default() {
    let p = project("name: acme\ndefault_profile: warehouse\n", &[]);
    p.dre("compile", &["kpi"]).ok();
    let sql = compiled(&p, "default");
    for want in [
        "'12,341' as n",
        "'1,234.57' as n2",
        "'-1,234.5' as neg",
        "'4.1%' as p",
        "'+4.1%' as ps",
        "'\u{2212}320' as s",
        "'€12,340' as c",
        "'1.2M' as k",
        "'' as nul",
    ] {
        assert!(sql.contains(want), "missing {want:?} in\n{sql}");
    }
}

#[test]
fn a_project_locale_changes_separators_and_symbol_placement() {
    let p = project("name: acme\ndefault_profile: warehouse\nlocale: de-DE\n", &[]);
    p.dre("compile", &["kpi"]).ok();
    let sql = compiled(&p, "default");
    for want in [
        "'12.341' as n",
        "'1.234,57' as n2",
        "'4,1\u{a0}%' as p",
        "'12.340\u{a0}€' as c",
        "'1,2M' as k",
    ] {
        assert!(sql.contains(want), "missing {want:?} in\n{sql}");
    }
}

#[test]
fn a_report_and_a_set_override_the_locale() {
    let p = project(
        "name: acme\ndefault_profile: warehouse\nlocale: de-DE\n",
        &[(
            "reports/kpi/kpi.yml",
            "queries: [kpi]\nlocale: en-US\nsets:\n  - name: fr\n    locale: fr-FR\n  - name: us\n",
        )],
    );
    p.dre("compile", &["kpi", "--set", "all"]).ok();
    assert!(compiled(&p, "us").contains("'12,341' as n"));
    assert!(compiled(&p, "fr").contains("'12\u{202f}341' as n"));
}

#[test]
fn a_set_in_sets_yml_can_set_the_locale() {
    let p = project(
        "name: acme\ndefault_profile: warehouse\n",
        &[
            ("sets.yml", "swiss: {locale: de-CH}\n"),
            ("reports/kpi/kpi.yml", "queries: [kpi]\nsets: [swiss]\n"),
        ],
    );
    p.dre("compile", &["kpi"]).ok();
    assert!(compiled(&p, "swiss").contains("'12\u{2019}341' as n"));
}

#[test]
fn an_unknown_locale_is_an_error_at_load() {
    let p = project("name: acme\ndefault_profile: warehouse\nlocale: xx-YY\n", &[]);
    p.dre("validate", &[])
        .failed()
        .says("error[invalid-locale]")
        .says("unknown locale `xx-YY`");
}

#[test]
fn a_non_number_is_a_clear_error() {
    let p = project(
        "name: acme\ndefault_profile: warehouse\n",
        &[("reports/kpi/kpi.sql", "select '{{ 'abc' | number }}' as n\n")],
    );
    p.dre("compile", &["kpi"])
        .failed()
        .says("`number` needs a number, but got the text \"abc\"");
}
