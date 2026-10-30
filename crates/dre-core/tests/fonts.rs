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
