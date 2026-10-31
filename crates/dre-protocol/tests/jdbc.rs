//! The Java source through the ordinary host and language-neutral conformance suite.
//! Set DRE_TEST_JDBC_BIN to its built launcher. Oracle cases additionally take
//! DRE_TEST_JDBC_URL, DRE_TEST_JDBC_JAR, DRE_TEST_JDBC_USER and DRE_TEST_JDBC_PASSWORD.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow::array::{Array, Decimal128Array, StringArray, TimestampMillisecondArray};
use arrow::datatypes::{DataType, TimeUnit};
use dre_protocol::conformance::{self, Options, Session};
use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_LOAD, CAP_READ_ONLY, CAP_SESSIONS};
use serde_json::{Value, json};

fn binary() -> Option<PathBuf> {
    let path = std::env::var_os("DRE_TEST_JDBC_BIN").map(PathBuf::from);
    if path.is_none() {
        eprintln!("skipped: set DRE_TEST_JDBC_BIN to the built Java source launcher");
    }
    path
}

fn connection() -> Option<Value> {
    let url = std::env::var("DRE_TEST_JDBC_URL").ok()?;
    Some(json!({
        "url": url,
        "driver_jars": [std::env::var("DRE_TEST_JDBC_JAR").expect("Oracle fixture needs its driver JAR")],
        "properties": {
            "user": std::env::var("DRE_TEST_JDBC_USER").unwrap_or_else(|_| "dre_fixture".into()),
            "password": std::env::var("DRE_TEST_JDBC_PASSWORD").expect("Oracle fixture needs a password"),
            "oracle.jdbc.fanEnabled": false,
        },
    }))
}

fn start(path: &std::path::Path) -> PluginProcess {
    PluginProcess::start(path, Arc::new(|_, _| {})).unwrap()
}

const ROWS: &str = "select cast(level as number(18,0)) as \"n\" from dual connect by level <= 10";

#[test]
fn jdbc_conforms_to_the_shared_protocol_and_advertises_only_implemented_capabilities() {
    let Some(path) = binary() else { return };
    let options = match connection() {
        Some(config) => Options::default().session(
            Session::new(config.as_object().unwrap().clone())
                .statement("rows", ROWS)
                .statement(
                    "empty",
                    "select cast(1 as number(18,0)) as \"n\" from dual where 1=0",
                )
                .statement("no_result", "alter session set nls_date_format = 'YYYY-MM-DD'")
                .statement("error", "select * from dre_conformance_missing")
                .statement("slow", "begin dbms_session.sleep(30); end;"),
        ),
        None => Options::default(),
    };
    conformance::assert_conforms_with(&path, &options);
    let mut plugin = start(&path);
    assert!(plugin.has(CAP_SESSIONS));
    assert!(!plugin.has(CAP_CHECK) && !plugin.has(CAP_LOAD) && !plugin.has(CAP_READ_ONLY));
    let description = plugin.description().unwrap();
    assert!(
        description
            .connection_fields
            .iter()
            .any(|field| field.name == "url" && field.secret)
    );
    assert!(
        description
            .connection_fields
            .iter()
            .any(|field| field.name == "properties" && field.secret)
    );
    plugin.close().unwrap();
}

#[test]
fn oracle_preparation_and_typed_results_share_one_session() {
    let (Some(path), Some(config)) = (binary(), connection()) else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    let prepared = plugin
        .execute(
            "alter session set nls_date_format = 'YYYY/MM/DD'",
            None,
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(matches!(prepared, Execution::NoResult { .. }));
    let mut batches = Vec::new();
    let result = plugin.execute(
        "select cast('000019' as varchar2(20)) as \"id\", cast(12.34 as number(18,2)) as \"amount\", cast(date '2026-01-31' as timestamp(3)) as \"opened\", to_char(date '2026-01-31') as \"prepared\" from dual",
        None,
        |_, batch| { batches.push(batch); Ok(()) },
    ).unwrap();
    assert!(matches!(result, Execution::Result { rows: 1, .. }));
    let batch = &batches[0];
    let strings = |index| {
        batch
            .column(index)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
    };
    assert_eq!(strings(0).value(0), "000019");
    assert_eq!(strings(3).value(0), "2026/01/31");
    let amount = batch
        .column(1)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    assert_eq!(amount.value(0), 1234);
    assert_eq!(amount.data_type(), &DataType::Decimal128(18, 2));
    assert_eq!(
        batch.column(2).data_type(),
        &DataType::Timestamp(TimeUnit::Millisecond, None)
    );
    let opened = batch.column(2).as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
    assert_eq!(opened.value(0), 1_769_817_600_000);
    plugin.close().unwrap();
}

#[test]
fn oracle_accepts_a_complete_url_and_explicit_driver_class() {
    let (Some(path), Some(mut config)) = (binary(), connection()) else {
        return;
    };
    let url = config["url"].as_str().unwrap().replace(
        "jdbc:oracle:thin:@",
        &format!(
            "jdbc:oracle:thin:{}/{}@",
            config["properties"]["user"].as_str().unwrap(),
            config["properties"]["password"].as_str().unwrap(),
        ),
    );
    config["url"] = json!(url);
    config["properties"] = json!({});
    config["driver_class"] = json!("oracle.jdbc.OracleDriver");
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    assert!(matches!(
        plugin.execute(ROWS, Some(2), |_, _| Ok(())).unwrap(),
        Execution::Result { rows: 2, .. }
    ));
    plugin.close().unwrap();
}

#[test]
fn oracle_failures_do_not_expose_connection_properties_in_diagnostics() {
    let (Some(path), Some(mut config)) = (binary(), connection()) else {
        return;
    };
    config["properties"]["password"] = json!("DRE_JDBC_PASSWORD_SENTINEL");
    config["properties"]["arbitraryVendorCredential"] = json!("DRE_VENDOR_SECRET_SENTINEL");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&logs);
    let log: LogSink = Arc::new(move |_, line| captured.lock().unwrap().push(line.to_string()));
    let mut plugin = PluginProcess::start(&path, log).unwrap();
    let error = plugin
        .open(config.as_object().unwrap().clone(), false)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("SENTINEL"), "{error}");
    assert!(error.contains("SQLSTATE") && error.contains("1017"), "{error}");
    plugin.close().unwrap();
    assert!(!logs.lock().unwrap().join("\n").contains("SENTINEL"));
}

#[test]
fn oracle_refuses_implicit_decimal_or_timestamp_precision_loss() {
    let (Some(path), Some(config)) = (binary(), connection()) else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    for sql in [
        "select 1 as \"unconstrained\" from dual",
        "select timestamp '2026-01-31 01:02:03.123456' as \"precise\" from dual",
    ] {
        let error = plugin.execute(sql, None, |_, _| Ok(())).unwrap_err().to_string();
        assert!(error.contains("cast"), "{error}");
    }
    plugin
        .execute(ROWS, Some(0), |_, batch| {
            assert_eq!(batch.num_rows(), 0);
            Ok(())
        })
        .unwrap();
    plugin.close().unwrap();
}

#[test]
fn oracle_streams_multiple_batches_and_truncates_only_the_requested_row_count() {
    let (Some(path), Some(config)) = (binary(), connection()) else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    let mut counts = Vec::new();
    let result = plugin
        .execute(
            "select cast(level as number(18,0)) as \"n\" from dual connect by level <= 10000",
            Some(9000),
            |_, batch| {
                counts.push(batch.num_rows());
                let numbers = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Decimal128Array>()
                    .unwrap();
                if counts.len() == 2 {
                    assert_eq!(numbers.value(batch.num_rows() - 1), 9000);
                }
                Ok(())
            },
        )
        .unwrap();
    assert!(matches!(result, Execution::Result { rows: 9000, .. }));
    assert_eq!(counts, [8192, 808]);
    plugin.close().unwrap();
}

#[test]
fn cancelling_an_oracle_statement_leaves_the_session_usable() {
    let (Some(path), Some(config)) = (binary(), connection()) else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    let canceller = plugin.canceller();
    let cancellation = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        canceller.cancel()
    });
    let started = std::time::Instant::now();
    let error = plugin
        .execute("begin dbms_session.sleep(30); end;", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(cancellation.join().unwrap(), "a running request was cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(matches!(
        plugin.execute(ROWS, Some(1), |_, _| Ok(())).unwrap(),
        Execution::Result { rows: 1, .. }
    ));
    plugin.close().unwrap();
}
