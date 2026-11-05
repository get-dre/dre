//! Macro packages: declared in dependencies.yml/packages.yml, installed into dre_deps/packages,
//! called through the package's name, and overridable through `dispatch()`.

mod common;

use std::path::Path;
use std::process::Command;

use common::{DUCK_PROFILES, TestProject};

const DEPS: &str = "plugins: [duckdb, csv]\n";

/// A `dre_utils` package: a plain macro, and a dispatched one with a DuckDB variant.
fn write_package(dir: &Path) {
    std::fs::create_dir_all(dir.join("macros")).unwrap();
    std::fs::write(dir.join("dre_package.yml"), "name: dre_utils\n").unwrap();
    std::fs::write(
        dir.join("macros/utils.sql"),
        "{% macro shout(s) %}upper('{{ s }}'){% endmacro %}\n\
         {% macro label(s) %}{{ dispatch('label', 'dre_utils')(s) }}{% endmacro %}\n\
         {% macro default__label(s) %}'default {{ s }}'{% endmacro %}\n\
         {% macro duckdb__label(s) %}'duckdb {{ s }}'{% endmacro %}\n",
    )
    .unwrap();
}

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        ("dre_project.yml", "name: acme\ndefault_profile: warehouse\n"),
        ("dependencies.yml", DEPS),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, DUCK_PROFILES);
    p.duckdb("data.duckdb", "select 1;");
    p
}

const REPORT: [(&str, &str); 2] = [
    ("reports/r/r.yml", "queries: [q]\n"),
    (
        "reports/r/q.sql",
        "select {{ dre_utils.shout('hi') }} as a, {{ dre_utils.label('x') }} as b\n",
    ),
];

#[test]
fn a_local_package_is_called_through_its_name_and_dispatches_per_source() {
    let p = project(&[
        ("packages.yml", "packages:\n  - local: ../shared/dre_utils\n"),
        REPORT[0],
        REPORT[1],
    ]);
    write_package(&p.root().join("../shared/dre_utils"));
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.csv"), "a,b\r\nHI,duckdb x\r\n");
}

#[test]
fn the_project_overrides_a_dispatched_macro_unless_dispatch_config_says_otherwise() {
    let p = project(&[
        ("packages.yml", "packages:\n  - local: ../shared/dre_utils\n"),
        (
            "macros/overrides.sql",
            "{% macro duckdb__label(s) %}'mine {{ s }}'{% endmacro %}\n",
        ),
        // The project's own `shout` doesn't touch the package's: they're different names.
        (
            "macros/shout.sql",
            "{% macro shout(s) %}'local {{ s }}'{% endmacro %}\n",
        ),
        REPORT[0],
        (
            "reports/r/q.sql",
            "select {{ shout('hi') }} as a, {{ dre_utils.shout('hi') }} as b, {{ dre_utils.label('x') }} as c\n",
        ),
    ]);
    write_package(&p.root().join("../shared/dre_utils"));
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.read("target/run/r/default/r.csv"),
        "a,b,c\r\nlocal hi,HI,mine x\r\n"
    );

    p.write(
        "dre_project.yml",
        "name: acme\ndefault_profile: warehouse\ndispatch:\n  - macro_namespace: dre_utils\n    search_order: [dre_utils]\n",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.read("target/run/r/default/r.csv"),
        "a,b,c\r\nlocal hi,HI,duckdb x\r\n"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_git_package_installs_into_dre_deps_and_pins_its_commit() {
    let p = project(&[REPORT[0], REPORT[1]]);
    let repo = p.root().join("../repo");
    write_package(&repo);
    std::fs::write(repo.join("macros/tests.sql"),
        "{% test positive(relation, column='n') %}select * from {{ relation }} where {{ column }} < 0{% endtest %}").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v1"]);
    git(&repo, &["tag", "v1"]);
    // A plain path (not canonicalize(), whose `\\?\` prefix on Windows git reads as a host).
    let url = repo.to_string_lossy().replace('\\', "/");
    p.write(
        "dependencies.yml",
        &format!("{DEPS}packages:\n  - git: {url}\n    revision: v1\n"),
    );

    p.dre("deps", &[])
        .ok()
        .says("package `dre_utils`")
        .says("1 package(s)");
    assert!(p.path("dre_deps/packages/dre_utils/macros/utils.sql").is_file());
    assert!(!p.path("dre_deps/packages/dre_utils/.git").exists());
    let lock = p.read("dre.lock");
    assert!(
        lock.contains("packages:\n  dre_utils:\n")
            && lock.contains("revision: v1")
            && lock.contains("commit: "),
        "{lock}"
    );

    // Up to date: nothing to install.
    let again = p.dre("deps", &[]);
    again.ok();
    assert!(!again.stdout.contains("Installed"), "{}", again.stdout);
    p.dre("run", &["r"]).ok();

    p.write(
        "reports/r/r.yml",
        "queries:\n  - query: q\n    data_tests: [{dre_utils.positive: {}}]\n",
    );
    p.write("reports/r/q.sql", "select 1 as n");
    p.dre("test", &["r"]).ok().says("PASS");

    // A new revision is picked up; the old checkout is replaced.
    std::fs::write(repo.join("macros/more.sql"), "{% macro two() %}2{% endmacro %}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v2"]);
    git(&repo, &["tag", "v2"]);
    p.write(
        "dependencies.yml",
        &format!("{DEPS}packages:\n  - git: {url}\n    revision: v2\n"),
    );
    p.dre("deps", &[]).ok().says("Installed");
    assert!(p.path("dre_deps/packages/dre_utils/macros/more.sql").is_file());
    assert!(p.read("dre.lock").contains("revision: v2"));
}

/// A project depending on a `dre_utils` git repo at tag v1 (made by `fill`), and its URL.
fn git_package_project(fill: impl FnOnce(&Path)) -> TestProject {
    let p = project(&[REPORT[0], REPORT[1]]);
    let repo = p.root().join("../repo");
    write_package(&repo);
    fill(&repo);
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v1"]);
    git(&repo, &["tag", "v1"]);
    let url = repo.to_string_lossy().replace('\\', "/");
    p.write(
        "dependencies.yml",
        &format!("{DEPS}packages:\n  - git: {url}\n    revision: v1\n"),
    );
    p
}

#[test]
fn parallel_deps_install_a_git_package_once() {
    let p = git_package_project(|_| {});
    let start = std::sync::Barrier::new(4);
    let runs: Vec<common::Run> = std::thread::scope(|s| {
        let racers: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    p.dre("deps", &[])
                })
            })
            .collect();
        racers.into_iter().map(|t| t.join().unwrap()).collect()
    });
    for r in &runs {
        r.ok();
    }
    let installs = runs
        .iter()
        .filter(|r| r.stdout.contains("package `dre_utils`"))
        .count();
    assert_eq!(installs, 1, "{runs:?}");
    p.dre("run", &["r"]).ok();
}

#[cfg(unix)]
#[test]
fn a_git_package_with_a_symlink_is_refused() {
    let p = git_package_project(|repo| {
        std::fs::write(repo.join("../secret.txt"), "not for macros").unwrap();
        std::os::unix::fs::symlink("../../secret.txt", repo.join("macros/k.sql")).unwrap();
    });
    p.dre("deps", &[]).failed().says("macros/k.sql is a symlink");
    assert!(!p.path("dre_deps/packages/dre_utils/macros/k.sql").exists());
}

#[test]
fn package_problems_are_reported() {
    let p = project(&[
        (
            "packages.yml",
            "packages:\n  - local: ../shared/run_pkg\n  - git: https://example.invalid/x.git\n",
        ),
        ("reports/r/r.yml", "queries: [q]\npackages: [nope]\n"),
        ("reports/r/q.sql", "select 1\n"),
    ]);
    let pkg = p.root().join("../shared/run_pkg");
    std::fs::create_dir_all(pkg.join("macros")).unwrap();
    std::fs::write(pkg.join("dre_package.yml"), "name: run\n").unwrap();
    p.dre("validate", &["--no-auto-install"])
        .failed()
        .says("git package `https://example.invalid/x.git` needs a `revision`")
        .says("`packages:` goes in dependencies.yml or packages.yml at the project root")
        .says("package `run` has the same name as a DRE function or variable");
}

#[test]
fn a_git_package_that_isnt_installed_asks_for_dre_deps() {
    let p = project(&[
        (
            "packages.yml",
            "packages:\n  - git: https://example.invalid/x.git\n    revision: v1\n",
        ),
        REPORT[0],
        REPORT[1],
    ]);
    p.dre("validate", &["--no-auto-install"])
        .failed()
        .says("the package from https://example.invalid/x.git isn't installed; run `dre deps`");
}

#[cfg(unix)]
#[test]
fn a_git_package_without_git_says_git_is_missing() {
    let p = project(&[REPORT[0], REPORT[1]]);
    p.write(
        "dependencies.yml",
        &format!("{DEPS}packages:\n  - git: https://example.invalid/acme/dre_utils.git\n    revision: v1\n"),
    );
    // A PATH with no git on it.
    let empty = p.root().join("../empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    p.dre_env("deps", &[], &[("PATH", &empty.to_string_lossy())])
        .failed()
        .says("git isn't installed (needed for git packages)");
}
