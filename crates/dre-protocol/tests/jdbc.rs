//! The Java source through the ordinary host and language-neutral conformance suite.
//! Set `DRE_TEST_JDBC_BIN` to its built launcher. A database run also sets
//! `DRE_TEST_JDBC_DATABASE=oracle|sqlserver`, `DRE_TEST_JDBC_URL`, `DRE_TEST_JDBC_JAR`,
//! `DRE_TEST_JDBC_USER` and `DRE_TEST_JDBC_PASSWORD`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow::array::{Array, BinaryArray, Decimal128Array, StringArray, TimestampMillisecondArray};
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

#[derive(Clone, Copy)]
enum Database {
    Oracle,
    SqlServer,
}

impl Database {
    fn name(self) -> &'static str {
        match self {
            Database::Oracle => "oracle",
            Database::SqlServer => "sqlserver",
        }
    }

    fn rows(self) -> &'static str {
        match self {
            Database::Oracle => {
                "select cast(level as number(18,0)) as \"n\" from dual connect by level <= 10"
            }
            Database::SqlServer => {
                "select cast(v as decimal(18,0)) as n from (values (1),(2),(3)) x(v) order by v"
            }
        }
    }

    fn empty(self) -> &'static str {
        match self {
            Database::Oracle => "select cast(1 as number(18,0)) as \"n\" from dual where 1=0",
            Database::SqlServer => "select cast(1 as decimal(18,0)) as n where 1=0",
        }
    }

    fn no_result(self) -> &'static str {
        match self {
            Database::Oracle => "alter session set nls_date_format = 'YYYY-MM-DD'",
            Database::SqlServer => "set nocount on",
        }
    }

    fn slow(self) -> &'static str {
        match self {
            Database::Oracle => "begin dbms_session.sleep(30); end;",
            Database::SqlServer => "waitfor delay '00:00:30'",
        }
    }

    fn relation_preparation(self) -> &'static [&'static str] {
        match self {
            Database::Oracle => &[
                "delete from dre_fixture.dre_fixture_data",
                "insert into dre_fixture.dre_fixture_data values ('000019', '1234567890123456789012345678901234567890', 12.34, 12345678901234567890.123456789012345678, timestamp '2026-01-31 01:02:03.123', '2026/01/31', hextoraw('002AFF'), null)",
                "insert into dre_fixture.dre_fixture_data values ('000020', '9876543210987654321098765432109876543210', 0.00, -0.000000000000000001, timestamp '2026-02-01 00:00:00.000', '2026/02/01', null, 99.99)",
            ],
            Database::SqlServer => &[
                "create table #dre_fixture (account_id varchar(20) not null, wide_id varchar(40) not null, amount decimal(18,2) not null, wide_amount decimal(38,18) not null, opened datetime2(3) not null, prepared varchar(10) not null, bytes varbinary(8) null, optional decimal(18,2) null)",
                "insert into #dre_fixture values ('000019', '1234567890123456789012345678901234567890', 12.34, 12345678901234567890.123456789012345678, '2026-01-31T01:02:03.123', '2026/01/31', 0x002AFF, null), ('000020', '9876543210987654321098765432109876543210', 0.00, -0.000000000000000001, '2026-02-01T00:00:00.000', '2026/02/01', null, 99.99)",
            ],
        }
    }

    fn auth_code(self) -> &'static str {
        match self {
            Database::Oracle => "1017",
            Database::SqlServer => "18456",
        }
    }

    fn precision_sql(self) -> &'static [&'static str] {
        match self {
            Database::Oracle => &[
                "select 1 as \"unconstrained\" from dual",
                "select timestamp '2026-01-31 01:02:03.123456' as \"precise\" from dual",
            ],
            Database::SqlServer => &["select cast('2026-01-31T01:02:03.1234' as datetime2(4)) as precise"],
        }
    }

    fn driver_class(self) -> &'static str {
        match self {
            Database::Oracle => "oracle.jdbc.OracleDriver",
            Database::SqlServer => "com.microsoft.sqlserver.jdbc.SQLServerDriver",
        }
    }
}

fn database() -> Option<Database> {
    let Some(name) = std::env::var_os("DRE_TEST_JDBC_DATABASE") else {
        let names = [
            "DRE_TEST_JDBC_URL",
            "DRE_TEST_JDBC_JAR",
            "DRE_TEST_JDBC_USER",
            "DRE_TEST_JDBC_PASSWORD",
        ];
        if names.iter().any(|name| std::env::var_os(name).is_some()) {
            panic!("set DRE_TEST_JDBC_DATABASE with the connection fields");
        }
        return None;
    };
    match name.to_str() {
        Some("oracle") => Some(Database::Oracle),
        Some("sqlserver") => Some(Database::SqlServer),
        _ => panic!("DRE_TEST_JDBC_DATABASE must be `oracle` or `sqlserver`"),
    }
}

fn connection(db: Database) -> Value {
    let names = [
        "DRE_TEST_JDBC_URL",
        "DRE_TEST_JDBC_JAR",
        "DRE_TEST_JDBC_USER",
        "DRE_TEST_JDBC_PASSWORD",
    ];
    let [Some(url), Some(jar), Some(user), Some(password)] = names.map(|name| std::env::var(name).ok())
    else {
        panic!(
            "set DRE_TEST_JDBC_URL, DRE_TEST_JDBC_JAR, DRE_TEST_JDBC_USER and DRE_TEST_JDBC_PASSWORD for {}",
            db.name()
        );
    };
    let mut properties = json!({"user": user, "password": password});
    match db {
        Database::Oracle => properties["oracle.jdbc.fanEnabled"] = json!(false),
        Database::SqlServer => {
            properties["databaseName"] = json!("master");
            properties["encrypt"] = json!(false);
            properties["trustServerCertificate"] = json!(true);
        }
    }
    json!({
        "url": url,
        "driver_jars": [jar],
        "properties": properties,
    })
}

fn required_connection() -> Option<(PathBuf, Database, Value)> {
    let path = binary()?;
    let db = database()?;
    Some((path, db, connection(db)))
}

fn start(path: &std::path::Path) -> PluginProcess {
    PluginProcess::start(path, Arc::new(|_, _| {})).unwrap()
}

#[test]
fn jdbc_conforms_to_the_shared_protocol_and_advertises_only_implemented_capabilities() {
    let Some(path) = binary() else { return };
    let options = match database() {
        Some(db) => Options::default().session(
            Session::new(connection(db).as_object().unwrap().clone())
                .statement("rows", db.rows())
                .statement("empty", db.empty())
                .statement("no_result", db.no_result())
                .statement("error", "select * from dre_conformance_missing")
                .statement("slow", db.slow()),
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
fn accepts_complete_url_and_minimal_url_with_properties() {
    let Some((path, db, mut config)) = required_connection() else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    assert!(matches!(
        plugin.execute(db.rows(), Some(2), |_, _| Ok(())).unwrap(),
        Execution::Result { rows: 2, .. }
    ));
    plugin.close().unwrap();

    let user = config["properties"]["user"].as_str().unwrap();
    let password = config["properties"]["password"].as_str().unwrap();
    let url = match db {
        Database::Oracle => config["url"].as_str().unwrap().replace(
            "jdbc:oracle:thin:@",
            &format!("jdbc:oracle:thin:{user}/{password}@"),
        ),
        Database::SqlServer => format!(
            "{};databaseName=master;user={user};password={password};encrypt=false;trustServerCertificate=true",
            config["url"].as_str().unwrap()
        ),
    };
    config["url"] = json!(url);
    config["properties"] = json!({});
    config["driver_class"] = json!(db.driver_class());
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    assert!(matches!(
        plugin.execute(db.rows(), Some(2), |_, _| Ok(())).unwrap(),
        Execution::Result { rows: 2, .. }
    ));
    plugin.close().unwrap();
}

#[test]
fn failures_do_not_expose_connection_properties_in_diagnostics() {
    let Some((path, db, mut config)) = required_connection() else {
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
    assert!(
        error.contains("SQLSTATE") && error.contains(db.auth_code()),
        "{error}"
    );
    plugin.close().unwrap();
    assert!(!logs.lock().unwrap().join("\n").contains("SENTINEL"));
}

#[test]
fn refuses_implicit_timestamp_precision_loss_and_zero_limit_is_empty() {
    let Some((path, db, config)) = required_connection() else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    for sql in db.precision_sql() {
        let error = plugin.execute(sql, None, |_, _| Ok(())).unwrap_err().to_string();
        assert!(error.contains("cast"), "{error}");
    }
    plugin
        .execute(db.rows(), Some(0), |_, batch| {
            assert_eq!(batch.num_rows(), 0);
            Ok(())
        })
        .unwrap();
    plugin.close().unwrap();
}

#[test]
fn cancelling_a_statement_leaves_the_session_usable() {
    let Some((path, db, config)) = required_connection() else {
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
    let error = plugin.execute(db.slow(), None, |_, _| Ok(())).unwrap_err();
    assert!(cancellation.join().unwrap(), "a running request was cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(matches!(
        plugin.execute(db.rows(), Some(1), |_, _| Ok(())).unwrap(),
        Execution::Result { rows: 1, .. }
    ));
    plugin.close().unwrap();
}

#[test]
fn oracle_date_mapping_cannot_silently_discard_time_of_day() {
    let Some(path) = binary() else {
        return;
    };
    let Some(db) = database() else {
        return;
    };
    if !matches!(db, Database::Oracle) {
        return;
    }
    let mut config = connection(db);
    config["properties"]["oracle.jdbc.mapDateToTimestamp"] = json!(false);
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    let error = plugin
        .execute(
            "select to_date('2026-01-31 12:34:56', 'YYYY-MM-DD HH24:MI:SS') as \"opened\" from dual",
            None,
            |_, _| Ok(()),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("TIMESTAMP(3)"), "{error}");
    let mut opened = None;
    plugin
        .execute(
            "select cast(to_date('2026-01-31 12:34:56', 'YYYY-MM-DD HH24:MI:SS') as timestamp(3)) as \"opened\" from dual",
            None,
            |_, batch| {
                opened = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .map(|values| values.value(0));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(opened, Some(1_769_862_896_000));
    plugin.close().unwrap();
}

#[test]
fn preparation_and_typed_results_share_one_session() {
    let Some((path, db, config)) = required_connection() else {
        return;
    };
    let mut plugin = start(&path);
    plugin.open(config.as_object().unwrap().clone(), false).unwrap();
    let query = match db {
        Database::Oracle => {
            "select account_id, wide_id, amount, wide_amount, opened, prepared, bytes, optional_amount as optional from dre_fixture.dre_fixture_data order by account_id"
        }
        Database::SqlServer => {
            "select account_id, wide_id, amount, wide_amount, opened, prepared, bytes, optional from #dre_fixture order by account_id"
        }
    };
    let mut batches = Vec::new();
    for statement in db.relation_preparation() {
        plugin.execute(statement, None, |_, _| Ok(())).unwrap();
    }
    assert!(matches!(
        plugin
            .execute(query, None, |_, batch| {
                batches.push(batch);
                Ok(())
            })
            .unwrap(),
        Execution::Result { rows: 2, .. }
    ));
    let batch = &batches[0];
    let strings = |index| {
        batch
            .column(index)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
    };
    assert_eq!(strings(0).value(0), "000019");
    assert_eq!(strings(0).value(1), "000020");
    assert_eq!(strings(1).value(0), "1234567890123456789012345678901234567890");
    assert_eq!(strings(1).value(1), "9876543210987654321098765432109876543210");
    assert_eq!(strings(5).value(0), "2026/01/31");
    assert_eq!(strings(5).value(1), "2026/02/01");
    let decimal = |index, precision, scale| {
        let values = batch
            .column(index)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        assert_eq!(values.data_type(), &DataType::Decimal128(precision, scale));
        values
    };
    let amount = decimal(2, 18, 2);
    assert_eq!(amount.value(0), 1234);
    assert_eq!(amount.value(1), 0);
    let wide = decimal(3, 38, 18);
    assert_eq!(wide.value(0), 12_345_678_901_234_567_890_123_456_789_012_345_678);
    assert_eq!(wide.value(1), -1);
    assert_eq!(
        batch.column(4).data_type(),
        &DataType::Timestamp(TimeUnit::Millisecond, None)
    );
    let opened = batch
        .column(4)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .unwrap();
    assert_eq!(opened.value(0), 1_769_821_323_123);
    assert_eq!(opened.value(1), 1_769_904_000_000);
    assert_eq!(batch.column(6).data_type(), &DataType::Binary);
    let bytes = batch.column(6).as_any().downcast_ref::<BinaryArray>().unwrap();
    assert_eq!(bytes.value(0), &[0x00, 0x2a, 0xff]);
    assert!(bytes.is_null(1));
    let optional = decimal(7, 18, 2);
    assert!(optional.is_null(0));
    assert_eq!(optional.value(1), 9999);
    plugin.close().unwrap();
}
