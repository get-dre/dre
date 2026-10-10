//! Conformance always; integration tests when `DRE_TEST_POSTGRES=host:port` points at a server
//! with user/password/database `dre` (CI runs one as a service container). TLS and SSH tunnel
//! tests need the containers `.github/scripts/ssh_bastion.sh` starts (`DRE_TEST_POSTGRES_TLS`,
//! `DRE_TEST_POSTGRES_CA`, `DRE_TEST_SSH_BASTION`, `DRE_TEST_SSH_KEY`).

use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Decimal128Type, Int32Type, TimeUnit};
use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_READ_ONLY, CAP_SESSIONS, conformance};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-postgres"))
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

fn server() -> Option<(String, u16)> {
    let v = std::env::var("DRE_TEST_POSTGRES").ok()?;
    let (h, p) = v.split_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn conn(extra: Value) -> Map<String, Value> {
    let (host, port) = server().unwrap();
    let mut m = json!({"host": host, "port": port, "user": "dre", "password": "dre", "database": "dre", "sslmode": "disable"});
    if let Value::Object(e) = extra {
        m.as_object_mut().unwrap().extend(e);
    }
    m.as_object().unwrap().clone()
}

fn open(read_only: bool, extra: Value) -> PluginProcess {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    p.open(conn(extra), read_only).unwrap();
    p
}

fn collect(p: &mut PluginProcess, sql: &str, limit: Option<u64>) -> (Execution, Vec<RecordBatch>) {
    let mut out = Vec::new();
    let e = p
        .execute(sql, limit, |_, b| {
            out.push(b);
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    (e, out)
}

macro_rules! needs_server {
    () => {
        if server().is_none() {
            eprintln!("skipped: set DRE_TEST_POSTGRES=host:port to run");
            return;
        }
    };
}

#[test]
fn advertises_sessions_read_only_and_check() {
    needs_server!();
    let p = open(false, json!({}));
    assert!(p.has(CAP_SESSIONS) && p.has(CAP_READ_ONLY) && p.has(CAP_CHECK));
}

#[test]
fn temp_tables_persist_and_result_sets_are_told_apart_from_side_effects() {
    needs_server!();
    let mut p = open(false, json!({}));
    assert!(matches!(
        collect(&mut p, "create temp table t (id int, name text)", None).0,
        Execution::NoResult { .. }
    ));
    match collect(
        &mut p,
        "insert into t values (1, 'Acme Corp'), (2, 'Client A')",
        None,
    )
    .0
    {
        Execution::NoResult { rows_affected } => assert_eq!(rows_affected, Some(2)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        collect(&mut p, "set statement_timeout = 0", None).0,
        Execution::NoResult { .. }
    ));
    let (e, b) = collect(&mut p, "select id, name from t order by id", None);
    assert!(matches!(e, Execution::Result { rows: 2, .. }));
    assert_eq!(b[0].column(0).as_primitive::<Int32Type>().values(), &[1, 2]);
    assert_eq!(b[0].column(1).as_string::<i32>().value(1), "Client A");
    // An empty result still has a schema.
    match collect(&mut p, "select id from t where false", None).0 {
        Execution::Result { rows: 0, schema } => assert_eq!(schema.field(0).name(), "id"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn types_map_to_arrow() {
    needs_server!();
    let mut p = open(false, json!({}));
    let (_, b) = collect(
        &mut p,
        "select true as b, 1::int2 as s, 2::int8 as l, 1.5::float4 as f, 2.25::float8 as d,
                12345.678::numeric(10,3) as money, 1.5::numeric as loose, (-0.0001)::numeric as tiny,
                date '2026-01-25' as day, time '06:30:00.5' as at,
                timestamp '2026-01-25 12:00:00' as ts, timestamptz '2026-01-25 12:00:00+10' as tstz,
                '6f1c9a52-8d1e-4c4b-9a4e-1f2b3c4d5e6f'::uuid as id, '{\"a\": 1}'::jsonb as j, '\\x0102'::bytea as raw,
                null::int4 as missing",
        None,
    );
    let b = &b[0];
    let s = b.schema();
    let t = |n: &str| s.field_with_name(n).unwrap().data_type().clone();
    assert_eq!(t("b"), DataType::Boolean);
    assert_eq!(t("s"), DataType::Int16);
    assert_eq!(t("money"), DataType::Decimal128(10, 3));
    assert_eq!(
        b.column_by_name("money")
            .unwrap()
            .as_primitive::<Decimal128Type>()
            .value(0),
        12_345_678
    );
    assert_eq!(t("loose"), DataType::Utf8);
    assert_eq!(
        b.column_by_name("loose").unwrap().as_string::<i32>().value(0),
        "1.5"
    );
    assert_eq!(
        b.column_by_name("tiny").unwrap().as_string::<i32>().value(0),
        "-0.0001"
    );
    assert_eq!(t("day"), DataType::Date32);
    assert_eq!(t("at"), DataType::Time64(TimeUnit::Microsecond));
    assert_eq!(t("ts"), DataType::Timestamp(TimeUnit::Microsecond, None));
    assert_eq!(
        t("tstz"),
        DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    let tstz = b
        .column_by_name("tstz")
        .unwrap()
        .as_primitive::<arrow::datatypes::TimestampMicrosecondType>()
        .value(0);
    assert_eq!(tstz, 1_769_306_400_000_000, "12:00+10 is 02:00 UTC");
    assert_eq!(
        b.column_by_name("id").unwrap().as_string::<i32>().value(0),
        "6f1c9a52-8d1e-4c4b-9a4e-1f2b3c4d5e6f"
    );
    assert_eq!(
        b.column_by_name("j").unwrap().as_string::<i32>().value(0),
        "{\"a\":1}"
    );
    assert_eq!(t("raw"), DataType::Binary);
    assert!(b.column_by_name("missing").unwrap().is_null(0));
}

#[test]
fn unsupported_types_ask_for_a_cast() {
    needs_server!();
    let mut p = open(false, json!({}));
    let err = p
        .execute("select interval '1 day' as gap", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(
        err.to_string().contains("cast it in the query, e.g. `gap::text`"),
        "{err}"
    );
    // The session is fine afterwards.
    collect(&mut p, "select 1", None);
}

#[test]
fn row_limit_and_large_results_stream_in_batches() {
    needs_server!();
    let mut p = open(false, json!({}));
    let (e, _) = collect(&mut p, "select g from generate_series(1, 100000) g", Some(10));
    assert!(matches!(e, Execution::Result { rows: 10, .. }));
    let (e, batches) = collect(&mut p, "select g from generate_series(1, 20000) g", None);
    assert!(matches!(e, Execution::Result { rows: 20000, .. }));
    assert!(batches.len() >= 3, "streamed in batches");
}

#[test]
fn read_only_sessions_refuse_writes() {
    needs_server!();
    let mut w = open(false, json!({}));
    collect(&mut w, "create table if not exists ro_probe (n int)", None);
    let mut r = open(true, json!({}));
    let err = r
        .execute("insert into ro_probe values (1)", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(err.to_string().contains("read-only transaction"), "{err}");
}

#[test]
fn check_explains_without_executing_and_reports_errors() {
    needs_server!();
    let mut p = open(false, json!({}));
    collect(&mut p, "create temp table c (n int)", None);
    p.check("insert into c values (1)").unwrap();
    assert!(matches!(
        collect(&mut p, "select * from c", None).0,
        Execution::Result { rows: 0, .. }
    ));
    let err = p.check("select nope from c").unwrap_err();
    assert!(
        err.to_string().contains("column \"nope\" does not exist"),
        "{err}"
    );
}

#[test]
fn schema_sets_the_search_path() {
    needs_server!();
    let mut setup = open(false, json!({}));
    collect(&mut setup, "create schema if not exists client_a", None);
    collect(
        &mut setup,
        "create table if not exists client_a.accounts as select 42 as n",
        None,
    );
    let mut p = open(false, json!({"schema": "client_a"}));
    let (_, b) = collect(&mut p, "select n from accounts", None);
    assert_eq!(b[0].column(0).as_primitive::<Int32Type>().value(0), 42);
}

#[test]
fn bad_credentials_give_a_clear_error() {
    needs_server!();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let err = p.open(conn(json!({"password": "wrong"})), false).unwrap_err();
    assert!(
        err.to_string().contains("password authentication failed"),
        "{err}"
    );
}

#[test]
fn numeric_scales_outside_arrow_decimals_come_back_as_exact_text() {
    needs_server!();
    let mut p = open(false, json!({}));
    // Postgres 15+ allows a negative scale and a scale above the precision.
    let (_, b) = collect(
        &mut p,
        "select 12300::numeric(5,-2) as hundreds, 0.00123::numeric(3,5) as tiny",
        None,
    );
    let b = &b[0];
    assert_eq!(
        b.column_by_name("hundreds").unwrap().as_string::<i32>().value(0),
        "12300"
    );
    assert_eq!(
        b.column_by_name("tiny").unwrap().as_string::<i32>().value(0),
        "0.00123"
    );
}

#[test]
fn load_copies_rows_into_a_temp_table() {
    needs_server!();
    use arrow::array::{BooleanArray, Date32Array, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    let mut p = open(false, json!({}));
    assert!(p.has(dre_protocol::CAP_LOAD));
    let schema = Arc::new(Schema::new(vec![
        Field::new("code", DataType::Utf8, true),
        Field::new("n", DataType::Int64, true),
        Field::new("x", DataType::Float64, true),
        Field::new("ok", DataType::Boolean, true),
        Field::new("d", DataType::Date32, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec![Some("a \"quoted\", value"), None])),
            Arc::new(Int64Array::from(vec![Some(1), None])),
            Arc::new(Float64Array::from(vec![Some(2.5), None])),
            Arc::new(BooleanArray::from(vec![Some(true), None])),
            Arc::new(Date32Array::from(vec![Some(20454), None])),
        ],
    )
    .unwrap();
    // Twice: a second load of the same name replaces the first.
    for _ in 0..2 {
        let (relation, warning) = p.load("things", &schema, [batch.clone()]).unwrap();
        assert_eq!((relation.as_str(), warning), ("dre_lookup_things", None));
    }
    let (_, b) = collect(
        &mut p,
        "select code, n, x, ok, d::text as d, (code is null) as code_null from dre_lookup_things order by n nulls last",
        None,
    );
    assert_eq!(b[0].num_rows(), 2);
    assert_eq!(b[0].column(0).as_string::<i32>().value(0), "a \"quoted\", value");
    assert_eq!(b[0].column(4).as_string::<i32>().value(0), "2026-01-01");
    assert!(b[0].column(5).as_boolean().value(1));
}

#[test]
fn a_connection_error_names_the_server() {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    // Nothing listens on port 1.
    let c = json!({"host": "127.0.0.1", "port": 1, "user": "dre", "database": "shop", "sslmode": "disable"});
    let e = p
        .open(c.as_object().unwrap().clone(), true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("can't connect to Postgres at 127.0.0.1:1/shop"), "{e}");
}

/// `host:port` from an environment variable.
fn addr(var: &str) -> Option<(String, u16)> {
    let v = std::env::var(var).ok()?;
    let (h, p) = v.split_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

/// Settings for the TLS server (`DRE_TEST_POSTGRES_TLS`), whose certificate names `db.internal`.
fn tls_conn(extra: Value) -> Option<Map<String, Value>> {
    let (host, port) = addr("DRE_TEST_POSTGRES_TLS")?;
    let mut m = json!({"host": host, "port": port, "user": "dre", "password": "dre", "database": "dre"});
    if let Value::Object(e) = extra {
        m.as_object_mut().unwrap().extend(e);
    }
    Some(m.as_object().unwrap().clone())
}

fn try_open(c: Map<String, Value>) -> Result<PluginProcess, String> {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    p.open(c, true).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Whether the session is encrypted.
fn encrypted(p: &mut PluginProcess) -> bool {
    let (_, b) = collect(
        p,
        "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
        None,
    );
    b[0].column(0).as_boolean().value(0)
}

#[test]
fn sslmodes_encrypt_and_verify_like_libpq() {
    let (Some(_), Ok(ca)) = (tls_conn(json!({})), std::env::var("DRE_TEST_POSTGRES_CA")) else {
        eprintln!("skipped: run .github/scripts/ssh_bastion.sh and set DRE_TEST_POSTGRES_TLS/_CA");
        return;
    };
    // `prefer` (the default) and `require` encrypt without checking the certificate.
    for mode in [json!({}), json!({"sslmode": "require"})] {
        let mut p = try_open(tls_conn(mode).unwrap()).unwrap();
        assert!(encrypted(&mut p));
    }
    // `verify-ca` checks the chain only: the certificate names db.internal, not localhost.
    let mut p = try_open(tls_conn(json!({"sslmode": "verify-ca", "sslrootcert": ca})).unwrap()).unwrap();
    assert!(encrypted(&mut p));
    let e = try_open(tls_conn(json!({"sslmode": "verify-ca"})).unwrap())
        .err()
        .unwrap();
    assert!(e.contains("can't connect to Postgres at"), "{e}");
    // `verify-full` also checks the host name.
    let e = try_open(tls_conn(json!({"sslmode": "verify-full", "sslrootcert": ca})).unwrap())
        .err()
        .unwrap();
    assert!(e.contains("can't connect to Postgres at"), "{e}");
}

#[test]
fn prefer_falls_back_to_plain_text_and_require_refuses_it() {
    needs_server!();
    let mut p = try_open(conn(json!({"sslmode": "prefer"}))).unwrap();
    assert!(!encrypted(&mut p));
    let e = try_open(conn(json!({"sslmode": "require"}))).err().unwrap();
    assert!(e.contains("does not support TLS"), "{e}");
    let e = try_open(conn(json!({"sslmode": "sometimes"}))).err().unwrap();
    assert!(e.contains("unknown `sslmode` `sometimes`"), "{e}");
}

/// Settings for Postgres at `db.internal`, reachable only through the bastion
/// (`DRE_TEST_SSH_BASTION`), and the bastion's `ssh:` block with `ssh` merged in.
fn tunnel_conn(ssh: Value, extra: Value) -> Option<Map<String, Value>> {
    let (host, port) = addr("DRE_TEST_SSH_BASTION")?;
    let mut block = json!({"host": host, "port": port, "username": "dre", "password": "dre-pass"});
    block
        .as_object_mut()
        .unwrap()
        .extend(ssh.as_object().unwrap().clone());
    let mut m = json!({"host": "db.internal", "port": 5432, "user": "dre", "password": "dre",
        "database": "dre", "ssh": block});
    m.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Some(m.as_object().unwrap().clone())
}

/// The bastion's host key fingerprint, from the error an untrusted bastion gives.
fn bastion_fingerprint() -> String {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("known_hosts");
    std::fs::write(&empty, "").unwrap();
    let e = try_open(tunnel_conn(json!({"known_hosts_path": empty}), json!({})).unwrap())
        .err()
        .unwrap();
    assert!(
        e.contains("can't reach the SSH bastion") && e.contains("isn't in"),
        "{e}"
    );
    e.split("pin `host_key_fingerprint: ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`')
        .to_string()
}

macro_rules! needs_bastion {
    () => {
        if addr("DRE_TEST_SSH_BASTION").is_none() {
            eprintln!("skipped: run .github/scripts/ssh_bastion.sh and set DRE_TEST_SSH_BASTION");
            return;
        }
    };
}

#[test]
fn queries_previews_and_loads_run_through_an_ssh_bastion() {
    needs_bastion!();
    let fp = bastion_fingerprint();
    let mut p = try_open(tunnel_conn(json!({"host_key_fingerprint": fp}), json!({})).unwrap()).unwrap();
    let (_, b) = collect(
        &mut p,
        "select inet_server_addr()::text is not null as remote",
        None,
    );
    assert!(b[0].column(0).as_boolean().value(0));
    let (_, b) = collect(&mut p, "select g from generate_series(1, 20000) g", None);
    assert_eq!(b.iter().map(RecordBatch::num_rows).sum::<usize>(), 20000);
    let (_, b) = collect(&mut p, "select g from generate_series(1, 20000) g", Some(5));
    assert_eq!(b.iter().map(RecordBatch::num_rows).sum::<usize>(), 5);
    // `prefer` (the default) encrypts through the tunnel too.
    assert!(encrypted(&mut p));
    p.close().unwrap();
}

#[test]
fn verify_full_checks_the_database_host_name_through_the_tunnel() {
    needs_bastion!();
    let Ok(ca) = std::env::var("DRE_TEST_POSTGRES_CA") else {
        return;
    };
    let fp = bastion_fingerprint();
    let tls = json!({"sslmode": "verify-full", "sslrootcert": ca});
    let mut p = try_open(tunnel_conn(json!({"host_key_fingerprint": fp}), tls).unwrap()).unwrap();
    assert!(encrypted(&mut p));
}

#[test]
fn the_bastion_takes_a_key_file_or_key_text() {
    needs_bastion!();
    let Ok(key) = std::env::var("DRE_TEST_SSH_KEY") else {
        return;
    };
    let fp = bastion_fingerprint();
    let text = std::fs::read_to_string(&key).unwrap();
    for ssh in [
        json!({"private_key_path": key}),
        json!({"private_key": text}),
        json!({"private_key": text.trim_end().replace('\n', "\\n")}),
    ] {
        let mut ssh = ssh;
        ssh["host_key_fingerprint"] = json!(fp);
        ssh["password"] = Value::Null;
        try_open(tunnel_conn(ssh, json!({})).unwrap()).unwrap();
    }
}

#[test]
fn tunnel_errors_name_the_hop_that_failed() {
    needs_bastion!();
    let fp = bastion_fingerprint();
    let e =
        try_open(tunnel_conn(json!({"host_key_fingerprint": "SHA256:AAAAnotthekey"}), json!({})).unwrap())
            .err()
            .unwrap();
    assert!(
        e.contains("can't reach the SSH bastion") && e.contains("not the pinned"),
        "{e}"
    );
    let e = try_open(
        tunnel_conn(
            json!({"host_key_fingerprint": fp, "password": "wrong"}),
            json!({}),
        )
        .unwrap(),
    )
    .err()
    .unwrap();
    assert!(e.contains("refused the credentials for `dre`"), "{e}");
    let e = try_open(
        tunnel_conn(
            json!({"host_key_fingerprint": fp}),
            json!({"host": "nowhere.internal"}),
        )
        .unwrap(),
    )
    .err()
    .unwrap();
    assert!(e.contains("couldn't connect to nowhere.internal:5432"), "{e}");
    let e = try_open(tunnel_conn(json!({"host_key_fingerprint": fp}), json!({"password": "wrong"})).unwrap())
        .err()
        .unwrap();
    assert!(
        e.contains("can't connect to Postgres at db.internal:5432/dre through the SSH bastion")
            && e.contains("password authentication failed"),
        "{e}"
    );
}

#[test]
fn ssh_settings_are_checked() {
    let c = json!({"host": "db", "user": "dre", "database": "dre", "ssh": "bastion"});
    let e = try_open(c.as_object().unwrap().clone()).err().unwrap();
    assert!(e.contains("`ssh` must be a block"), "{e}");
    let c = json!({"host": "db", "user": "dre", "database": "dre",
        "ssh": {"host": "b", "username": "u", "private_key_path": "/k", "private_key": "k"}});
    let e = try_open(c.as_object().unwrap().clone()).err().unwrap();
    assert!(
        e.contains("set `ssh.private_key_path` or `ssh.private_key`, not both"),
        "{e}"
    );
}

#[test]
fn the_ssh_block_is_a_secret_init_doesnt_ask_for() {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let d = p.description().unwrap();
    let f = d.connection_fields.iter().find(|f| f.name == "ssh").unwrap();
    assert!(f.secret && f.manual);
}

#[test]
fn a_cancel_stops_the_query_on_the_server() {
    needs_server!();
    let mut p = open(false, json!({}));
    let canceller = p.canceller();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(canceller.cancel());
    });
    let started = std::time::Instant::now();
    let err = p
        .execute("select pg_sleep(30)::text", None, |_, _| Ok(()))
        .unwrap_err();
    t.join().unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    match err {
        dre_protocol::host::HostError::Plugin { kind, message, .. } => {
            assert_eq!(kind.as_deref(), Some("cancelled"), "{message}");
            assert!(message.contains("cancel"), "{message}");
        }
        e => panic!("{e:?}"),
    }
    // The session is still there.
    collect(&mut p, "select 1", None);
}

/// Open with `c`, returning the error and the log lines.
fn open_logged(c: Value) -> (String, Vec<String>) {
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = lines.clone();
    let log: LogSink = Arc::new(move |_, m: &str| l.lock().unwrap().push(m.to_string()));
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let err = p
        .open(c.as_object().unwrap().clone(), false)
        .unwrap_err()
        .to_string();
    let _ = p.close();
    let lines = lines.lock().unwrap().clone();
    (err, lines)
}

#[test]
fn connecting_is_tried_again_and_refused_credentials_are_not() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (err, log) = open_logged(
        json!({"host": "127.0.0.1", "port": port, "user": "u", "database": "d",
        "sslmode": "disable", "retries": 1}),
    );
    assert!(err.contains("error connecting to server"), "{err}");
    assert!(log.iter().any(|l| l.contains("attempt 2 of 2")), "{log:?}");
    let Some(_) = server() else { return };
    let mut c = Value::Object(conn(json!({"retries": 3})));
    c["password"] = json!("wrong");
    let (err, log) = open_logged(c);
    assert!(err.contains("password authentication failed"), "{err}");
    assert!(!log.iter().any(|l| l.contains("trying again")), "{log:?}");
}

#[test]
fn the_tunnel_warns_about_rsa_keys_and_can_refuse_them() {
    let dir = tempfile::tempdir().unwrap();
    let rsa = dir.path().join("id_rsa");
    assert!(
        std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "rsa", "-N", "", "-f"])
            .arg(&rsa)
            .status()
            .unwrap()
            .success()
    );
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let conn = |allow: Value| {
        json!({"host": "db", "user": "u", "database": "d", "retries": 0,
            "ssh": {"host": "127.0.0.1", "port": port, "username": "dre",
                "private_key_path": rsa.to_str().unwrap(), "allow_rsa_keys": allow}})
    };
    let (_, log) = open_logged(conn(Value::Null));
    assert!(log.iter().any(|l| l.contains("RUSTSEC-2023-0071")), "{log:?}");
    let (err, _) = open_logged(conn(json!(false)));
    assert!(
        err.contains("`ssh.allow_rsa_keys: false` refuses RSA keys"),
        "{err}"
    );
}
