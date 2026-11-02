use super::*;
use arrow::array::{Int64Array, RecordBatch};
use arrow::datatypes::{Field, Schema};
use arrow::ipc::writer::FileWriter;
use std::sync::Arc;

fn write_spool(path: &Path, batches: &[RecordBatch]) {
    let mut writer = FileWriter::try_new(File::create(path).unwrap(), &batches[0].schema()).unwrap();
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn scalar_values_types_and_nulls_survive_both_arrow_versions_and_spool_import() {
    let temp = tempfile::tempdir().unwrap();
    let spool = temp.path().join("result.arrow");
    let source = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = source.prepare("select true as flag, (-128)::tinyint as i8, (-32768)::smallint as i16, (-2147483648)::integer as i32, -9223372036854775807::bigint as i64, 255::utinyint as u8, 65535::usmallint as u16, 4294967295::uinteger as u32, 18446744073709551615::ubigint as u64, 1.25::float as f32, -2.5::double as f64, 'a''b' as \"quoted\"\"name\", '\\x00ff'::blob as binary, date '1969-12-31' as date, '1969-12-31 23:59:59'::timestamp_s as seconds, '1969-12-31 23:59:59.123'::timestamp_ms as millis, '1969-12-31 23:59:59.123456'::timestamp as micros, '1969-12-31 23:59:59.123456789'::timestamp_ns as nanos, 12345678901234567890.123456789012345678::decimal(38,18) as decimal_value").unwrap();
    let source_batch = bridge::from_duckdb(&stmt.query_arrow([]).unwrap().next().unwrap()).unwrap();
    let null_batch = RecordBatch::try_new(
        source_batch.schema(),
        source_batch
            .schema()
            .fields()
            .iter()
            .map(|f| arrow::array::new_null_array(f.data_type(), 1))
            .collect(),
    )
    .unwrap();
    // The bridge itself must retain the input schema and values, including nulls and slices.
    let sliced =
        arrow::compute::concat_batches(&source_batch.schema(), [&null_batch, &source_batch, &null_batch])
            .unwrap()
            .slice(1, 2);
    for batch in [&source_batch, &null_batch, &sliced] {
        assert_eq!(
            *batch,
            bridge::from_duckdb(&bridge::to_duckdb(batch).unwrap()).unwrap()
        );
    }
    write_spool(&spool, &[source_batch.clone(), null_batch.clone()]);
    let original = std::fs::read(&spool).unwrap();
    let conn = local_connection(temp.path()).unwrap();
    load_result(&conn, &spool, "quoted\"relation", &CancelToken::new()).unwrap();
    let mut stmt = conn
        .prepare("select * from \"quoted\"\"relation\" order by flag nulls last")
        .unwrap();
    let imported = bridge::from_duckdb(&stmt.query_arrow([]).unwrap().next().unwrap()).unwrap();
    let expected =
        arrow::compute::concat_batches(&source_batch.schema(), [&source_batch, &null_batch]).unwrap();
    assert_eq!(imported, expected);
    assert_eq!(std::fs::read(&spool).unwrap(), original);
}

#[test]
fn large_string_and_binary_offsets_preserve_values_and_original_types() {
    use arrow::array::{LargeBinaryArray, LargeStringArray};
    let temp = tempfile::tempdir().unwrap();
    let spool = temp.path().join("result.arrow");
    let schema = Arc::new(Schema::new(vec![
        Field::new("text", DataType::LargeUtf8, true),
        Field::new("bytes", DataType::LargeBinary, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(LargeStringArray::from(vec![Some("a'🦀"), None])),
            Arc::new(LargeBinaryArray::from(vec![Some(&[0, 255][..]), None])),
        ],
    )
    .unwrap();
    assert_eq!(
        batch,
        bridge::from_duckdb(&bridge::to_duckdb(&batch).unwrap()).unwrap()
    );
    write_spool(&spool, &[batch]);
    let before = std::fs::read(&spool).unwrap();
    let conn = local_connection(temp.path()).unwrap();
    load_result(&conn, &spool, "result", &CancelToken::new()).unwrap();
    let values: (String, String, i64) = conn.query_row(
        "select max(text), max(hex(bytes)), count(*) filter (where text is null and bytes is null) from result",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(values, ("a'🦀".into(), "00FF".into(), 1));
    assert_eq!(
        FileReader::try_new(File::open(&spool).unwrap(), None)
            .unwrap()
            .schema(),
        schema
    );
    assert_eq!(std::fs::read(&spool).unwrap(), before);
}

#[test]
fn unsupported_types_and_extensions_fail_explicitly_without_changing_spools() {
    use arrow::datatypes::TimeUnit;
    let mut fields: Vec<Field> = [
        DataType::Null,
        DataType::Date64,
        DataType::Time64(TimeUnit::Nanosecond),
        DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        DataType::Decimal128(10, -2),
        DataType::Decimal256(50, 2),
        DataType::List(Arc::new(Field::new("item", DataType::Int32, true))),
    ]
    .into_iter()
    .map(|ty| Field::new("unsupported", ty, true))
    .collect();
    fields.push(
        Field::new("extension", DataType::Int64, true)
            .with_metadata([("ARROW:extension:name".into(), "custom_type".into())].into()),
    );
    for field in fields {
        let temp = tempfile::tempdir().unwrap();
        let spool = temp.path().join("result.arrow");
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![field.clone()])),
            vec![arrow::array::new_null_array(field.data_type(), 1)],
        )
        .unwrap();
        write_spool(&spool, &[batch]);
        let original = std::fs::read(&spool).unwrap();
        let conn = local_connection(temp.path()).unwrap();
        let error = load_result(&conn, &spool, "result", &CancelToken::new()).unwrap_err();
        assert!(error.contains("local data tests cannot load column"), "{error}");
        assert!(error.contains(field.name()), "{error}");
        assert_eq!(std::fs::read(&spool).unwrap(), original);
    }
}

#[test]
fn cancelled_import_does_not_open_the_spool_or_create_a_relation() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let error = load_result(&conn, Path::new("missing.arrow"), "result", &cancel).unwrap_err();
    assert_eq!(error, "local data test cancelled");
    let count: i64 = conn
        .query_row("select count(*) from duckdb_tables()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[ignore = "bounded manual spill measurement; writes roughly 850 MB of Arrow input"]
fn bounded_large_spool_uses_disk_spill() {
    use arrow::array::{ArrayRef, StringArray};
    let started = Instant::now();
    let temp = tempfile::tempdir().unwrap();
    let spool = temp.path().join("result.arrow");
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("payload", DataType::Utf8, false),
    ]));
    let mut writer = FileWriter::try_new(File::create(&spool).unwrap(), &schema).unwrap();
    let suffix = "x".repeat(240);
    const ROWS: i64 = 3_145_728;
    for start in (0..ROWS).step_by(8192) {
        let ids = Int64Array::from_iter_values(start..start + 8192);
        let values: Vec<String> = (start..start + 8192)
            .map(|id| format!("{:016x}{suffix}", (id as u64).wrapping_mul(0x9e3779b97f4a7c15)))
            .collect();
        let strings = StringArray::from_iter_values(values.iter());
        writer
            .write(
                &RecordBatch::try_new(schema.clone(), vec![Arc::new(ids) as ArrayRef, Arc::new(strings)])
                    .unwrap(),
            )
            .unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    let conn = local_connection(temp.path()).unwrap();
    load_result(&conn, &spool, "result", &CancelToken::new()).unwrap();
    let profile = temp.path().join("profile.json");
    conn.execute_batch(&format!(
        "SET enable_profiling = 'json'; SET profiling_output = '{}'",
        profile.display()
    ))
    .unwrap();
    let mut stmt = conn
        .prepare("select id, payload from result order by payload")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut count = 0;
    while rows.next().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, ROWS);
    drop(rows);
    drop(stmt);
    let profile: serde_json::Value = serde_json::from_slice(&std::fs::read(profile).unwrap()).unwrap();
    let spill = profile["system_peak_temp_dir_size"].as_u64().unwrap();
    assert!(spill > 0, "no disk spill: {profile}");
    eprintln!(
        "rows={ROWS} payload_bytes=256 spool_bytes={} elapsed_s={:.3} peak_buffer_bytes={} peak_spill_bytes={spill}",
        std::fs::metadata(&spool).unwrap().len(),
        started.elapsed().as_secs_f64(),
        profile["system_peak_buffer_memory"]
    );
    drop(conn);
    temp.close().unwrap();
}

#[test]
fn loading_multiple_batches_and_empty_results_preserves_the_original_spool() {
    for empty in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let spool = temp.path().join("result.arrow");
        let schema = Arc::new(Schema::new(vec![Field::new("quoted\"id", DataType::Int64, true)]));
        let mut writer = FileWriter::try_new(File::create(&spool).unwrap(), &schema).unwrap();
        if !empty {
            for values in [vec![Some(1), None], vec![Some(i64::MAX)]] {
                writer
                    .write(
                        &RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(values))])
                            .unwrap(),
                    )
                    .unwrap();
            }
        }
        writer.finish().unwrap();
        drop(writer);
        let before = std::fs::read(&spool).unwrap();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        load_result(&conn, &spool, "dre_test_result", &CancelToken::new()).unwrap();
        let count: i64 = conn
            .query_row("select count(*) from dre_test_result", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, if empty { 0 } else { 3 });
        if !empty {
            let max: i64 = conn
                .query_row("select max(\"quoted\"\"id\") from dre_test_result", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(max, i64::MAX);
        }
        assert_eq!(std::fs::read(&spool).unwrap(), before);
    }
}
