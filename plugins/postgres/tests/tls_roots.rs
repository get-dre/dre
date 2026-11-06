//! `sslrootcert` is the only trust for `verify-ca`: a server certificate from any other CA, even
//! one the system trusts, is refused. Needs the TLS container `.github/scripts/ssh_bastion.sh`
//! starts (`DRE_TEST_POSTGRES_TLS`, `DRE_TEST_POSTGRES_CA`).
//!
//! The attack: on OpenSSL (Linux) `SSL_CERT_FILE` makes the server's CA a system root, standing
//! in for a public CA that signed an attacker's certificate. `sslrootcert` names another CA.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dre_plugin_host::host::{LogSink, PluginProcess};
use dre_protocol::{CORE_MIN_VERSION, MAX_VERSION};
use serde_json::{Value, json};

const OTHER_CA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/other-ca.pem");

/// Opens a read-only session with the server's CA as a system root; returns the error, if any.
fn open(system_ca: &str, extra: Value) -> Result<(), String> {
    let (host, port) = std::env::var("DRE_TEST_POSTGRES_TLS")
        .unwrap()
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap()))
        .unwrap();
    let mut c = json!({"host": host, "port": port, "user": "dre", "password": "dre", "database": "dre"});
    c.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let log: LogSink = Arc::new(|_, _| {});
    let bin = Path::new(env!("CARGO_BIN_EXE_dre-plugin-postgres"));
    let mut p = PluginProcess::spawn_env(bin, log, &[("SSL_CERT_FILE", system_ca)]).unwrap();
    p.handshake((CORE_MIN_VERSION, MAX_VERSION), Duration::from_secs(30))
        .unwrap();
    p.open(c.as_object().unwrap().clone(), true)
        .map_err(|e| e.to_string())
}

#[test]
fn verify_ca_trusts_only_sslrootcert_and_reads_a_whole_bundle() {
    let (Ok(_), Ok(ca)) = (
        std::env::var("DRE_TEST_POSTGRES_TLS"),
        std::env::var("DRE_TEST_POSTGRES_CA"),
    ) else {
        eprintln!("skipped: run .github/scripts/ssh_bastion.sh and set DRE_TEST_POSTGRES_TLS/_CA");
        return;
    };
    // A certificate from another CA than `sslrootcert`'s is refused, though the system trusts it.
    let e = open(&ca, json!({"sslmode": "verify-ca", "sslrootcert": OTHER_CA})).unwrap_err();
    assert!(e.contains("can't connect to Postgres at"), "{e}");
    // `verify-ca` without `sslrootcert` would trust every public CA: refused up front.
    let e = open(&ca, json!({"sslmode": "verify-ca"})).unwrap_err();
    assert!(e.contains("`sslmode: verify-ca` needs `sslrootcert`"), "{e}");
    // A bundle whose second certificate is the server's CA is read in full.
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("bundle.pem");
    let pem = std::fs::read_to_string(OTHER_CA).unwrap() + &std::fs::read_to_string(&ca).unwrap();
    std::fs::write(&bundle, pem).unwrap();
    open(OTHER_CA, json!({"sslmode": "verify-ca", "sslrootcert": bundle})).unwrap();
}
