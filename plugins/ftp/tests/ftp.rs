//! FTP delivery against a server, when `DRE_TEST_FTP=host:port` is set (user `dre`,
//! password `dre-pass`, passive ports reachable from the test).

use std::path::Path;
use std::sync::Arc;

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-ftp"))
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

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn uploads_in_passive_mode_creating_directories() {
    let Ok(server) = std::env::var("DRE_TEST_FTP") else {
        eprintln!("skipped: set DRE_TEST_FTP=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i % 253) as u8).collect();
    let loc = deliver("reports/2026/monthly.csv", conn.clone(), &data).unwrap();
    assert_eq!(loc, format!("ftp://dre@{host}:{port}/reports/2026/monthly.csv"));
    // Again, into the same (now existing) directories.
    deliver("reports/2026/monthly.csv", conn.clone(), b"again").unwrap();

    let mut bad = conn.clone();
    bad["password"] = json!("wrong");
    assert!(
        deliver("x.csv", bad, b"x")
            .unwrap_err()
            .contains("refused the credentials")
    );
    // This server doesn't offer TLS: asking for FTPS fails clearly instead of sending in clear.
    let mut tls = conn;
    tls["tls"] = json!("explicit");
    assert!(
        deliver("x.csv", tls, b"x")
            .unwrap_err()
            .contains("didn't accept explicit FTPS")
    );
}

#[test]
fn explicit_ftps_reuses_the_tls_session_for_data() {
    // vsftpd with its defaults (`require_ssl_reuse=YES`) and a self-signed certificate.
    let Ok(server) = std::env::var("DRE_TEST_FTPS") else {
        eprintln!("skipped: set DRE_TEST_FTPS=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass",
                      "tls": "explicit", "tls_accept_invalid_certs": true});
    let data: Vec<u8> = (0..500_000u32).map(|i| (i % 251) as u8).collect();
    let loc = deliver("tls/reports/monthly.csv", conn.clone(), &data).unwrap();
    assert_eq!(loc, format!("ftp://dre@{host}:{port}/tls/reports/monthly.csv"));
    deliver("tls/reports/monthly.csv", conn.clone(), b"again").unwrap();

    // Without accepting it, the self-signed certificate is refused.
    let mut strict = conn;
    strict["tls_accept_invalid_certs"] = json!(false);
    let e = deliver("tls/x.csv", strict, b"x").unwrap_err();
    assert!(e.contains("didn't accept explicit FTPS"), "{e}");
}

#[test]
fn uploads_under_a_temporary_name_replacing_a_file_already_there() {
    let Ok(server) = std::env::var("DRE_TEST_FTP") else {
        eprintln!("skipped: set DRE_TEST_FTP=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    deliver("atomic/r.csv", conn.clone(), b"one").unwrap();
    deliver("atomic/r.csv", conn.clone(), b"two").unwrap();
    deliver_with(
        "atomic/s.csv",
        conn.clone(),
        json!({"temp_dir": "../staging"}),
        b"three",
    )
    .unwrap();
    deliver_with("atomic/t.csv", conn, json!({"atomic": false}), b"four").unwrap();
}

#[test]
fn if_exists_refuses_or_numbers_a_name_already_taken() {
    let Ok(server) = std::env::var("DRE_TEST_FTP") else {
        eprintln!("skipped: set DRE_TEST_FTP=host:port");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    let name = format!("exists/{}.csv", std::process::id());
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
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let conn = json!({"host": "127.0.0.1", "port": port, "username": "dre", "password": "x", "retries": 1});
    let err = deliver("x.csv", conn, b"x").unwrap_err();
    assert!(err.contains("after 2 tries"), "{err}");
    let Ok(server) = std::env::var("DRE_TEST_FTP") else {
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    let conn = json!({"host": host, "port": port, "username": "dre", "password": "wrong", "retries": 3});
    let err = deliver("x.csv", conn, b"x").unwrap_err();
    assert!(
        err.contains("refused the credentials") && !err.contains("tries"),
        "{err}"
    );
}

/// Deliver a file of `bytes` with `conn`, returning the result and the plugin's log lines.
fn deliver_logged(conn: Value, bytes: &[u8]) -> (Result<String, String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("report.csv");
    std::fs::write(&local, bytes).unwrap();
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = lines.clone();
    let log: LogSink = Arc::new(move |_, m: &str| l.lock().unwrap().push(m.to_string()));
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let Value::Object(c) = conn else { panic!() };
    let r = p
        .deliver(local.to_str().unwrap(), Some("x.csv"), c)
        .map_err(|e| e.to_string());
    let _ = p.close();
    let lines = lines.lock().unwrap().clone();
    (r, lines)
}

/// A port nothing listens on.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn a_password_over_plain_ftp_warns() {
    let conn = |tls: &str| {
        json!({"host": "127.0.0.1", "port": closed_port(), "username": "dre", "password": "x",
            "tls": tls, "retries": 0})
    };
    let (r, log) = deliver_logged(conn("none"), b"x");
    assert!(r.is_err());
    assert!(
        log.iter()
            .any(|l| l.contains("plain text") && l.contains("tls: explicit")),
        "{log:?}"
    );
    let (_, log) = deliver_logged(conn("explicit"), b"x");
    assert!(!log.iter().any(|l| l.contains("plain text")), "{log:?}");
}

#[test]
fn a_port_above_65535_is_refused() {
    let conn = json!({"host": "127.0.0.1", "port": 65557, "username": "dre", "password": "x"});
    let err = deliver("x.csv", conn, b"x").unwrap_err();
    assert!(err.contains("`port` must be a port number"), "{err}");
}

/// A server that stops reading the data connection mid-upload (disk full, a stuck process):
/// the upload fails after `timeout` instead of hanging.
#[test]
fn a_stalled_data_connection_times_out() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let data = TcpListener::bind("127.0.0.1:0").unwrap();
    let (cport, dport) = (
        control.local_addr().unwrap().port(),
        data.local_addr().unwrap().port(),
    );
    // Sockets the server holds open without reading, until the test ends.
    let (held_tx, held_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (s, _) = control.accept().unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(b"220 ready\r\n").unwrap();
        for line in BufReader::new(s).lines() {
            let line = line.unwrap();
            let reply = match line.split(' ').next().unwrap() {
                "USER" => "331 password".to_string(),
                "PASS" => "230 in".to_string(),
                "SIZE" => "550 no such file".to_string(),
                "PASV" => format!("227 Passive (127,0,0,1,{},{})", dport >> 8, dport & 0xff),
                "STOR" => {
                    w.write_all(b"150 go\r\n").unwrap();
                    held_tx.send(data.accept().unwrap().0).unwrap();
                    continue;
                }
                _ => "200 ok".to_string(),
            };
            w.write_all(format!("{reply}\r\n").as_bytes()).unwrap();
        }
    });
    // More than the socket buffers hold, so the upload blocks once the server stops reading.
    let bytes = vec![b'x'; 64 << 20];
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let conn = json!({"host": "127.0.0.1", "port": cport, "username": "dre", "password": "x",
            "timeout": "1s", "retries": 0, "atomic": false});
        done_tx.send(deliver_logged(conn, &bytes).0).unwrap();
    });
    let r = done_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the upload hung on a stalled data connection");
    assert!(r.unwrap_err().contains("can't upload to x.csv"));
    drop(held_rx);
}
