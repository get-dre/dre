//! Core's side of the protocol, exercised against the fixture plugin.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::{Array, Int64Array};
use dre_protocol::host::{Execution, HostError, LogSink, PluginProcess};
use dre_protocol::{CAP_SESSIONS, Kind, MAX_VERSION, MIN_VERSION, conformance};
use serde_json::{Map, json};

fn fixture() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-source-fixture"))
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn start_mode(mode: &str, timeout: Duration) -> Result<PluginProcess, HostError> {
    let mut p = PluginProcess::spawn_env(fixture(), quiet(), &[("DRE_FIXTURE_MODE", mode)])?;
    p.handshake((MIN_VERSION, MAX_VERSION), timeout)?;
    Ok(p)
}

fn opened() -> PluginProcess {
    let mut p = PluginProcess::start(fixture(), quiet()).unwrap();
    p.open(Map::new(), false).unwrap();
    p
}

fn collect(p: &mut PluginProcess, sql: &str, limit: Option<u64>) -> (Execution, Vec<i64>) {
    let mut values = Vec::new();
    let e = p
        .execute(sql, limit, |_, b| {
            let col = b.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
            values.extend((0..col.len()).map(|i| col.value(i)));
            Ok(())
        })
        .unwrap();
    (e, values)
}

#[test]
fn the_fixture_plugin_passes_the_conformance_suite() {
    conformance::assert_conforms(fixture());
}

#[test]
fn handshake_reports_identity_and_capabilities() {
    let p = PluginProcess::start(fixture(), quiet()).unwrap();
    let info = p.info();
    assert_eq!(
        (info.kind, info.name.as_str(), info.protocol_version),
        (Kind::Source, "fixture", 1)
    );
    assert!(p.has(CAP_SESSIONS));
}

#[test]
fn execute_streams_a_result_set_or_reports_no_result() {
    let mut p = opened();
    let (e, values) = collect(&mut p, "rows 7", None);
    assert!(matches!(e, Execution::Result { rows: 7, .. }));
    assert_eq!(values, vec![0, 1, 2, 3, 4, 5, 6]);

    let (e, values) = collect(&mut p, "rows 0", None);
    match e {
        Execution::Result { rows: 0, schema } => assert_eq!(schema.field(0).name(), "n"),
        other => panic!("expected an empty result, got {other:?}"),
    }
    assert!(values.is_empty());

    let (e, _) = collect(&mut p, "none", None);
    assert!(matches!(
        e,
        Execution::NoResult {
            rows_affected: Some(0)
        }
    ));
}

#[test]
fn row_limit_truncates_the_result() {
    let mut p = opened();
    let (e, values) = collect(&mut p, "rows 10", Some(4));
    assert!(matches!(e, Execution::Result { rows: 4, .. }));
    assert_eq!(values, vec![0, 1, 2, 3]);
    // The session is still usable afterwards.
    assert!(matches!(
        collect(&mut p, "rows 1", None).0,
        Execution::Result { rows: 1, .. }
    ));
}

#[test]
fn plugin_errors_come_back_as_messages_and_the_session_survives() {
    let mut p = opened();
    let err = p.execute("fail", None, |_, _| Ok(())).unwrap_err();
    assert!(err.to_string().contains("fixture can't run `fail`"), "{err}");
    assert!(p.check("bad").unwrap_err().to_string().contains("bad statement"));
    p.check("select 1").unwrap();
    assert!(matches!(
        collect(&mut p, "rows 2", None).0,
        Execution::Result { rows: 2, .. }
    ));
}

#[test]
fn a_failed_open_is_reported() {
    let mut p = PluginProcess::start(fixture(), quiet()).unwrap();
    let mut conn = Map::new();
    conn.insert("fail".into(), json!(true));
    assert!(
        p.open(conn, false)
            .unwrap_err()
            .to_string()
            .contains("can't connect")
    );
}

#[test]
fn an_incompatible_version_names_both_ranges() {
    let err = start_mode("old_protocol", Duration::from_secs(10)).err().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("7..=9") && msg.contains("0..=1"), "{msg}");
}

#[test]
fn a_protocol_zero_plugin_is_told_to_update() {
    let mut p =
        PluginProcess::spawn_env(fixture(), quiet(), &[("DRE_FIXTURE_MODE", "protocol_zero")]).unwrap();
    let err = p
        .handshake(
            (dre_protocol::CORE_MIN_VERSION, MAX_VERSION),
            Duration::from_secs(10),
        )
        .err()
        .unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("0..=0") && msg.contains("update the plugin"),
        "{msg}"
    );
}

#[test]
fn the_sdk_still_speaks_protocol_zero_to_an_older_core() {
    let mut p = PluginProcess::spawn(fixture(), quiet()).unwrap();
    p.handshake((0, 0), Duration::from_secs(10)).unwrap();
    assert_eq!(p.info().protocol_version, 0);
    p.open(Map::new(), false).unwrap();
    let (_, values) = collect(&mut p, "rows 2", None);
    assert_eq!(values, vec![0, 1]);
}

fn recording() -> (LogSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let l = lines.clone();
    let sink: LogSink = Arc::new(move |_, line| l.lock().unwrap().push(line.to_string()));
    (sink, lines)
}

#[test]
fn log_and_progress_messages_reach_the_log_sink() {
    let (sink, lines) = recording();
    let mut p = PluginProcess::start(fixture(), sink).unwrap();
    p.open(Map::new(), false).unwrap();
    p.execute("slog hello there", None, |_, _| Ok(())).unwrap();
    p.execute("progress", None, |_, _| Ok(())).unwrap();
    let lines = lines.lock().unwrap().clone();
    assert!(
        lines.contains(&"info: hello there attempt=2".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"info: reading (2/2)".to_string()), "{lines:?}");
}

#[test]
fn an_error_carries_its_kind_and_namespaced_code() {
    let mut p = opened();
    match p.execute("coded", None, |_, _| Ok(())).unwrap_err() {
        HostError::Plugin {
            kind, code, message, ..
        } => {
            assert_eq!(kind.as_deref(), Some("auth"));
            assert_eq!(code.as_deref(), Some("fixture/bad-token"));
            assert_eq!(message, "the token was refused");
        }
        e => panic!("{e:?}"),
    }
    // Uncoded errors stay plain.
    match p.execute("fail", None, |_, _| Ok(())).unwrap_err() {
        HostError::Plugin { kind, code, .. } => assert_eq!((kind, code), (None, None)),
        e => panic!("{e:?}"),
    }
}

#[test]
fn a_cancelled_request_replies_cancelled_and_the_session_survives() {
    let (sink, lines) = recording();
    let mut p = PluginProcess::start(fixture(), sink).unwrap();
    p.open(Map::new(), false).unwrap();
    let canceller = p.canceller();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        assert!(canceller.cancel());
    });
    let started = std::time::Instant::now();
    let err = p.execute("sleep 20", None, |_, _| Ok(())).unwrap_err();
    t.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    match err {
        HostError::Plugin { kind, .. } => assert_eq!(kind.as_deref(), Some("cancelled")),
        e => panic!("{e:?}"),
    }
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "warning: fixture: cancel hook")
    );
    // The next request runs normally.
    let (_, values) = collect(&mut p, "rows 1", None);
    assert_eq!(values, vec![0]);
}

#[test]
fn a_crash_mid_request_is_a_clear_error_with_the_exit_status() {
    let mut p = opened();
    let err = p.execute("crash", None, |_, _| Ok(())).unwrap_err();
    assert!(matches!(err, HostError::Crashed { .. }), "{err:?}");
    assert!(err.to_string().contains("exited unexpectedly"), "{err}");
}

#[test]
fn a_plugin_that_dies_at_start_reports_its_stderr() {
    let err = start_mode("die", Duration::from_secs(10)).err().unwrap();
    assert!(err.to_string().contains("boom: fixture died on purpose"), "{err}");
}

#[test]
fn a_panic_is_reported_not_hung_on() {
    let mut p = opened();
    let err = p.execute("panic", None, |_, _| Ok(())).unwrap_err();
    assert!(err.to_string().contains("fixture panic"), "{err}");
}

#[test]
fn a_malformed_frame_is_a_protocol_error() {
    let err = start_mode("garbage", Duration::from_secs(10)).err().unwrap();
    assert!(matches!(err, HostError::Malformed { .. }), "{err:?}");
}

#[test]
fn a_silent_plugin_times_out_instead_of_hanging() {
    let err = start_mode("silent", Duration::from_millis(300)).err().unwrap();
    assert!(matches!(err, HostError::Timeout { .. }), "{err:?}");
}

#[test]
fn plugin_stderr_reaches_the_log_sink() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let log: LogSink = Arc::new(move |plugin, line| sink.lock().unwrap().push(format!("{plugin}: {line}")));
    let mut p = PluginProcess::start(fixture(), log).unwrap();
    p.open(Map::new(), false).unwrap();
    p.execute("log hello from the plugin", None, |_, _| Ok(()))
        .unwrap();
    p.close().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("hello from the plugin") && l.starts_with("dre-source-fixture")),
        "{lines:?}"
    );
}

#[test]
fn describe_lists_connection_fields() {
    let mut p = PluginProcess::start(fixture(), quiet()).unwrap();
    let fields = p.describe().unwrap();
    assert_eq!(
        fields
            .iter()
            .map(|f| (f.name.as_str(), f.secret, f.manual))
            .collect::<Vec<_>>(),
        vec![
            ("path", false, false),
            ("fail", false, true),
            ("token", true, false),
            ("token_text", true, true)
        ]
    );
}
