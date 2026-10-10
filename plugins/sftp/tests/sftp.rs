//! SFTP delivery against a server, when `DRE_TEST_SFTP=host:port` is set (user `dre`, password
//! `dre-pass`, writable `upload/`; `DRE_TEST_SFTP_KEY` is a private key the server accepts).

use std::path::Path;
use std::sync::{Arc, OnceLock};

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-sftp"))
}

fn server() -> Option<(String, u16)> {
    let v = std::env::var("DRE_TEST_SFTP").ok()?;
    let (h, p) = v.split_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn deliver(remote: &str, conn: Value, bytes: &[u8]) -> Result<String, String> {
    deliver_with(remote, conn, json!({}), bytes)
}

/// `deliver` with the destination entry's options.
fn deliver_with(remote: &str, conn: Value, options: Value, bytes: &[u8]) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("report.csv");
    std::fs::write(&local, bytes).unwrap();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let (Value::Object(c), Value::Object(o)) = (conn, options) else {
        panic!()
    };
    let file = dre_protocol::msg::DeliveryFile {
        local_path: local.to_str().unwrap().to_string(),
        remote_path: Some(remote.to_string()),
    };
    p.deliver_files(&[file], c, o).map_err(|e| e.to_string())
}

/// Scan the server's host keys into a known_hosts file. The scan runs once for all tests:
/// `ssh-keyscan` opens a connection per key type, and several scans at once from the parallel
/// tests pass sshd's `MaxStartups` (10 unauthenticated connections), so it drops some at random.
fn known_hosts(dir: &Path, host: &str, port: u16) -> String {
    static SCAN: OnceLock<Vec<u8>> = OnceLock::new();
    // ssh-keyscan can miss a key type when the server is busy (the tests run in parallel), and
    // the host key the client negotiates may be the one it missed: scan until it has both of
    // the emulator's (ed25519 and rsa).
    let keys = SCAN.get_or_init(|| {
        let types = ["ssh-rsa", "ssh-ed25519"];
        let mut out = Vec::new();
        for _ in 0..20 {
            out = std::process::Command::new("ssh-keyscan")
                .args(["-t", "rsa,ed25519", "-p", &port.to_string(), host])
                .output()
                .unwrap()
                .stdout;
            let text = String::from_utf8_lossy(&out);
            if types.iter().all(|t| text.contains(t)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        out
    });
    let path = dir.join("known_hosts");
    std::fs::write(&path, keys).unwrap();
    path.to_string_lossy().to_string()
}

fn base(extra: Value) -> Value {
    let (host, port) = server().unwrap();
    let mut v = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn unknown_hosts_are_refused_until_trusted() {
    let Some((host, port)) = server() else {
        eprintln!("skipped: set DRE_TEST_SFTP=host:port");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty_known_hosts");
    std::fs::write(&empty, "").unwrap();
    let err = deliver("upload/x.csv", base(json!({"known_hosts_path": empty})), b"x").unwrap_err();
    assert!(err.contains("isn't in") && err.contains("SHA256:"), "{err}");

    // Pinning the fingerprint the error reported is enough.
    let fp = err
        .split("pin `host_key_fingerprint: ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`')
        .to_string();
    deliver(
        "upload/pinned.csv",
        base(json!({"known_hosts_path": empty, "host_key_fingerprint": fp})),
        b"x",
    )
    .unwrap();
    let err = deliver(
        "upload/x.csv",
        base(json!({"host_key_fingerprint": "SHA256:AAAAnotthekey"})),
        b"x",
    )
    .unwrap_err();
    assert!(err.contains("not the pinned"), "{err}");

    // A known_hosts entry is trusted too.
    let kh = known_hosts(dir.path(), &host, port);
    deliver("upload/known.csv", base(json!({"known_hosts_path": kh})), b"x").unwrap();
}

#[test]
fn uploads_with_a_password_creating_directories() {
    let Some((host, port)) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let loc = deliver(
        "upload/2026/01/monthly.csv",
        base(json!({"known_hosts_path": kh})),
        &data,
    )
    .unwrap();
    assert_eq!(
        loc,
        format!("sftp://dre@{host}:{port}/upload/2026/01/monthly.csv")
    );
    let err = deliver(
        "upload/x.csv",
        base(json!({"known_hosts_path": kh, "password": "wrong"})),
        b"x",
    )
    .unwrap_err();
    assert!(err.contains("refused the credentials"), "{err}");
}

#[test]
fn uploads_with_a_private_key() {
    let (Some((host, port)), Ok(key)) = (server(), std::env::var("DRE_TEST_SFTP_KEY")) else {
        eprintln!("skipped: set DRE_TEST_SFTP and DRE_TEST_SFTP_KEY");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let conn = json!({"host": host, "port": port, "username": "dre", "private_key_path": key, "known_hosts_path": kh});
    deliver("upload/key.csv", conn, b"a,b\r\n").unwrap();
}

#[test]
fn uploads_with_a_private_key_given_as_text() {
    let (Some((host, port)), Ok(key)) = (server(), std::env::var("DRE_TEST_SFTP_KEY")) else {
        eprintln!("skipped: set DRE_TEST_SFTP and DRE_TEST_SFTP_KEY");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let text = std::fs::read_to_string(&key).unwrap();
    // As the file has it, and squeezed onto one line with literal `\n`, as some CI secrets are.
    for text in [text.clone(), text.trim_end().replace('\n', "\\n")] {
        let conn = json!({"host": host, "port": port, "username": "dre", "private_key": text, "known_hosts_path": kh});
        deliver("upload/key-text.csv", conn, b"a,b\r\n").unwrap();
    }
    let both = json!({"host": host, "port": port, "username": "dre", "private_key": text,
        "private_key_path": key, "known_hosts_path": kh});
    let err = deliver("upload/x.csv", both, b"x").unwrap_err();
    assert!(err.contains("not both"), "{err}");
}

#[test]
fn private_key_text_is_a_secret_init_doesnt_ask_for() {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let d = p.description().unwrap();
    let f = d
        .connection_fields
        .iter()
        .find(|f| f.name == "private_key")
        .unwrap();
    assert!(f.secret && f.manual);
}

#[test]
fn uploads_under_a_temporary_name_replacing_a_file_already_there() {
    let Some((host, port)) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let conn = base(json!({"known_hosts_path": kh}));
    // `atomic` is on by default: the second upload renames over the first.
    deliver("upload/atomic/r.csv", conn.clone(), b"one").unwrap();
    deliver("upload/atomic/r.csv", conn.clone(), b"two").unwrap();
    // The temporary file in another folder on the same server.
    deliver_with(
        "upload/atomic/s.csv",
        conn.clone(),
        json!({"temp_dir": "../staging"}),
        b"three",
    )
    .unwrap();
    // Straight to the final name.
    deliver_with(
        "upload/atomic/t.csv",
        conn.clone(),
        json!({"atomic": false}),
        b"four",
    )
    .unwrap();
    // Options are checked.
    let err = deliver_with("upload/atomic/u.csv", conn, json!({"atomic": "yes"}), b"x").unwrap_err();
    assert!(err.contains("atomic"), "{err}");
}

#[test]
fn if_exists_refuses_or_numbers_a_name_already_taken() {
    let Some((host, port)) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let conn = base(json!({"known_hosts_path": kh}));
    let name = format!("upload/exists/{}.csv", std::process::id());
    deliver(&name, conn.clone(), b"one").unwrap();
    for atomic in [true, false] {
        let opts = json!({"if_exists": "error", "atomic": atomic});
        let err = deliver_with(&name, conn.clone(), opts, b"x").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let opts = json!({"if_exists": "number", "atomic": atomic});
        let loc = deliver_with(&name, conn.clone(), opts, b"two").unwrap();
        let n = if atomic { 2 } else { 3 };
        assert!(
            loc.ends_with(&name.replace(".csv", &format!("_{n}.csv"))),
            "{loc}"
        );
    }
}

#[test]
fn a_refused_connection_is_tried_again_and_refused_credentials_are_not() {
    // Nothing listens on the port: refused at once, tried again after about a second.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let conn = json!({"host": "127.0.0.1", "port": port, "username": "dre", "password": "x",
        "accept_unknown_host": true, "retries": 1});
    let err = deliver("upload/x.csv", conn, b"x").unwrap_err();
    assert!(err.contains("after 2 tries"), "{err}");
    let Some((host, port)) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let err = deliver(
        "upload/x.csv",
        base(json!({"known_hosts_path": kh, "password": "wrong", "retries": 3})),
        b"x",
    )
    .unwrap_err();
    assert!(
        err.contains("refused the credentials") && !err.contains("tries"),
        "{err}"
    );
}

/// Deliver with `conn`, returning the result and the plugin's log lines.
fn deliver_logged(conn: Value) -> (Result<String, String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("report.csv");
    std::fs::write(&local, b"x").unwrap();
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = lines.clone();
    let log: LogSink = Arc::new(move |_, m: &str| l.lock().unwrap().push(m.to_string()));
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let Value::Object(c) = conn else { panic!() };
    let r = p
        .deliver(local.to_str().unwrap(), Some("upload/x.csv"), c)
        .map_err(|e| e.to_string());
    let _ = p.close();
    let lines = lines.lock().unwrap().clone();
    (r, lines)
}

fn keygen(dir: &Path, kind: &str) -> String {
    let path = dir.join(format!("id_{kind}"));
    let ok = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", kind, "-N", "", "-f"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    path.to_string_lossy().to_string()
}

#[test]
fn rsa_keys_warn_unless_allowed_and_can_be_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rsa = keygen(dir.path(), "rsa");
    // Nothing listens: the key is checked before connecting.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let conn = |allow: Value| {
        json!({"host": "127.0.0.1", "port": port, "username": "dre", "private_key_path": rsa,
            "accept_unknown_host": true, "retries": 0, "allow_rsa_keys": allow})
    };
    let (r, log) = deliver_logged(conn(Value::Null));
    assert!(r.is_err());
    assert_eq!(
        log.iter().filter(|l| l.contains("RUSTSEC-2023-0071")).count(),
        1,
        "{log:?}"
    );
    let (_, log) = deliver_logged(conn(json!(true)));
    assert!(!log.iter().any(|l| l.contains("RUSTSEC-2023-0071")), "{log:?}");
    let (r, _) = deliver_logged(conn(json!(false)));
    let err = r.unwrap_err();
    assert!(
        err.contains("`allow_rsa_keys: false` refuses RSA keys") && err.contains("ssh-keygen -t ed25519"),
        "{err}"
    );
    // An Ed25519 key never warns.
    let ed = keygen(dir.path(), "ed25519");
    let (_, log) = deliver_logged(json!({"host": "127.0.0.1", "port": port, "username": "dre",
        "private_key_path": ed, "accept_unknown_host": true, "retries": 0}));
    assert!(!log.iter().any(|l| l.contains("RUSTSEC")), "{log:?}");
}

#[test]
fn uploads_with_an_ecdsa_key() {
    let (Some((host, port)), Ok(key)) = (server(), std::env::var("DRE_TEST_SFTP_ECDSA_KEY")) else {
        eprintln!("skipped: set DRE_TEST_SFTP and DRE_TEST_SFTP_ECDSA_KEY");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(dir.path(), &host, port);
    let conn = json!({"host": host, "port": port, "username": "dre", "private_key_path": key, "known_hosts_path": kh});
    deliver("upload/ecdsa.csv", conn, b"a,b\r\n").unwrap();
}

#[cfg(unix)]
#[test]
fn signs_in_through_the_ssh_agent() {
    let (Some((host, port)), Ok(key)) = (server(), std::env::var("DRE_TEST_SFTP_KEY")) else {
        eprintln!("skipped: set DRE_TEST_SFTP and DRE_TEST_SFTP_KEY");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("agent.sock");
    let mut agent = std::process::Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let added = std::process::Command::new("ssh-add")
        .arg(&key)
        .env("SSH_AUTH_SOCK", &sock)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(added);
    // Only this test reads SSH_AUTH_SOCK, and the plugin process inherits it.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
    let kh = known_hosts(dir.path(), &host, port);
    let conn =
        json!({"host": host, "port": port, "username": "dre", "use_agent": true, "known_hosts_path": kh});
    let r = deliver("upload/agent.csv", conn.clone(), b"a,b\r\n");
    let mut both = conn;
    both["private_key_path"] = json!(key);
    let err = deliver("upload/x.csv", both, b"x").unwrap_err();
    let _ = agent.kill();
    r.unwrap();
    assert!(err.contains("not both"), "{err}");
}
