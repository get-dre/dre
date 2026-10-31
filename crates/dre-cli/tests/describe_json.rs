//! Each first-party plugin package commits its plugins' `describe` replies (and capabilities) in
//! `describe.json` beside its code, so tools that need the metadata (the skills' plugin
//! references) read a file instead of building and starting the plugins. The fields and options
//! are authored once, in the plugin's code; this test fails when a built plugin's reply differs
//! from its file. `DRE_UPDATE_DESCRIBE=1` rewrites the files. Go packages are checked when
//! `DRE_TEST_GO_PLUGINS` names their built programs. The Java package is checked when
//! `DRE_TEST_JDBC_JAR` names its built shaded JAR; the test launches it with the bundled
//! runtime contract through a small test launcher.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dre_protocol::PluginId;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Map, Value, json};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn java_launcher(jar: &Path) -> PathBuf {
    let dir = jar.parent().expect("the JDBC JAR has a parent");
    let exe = dir.join(format!("dre-plugin-jdbc{}", std::env::consts::EXE_SUFFIX));
    let java = if cfg!(windows) {
        dir.join("runtime/bin/java.exe")
    } else {
        dir.join("runtime/bin/java")
    };
    assert!(
        java.is_file(),
        "DRE_TEST_JDBC_JAR must have runtime/bin/java beside it"
    );
    if cfg!(windows) {
        return write_windows_launcher(dir, jar, &java);
    }
    let script = format!(
        "#!/bin/sh\nexec '{}' --add-opens=java.base/java.nio=ALL-UNNAMED -jar '{}' \"$@\"\n",
        java.display(),
        jar.display()
    );
    std::fs::write(&exe, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    exe
}

fn write_windows_launcher(dir: &Path, jar: &Path, java: &Path) -> PathBuf {
    let source = dir.join("launcher.rs");
    std::fs::write(
        &source,
        format!(
            r#"
use std::process::Command;
fn main() {{
    let status = Command::new(r"{}").arg("--add-opens=java.base/java.nio=ALL-UNNAMED")
        .arg("-jar").arg(r"{}").args(std::env::args_os().skip(1)).status().unwrap();
    std::process::exit(status.code().unwrap_or(1));
}}
"#,
            java.display(),
            jar.display()
        ),
    )
    .unwrap();
    let exe = dir.join("dre-plugin-jdbc.exe");
    let build = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            source.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(build.success());
    exe
}

/// What `describe.json` holds for one plugin: its reply, without the plugin version (which
/// changes on every release), and its capabilities.
fn descriptor(exe: &Path, id: &PluginId) -> Value {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start_for(exe, Some(id), log, None).unwrap();
    let mut caps = p.info().capabilities.clone();
    caps.sort();
    let d = p.description().unwrap();
    let _ = p.close();
    let mut v = json!({
        "capabilities": caps,
        "connection_fields": d.connection_fields,
        "option_fields": d.option_fields,
    });
    if let Some(q) = d.identifier_quote {
        v["identifier_quote"] = json!(q);
    }
    if let Some(s) = d.string_literal {
        v["string_literal"] = json!(s);
    }
    if let Some(l) = d.message_limit {
        v["message_limit"] = json!(l);
    }
    v
}

#[test]
fn each_package_describe_json_matches_its_plugins() {
    let packages: Map<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(root().join(".github/scripts/packages.json")).unwrap())
            .unwrap();
    let update = std::env::var_os("DRE_UPDATE_DESCRIBE").is_some();
    let go_dir = std::env::var_os("DRE_TEST_GO_PLUGINS").map(PathBuf::from);
    let mut stale = Vec::new();
    for (package, about) in &packages {
        let (exe, dir) = match about.get("go").and_then(Value::as_str) {
            Some(go) => {
                let Some(d) = &go_dir else {
                    eprintln!("skipped {package}: set DRE_TEST_GO_PLUGINS");
                    continue;
                };
                (
                    d.join(format!("dre-plugin-{package}{}", std::env::consts::EXE_SUFFIX)),
                    root().join(go),
                )
            }
            None => match about.get("java").and_then(Value::as_str) {
                Some(java) => {
                    let Some(jar) = std::env::var_os("DRE_TEST_JDBC_JAR") else {
                        eprintln!("skipped {package}: set DRE_TEST_JDBC_JAR");
                        continue;
                    };
                    let jar = PathBuf::from(jar);
                    (java_launcher(&jar), root().join(java))
                }
                None => (
                    common::workspace_bin(&format!("dre-plugin-{package}")),
                    root().join("plugins").join(package),
                ),
            },
        };
        let mut want = Map::new();
        for id in about["provides"].as_array().unwrap() {
            let id: PluginId = id.as_str().unwrap().parse().unwrap();
            want.insert(id.to_string(), descriptor(&exe, &id));
        }
        let want = Value::Object(want);
        let file = dir.join("describe.json");
        let have: Option<Value> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        if have.as_ref() != Some(&want) {
            if update {
                std::fs::write(&file, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
            } else {
                stale.push(file.display().to_string());
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale describe.json (run with DRE_UPDATE_DESCRIBE=1 and commit): {stale:?}"
    );
}
