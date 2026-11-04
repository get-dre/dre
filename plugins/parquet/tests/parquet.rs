use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int64Array, RecordBatch,
    StringArray, TimestampMicrosecondArray,
};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use serde_json::json;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-parquet"))
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn types_and_values_survive_a_round_trip() {
    let b1 = RecordBatch::try_from_iter([
        ("id", Arc::new(Int64Array::from(vec![Some(1), None])) as ArrayRef),
        (
            "name",
            Arc::new(StringArray::from(vec![Some("Acme Corp"), Some("Client A")])) as ArrayRef,
        ),
        (
            "amount",
            Arc::new(
                Decimal128Array::from(vec![10050, -325])
                    .with_precision_and_scale(12, 2)
                    .unwrap(),
            ) as ArrayRef,
        ),
        ("day", Arc::new(Date32Array::from(vec![20478, 0])) as ArrayRef),
        (
            "at",
            Arc::new(TimestampMicrosecondArray::from(vec![1, 2]).with_timezone("UTC")) as ArrayRef,
        ),
    ])
    .unwrap();
    let b2 = b1.slice(1, 1);
    round_trip(vec![b1, b2]);
}

fn round_trip(batches: Vec<RecordBatch>) {
    let schema = batches[0].schema();
    let expected = arrow::compute::concat_batches(&schema, &batches).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.parquet");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let meta = ResultSetMeta {
        name: "x".into(),
        query: "x".into(),
        result_index: 1,
        anchor: None,
        header: None,
        columns: Default::default(),
        autofit: None,
        style: None,
    };
    p.write_begin(
        path.to_str().unwrap(),
        "parquet",
        Default::default(),
        vec![meta],
        None,
    )
    .unwrap();
    p.write_result_set(&schema, batches).unwrap();
    assert_eq!(
        p.write_finish().unwrap(),
        vec![path.to_str().unwrap().to_string()]
    );

    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap()).unwrap();
    for group in builder.metadata().row_groups() {
        for column in group.columns() {
            assert_eq!(column.compression(), Compression::SNAPPY);
        }
    }
    let reader = builder.build().unwrap();
    let back: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    let all = arrow::compute::concat_batches(&schema, &back).unwrap();
    assert_eq!(all.schema(), schema);
    assert_eq!(all, expected);
}

#[test]
fn nullable_edge_values_and_empty_batches_round_trip() {
    let batch = RecordBatch::try_from_iter([
        (
            "id",
            Arc::new(Int64Array::from(vec![Some(i64::MIN), None, Some(i64::MAX)])) as ArrayRef,
        ),
        (
            "value",
            Arc::new(Float64Array::from(vec![Some(f64::MIN), None, Some(f64::MAX)])) as ArrayRef,
        ),
        (
            "flag",
            Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])) as ArrayRef,
        ),
        (
            "text",
            Arc::new(StringArray::from(vec![Some("日本語\n😀"), None, Some("")])) as ArrayRef,
        ),
        (
            "bytes",
            Arc::new(BinaryArray::from(vec![
                Some(b"\x00\xff".as_slice()),
                None,
                Some(b"".as_slice()),
            ])) as ArrayRef,
        ),
        (
            "all_null",
            Arc::new(StringArray::from(vec![None::<&str>; 3])) as ArrayRef,
        ),
        (
            "day",
            Arc::new(Date32Array::from(vec![Some(-1), None, Some(0)])) as ArrayRef,
        ),
        (
            "at",
            Arc::new(
                TimestampMicrosecondArray::from(vec![Some(-1), None, Some(0)])
                    .with_timezone("Australia/Sydney"),
            ) as ArrayRef,
        ),
        (
            "amount",
            Arc::new(
                Decimal128Array::from(vec![Some(-999999999999999999999999999999_i128), None, Some(0)])
                    .with_precision_and_scale(30, 10)
                    .unwrap(),
            ) as ArrayRef,
        ),
    ])
    .unwrap();
    round_trip(vec![batch.slice(0, 0), batch.clone(), batch.slice(0, 0)]);
    round_trip(vec![batch.slice(0, 0)]);
}

#[test]
fn multiple_reader_batches_keep_row_order() {
    let batch = RecordBatch::try_from_iter([
        ("id", Arc::new(Int64Array::from_iter_values(0..4097)) as ArrayRef),
        (
            "text",
            Arc::new(StringArray::from_iter_values(
                (0..4097).map(|i| format!("row {i}")),
            )) as ArrayRef,
        ),
    ])
    .unwrap();
    round_trip(vec![batch.slice(0, 2000), batch.slice(2000, 2097)]);
}

#[test]
fn options_including_compression_are_rejected() {
    let log: LogSink = Arc::new(|_, _| {});
    let mut plugin = PluginProcess::start(bin(), log).unwrap();
    assert!(plugin.validate(Default::default()).unwrap().is_empty());
    for (key, value) in [
        ("compression", json!("snappy")),
        ("compression", json!("gzip")),
        ("compression", json!(42)),
        ("typo", json!(true)),
    ] {
        let options = [(key.to_string(), value)].into_iter().collect();
        assert_eq!(
            plugin.validate(options).unwrap(),
            vec![format!(
                "the `parquet` format takes no options, but got `{key}`; check the key's spelling"
            )]
        );
    }
}
