use dre_core::context::{Context, Env};
use dre_protocol::msg::FontRequest;
use serde_json::json;

#[test]
fn registry_font_is_checked_and_cached_without_a_second_download() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("regular.otf");
    std::fs::write(&file, b"font bytes").unwrap();
    let registry = dir.path().join("packages.json");
    let checksum = "ee2e88308610019dce3856035fb572b7675dc9d4c990091374349476b5beecb1";
    // Independent SHA-256 literal, generated from the fixture bytes.
    std::fs::write(
        &registry,
        json!({"schema":2,"plugins":[],"fonts":[{
        "name":"noto_sans_jp","license":"OFL-1.1","scripts":["ja"],"versions":[{
            "version":"2.4.0","regular":{"url":file,"sha256":checksum},
            "bold":{"url":file,"sha256":checksum},"license":{"url":file,"sha256":checksum}
        }]}]})
        .to_string(),
    )
    .unwrap();
    let env =
        Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(dir.path().join("home"));
    let ctx = Context::subprocess(env);
    let font = FontRequest {
        name: "noto_sans_jp".into(),
        version: "2.4.0".into(),
    };
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let ctx = ctx.clone();
            let font = font.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                dre_core::fonts::resolve(&ctx, &font).unwrap()
            })
        })
        .collect();
    let paths: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let files = paths[0].clone();
    assert!(paths.iter().all(|p| p == &files));
    assert_eq!(files.len(), 2);
    std::fs::remove_file(file).unwrap();
    std::fs::remove_file(registry).unwrap();
    assert_eq!(dre_core::fonts::resolve(&ctx, &font).unwrap(), files);
    std::fs::write(&files[0], b"corrupt").unwrap();
    assert!(dre_core::fonts::resolve(&ctx, &font).is_err());
}

#[test]
fn a_run_tries_a_missing_font_once_and_marks_later_answers_repeated() {
    let dir = tempfile::tempdir().unwrap();
    // An offline copy of the registry with no `fonts` section: the font can't be found.
    let registry = dir.path().join("packages.json");
    std::fs::write(&registry, json!({"schema":2,"plugins":[]}).to_string()).unwrap();
    let env =
        Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(dir.path().join("home"));
    let ctx = Context::subprocess(env);
    let font = FontRequest {
        name: "noto_sans_jp".into(),
        version: "2.4.0".into(),
    };
    let run = dre_core::fonts::RunFonts::default();
    let first = run.resolve(&ctx, &font).unwrap_err();
    assert!(!first.repeated);
    assert!(
        first.message.contains("noto_sans_jp 2.4.0 isn't in the registry"),
        "{first}"
    );
    // The second row asks again: same answer, without reading the registry again.
    std::fs::remove_file(&registry).unwrap();
    let second = run.resolve(&ctx, &font).unwrap_err();
    assert!(second.repeated);
    assert_eq!(second.message, first.message);
}

/// An offline registry copy: `packages.json` and its font files, all plain paths in `dir`, with
/// a Japanese package and an Arabic one in two versions.
fn offline_registry(dir: &std::path::Path) -> std::path::PathBuf {
    let file = dir.join("font.otf");
    std::fs::write(&file, b"font bytes").unwrap();
    // Independent SHA-256 literal, generated from the fixture bytes.
    let sum = "ee2e88308610019dce3856035fb572b7675dc9d4c990091374349476b5beecb1";
    let version = |v: &str| {
        json!({"version": v, "regular":{"url":file,"sha256":sum},
        "bold":{"url":file,"sha256":sum},"license":{"url":file,"sha256":sum}})
    };
    let registry = dir.join("packages.json");
    std::fs::write(
        &registry,
        json!({"schema":2,"plugins":[],"fonts":[
            {"name":"noto_sans_jp","license":"OFL-1.1","scripts":["ja"],"versions":[version("2.4.0")]},
            {"name":"noto_sans_arabic","license":"OFL-1.1","scripts":["Arab"],
             "versions":[version("2.9.0"), version("2.13.0")]}
        ]})
        .to_string(),
    )
    .unwrap();
    registry
}

#[test]
fn install_fills_the_cache_by_script_or_language_and_ls_lists_it() {
    let dir = tempfile::tempdir().unwrap();
    let registry = offline_registry(dir.path());
    let home = dir.path().join("home");
    let env = Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(home.clone());
    let ctx = Context::subprocess(env.clone());
    assert!(dre_core::fonts::cached(&env).is_empty());

    // `ar` is Arabic's language code; the registry names the script `Arab`. The newest version.
    let installed = dre_core::fonts::install(&ctx, &["JA".into(), "ar".into()]).unwrap();
    let got: Vec<_> = installed
        .iter()
        .map(|f| (f.name.as_str(), f.version.as_str(), f.downloaded))
        .collect();
    assert_eq!(
        got,
        [
            ("noto_sans_jp", "2.4.0", true),
            ("noto_sans_arabic", "2.13.0", true)
        ]
    );
    assert!(
        home.join(".dre/fonts/noto_sans_arabic/2.13.0/regular.otf")
            .is_file()
    );

    // A run then finds the font offline, without the registry.
    std::fs::rename(&registry, dir.path().join("moved.json")).unwrap();
    let font = FontRequest {
        name: "noto_sans_jp".into(),
        version: "2.4.0".into(),
    };
    assert!(dre_core::fonts::resolve(&ctx, &font).is_ok());

    let listed: Vec<_> = dre_core::fonts::cached(&env)
        .into_iter()
        .map(|f| (f.name, f.version, f.scripts, f.verified))
        .collect();
    assert_eq!(
        listed,
        [
            (
                "noto_sans_arabic".into(),
                "2.13.0".into(),
                vec!["Arab".to_string()],
                true
            ),
            (
                "noto_sans_jp".into(),
                "2.4.0".into(),
                vec!["ja".to_string()],
                true
            ),
        ]
    );
}

#[test]
fn install_names_the_known_scripts_for_an_unknown_one() {
    let dir = tempfile::tempdir().unwrap();
    let registry = offline_registry(dir.path());
    let env =
        Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(dir.path().join("home"));
    let err =
        dre_core::fonts::install(&Context::subprocess(env), &["ja".into(), "klingon".into()]).unwrap_err();
    assert_eq!(
        err,
        "no font in the registry covers `klingon`; known scripts: `ja`, `Arab` (also `ar`)"
    );
    assert!(
        !dir.path().join("home/.dre/fonts/noto_sans_jp").exists(),
        "nothing is installed on an error"
    );
}

#[test]
fn install_all_takes_every_package_and_a_second_install_downloads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let registry = offline_registry(dir.path());
    let env =
        Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(dir.path().join("home"));
    let ctx = Context::subprocess(env);
    assert_eq!(dre_core::fonts::install(&ctx, &[]).unwrap().len(), 2);
    let again = dre_core::fonts::install(&ctx, &["ja".into()]).unwrap();
    assert!(!again[0].downloaded);
}

#[test]
fn a_registry_font_name_never_leaves_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("font.otf");
    std::fs::write(&file, b"font bytes").unwrap();
    let sum = "ee2e88308610019dce3856035fb572b7675dc9d4c990091374349476b5beecb1";
    let a = json!({"url": file, "sha256": sum});
    let registry = dir.path().join("packages.json");
    std::fs::write(
        &registry,
        json!({"schema":2,"plugins":[],"fonts":[{"name":"../../escape","scripts":["ja"],
            "versions":[{"version":"1.0.0","regular":a,"bold":a,"license":a}]}]})
        .to_string(),
    )
    .unwrap();
    let env =
        Env::from_vars([("DRE_REGISTRY_URL", registry.to_str().unwrap())]).with_home(dir.path().join("home"));
    let err = dre_core::fonts::install(&Context::subprocess(env), &["ja".into()]).unwrap_err();
    assert!(err.contains("invalid font package name"), "{err}");
    assert!(!dir.path().join("home/escape").exists());
}
