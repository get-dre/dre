//! Moves Arrow data between the `arrow` DRE is built on and the (older) `arrow` that `duckdb`
//! carries. The two are separate crates with separate types, so a batch can't be passed across
//! directly. Both implement the Arrow C data interface, a fixed C layout that doesn't change
//! between versions, so the data moves through it without being copied.

use arrow::array::{Array, RecordBatch, StructArray};
use arrow::datatypes::Schema;
use arrow::ffi::{FFI_ArrowArray, FFI_ArrowSchema};
use duckdb::arrow as duck;

/// A batch from DuckDB, as the `arrow` DRE uses.
pub fn from_duckdb(batch: &duck::array::RecordBatch) -> Result<RecordBatch, String> {
    use duck::array::Array as _;
    let (mut array, mut schema) = duck::ffi::to_ffi(&duck::array::StructArray::from(batch.clone()).to_data())
        .map_err(|e| e.to_string())?;
    // SAFETY: both crates define these structs from the same C layout. `from_raw` moves the
    // contents out and leaves an empty struct behind, which the old crate drops harmlessly.
    let (array, schema) = unsafe {
        (
            FFI_ArrowArray::from_raw(std::ptr::from_mut(&mut array).cast()),
            FFI_ArrowSchema::from_raw(std::ptr::from_mut(&mut schema).cast()),
        )
    };
    // SAFETY: `array` and `schema` were just exported from a valid array.
    let data = unsafe { arrow::ffi::from_ffi(array, &schema) }.map_err(|e| e.to_string())?;
    Ok(RecordBatch::from(StructArray::from(data)))
}

/// A batch from DRE, as the `arrow` DuckDB uses.
pub fn to_duckdb(batch: &RecordBatch) -> Result<duck::array::RecordBatch, String> {
    let (mut array, mut schema) =
        arrow::ffi::to_ffi(&StructArray::from(batch.clone()).to_data()).map_err(|e| e.to_string())?;
    // SAFETY: as in `from_duckdb`.
    let (array, schema) = unsafe {
        (
            duck::ffi::FFI_ArrowArray::from_raw(std::ptr::from_mut(&mut array).cast()),
            duck::ffi::FFI_ArrowSchema::from_raw(std::ptr::from_mut(&mut schema).cast()),
        )
    };
    // SAFETY: `array` and `schema` were just exported from a valid array.
    let data = unsafe { duck::ffi::from_ffi(array, &schema) }.map_err(|e| e.to_string())?;
    Ok(duck::array::RecordBatch::from(duck::array::StructArray::from(
        data,
    )))
}

/// A schema from DuckDB, as the `arrow` DRE uses.
pub fn schema_from_duckdb(schema: &duck::datatypes::Schema) -> Result<Schema, String> {
    let mut ffi = duck::ffi::FFI_ArrowSchema::try_from(schema).map_err(|e| e.to_string())?;
    // SAFETY: as in `from_duckdb`.
    let ffi = unsafe { FFI_ArrowSchema::from_raw(std::ptr::from_mut(&mut ffi).cast()) };
    Schema::try_from(&ffi).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_batch_survives_the_round_trip() {
        use arrow::array::{Date32Array, Float64Array, Int64Array, StringArray};
        use arrow::datatypes::{DataType, Field};

        let schema = Arc::new(Schema::new(vec![
            Field::new("n", DataType::Int64, true),
            Field::new("s", DataType::Utf8, true),
            Field::new("f", DataType::Float64, false),
            Field::new("d", DataType::Date32, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1), None, Some(3)])),
                Arc::new(StringArray::from(vec![Some("a"), Some("héllo"), None])),
                Arc::new(Float64Array::from(vec![1.5, -2.0, 0.0])),
                Arc::new(Date32Array::from(vec![Some(19000), None, Some(0)])),
            ],
        )
        .unwrap();

        let there = to_duckdb(&batch).unwrap();
        assert_eq!(there.num_rows(), 3);
        let back = from_duckdb(&there).unwrap();
        assert_eq!(back, batch);
    }

    #[test]
    fn a_schema_survives_the_conversion() {
        use arrow::datatypes::{DataType, Field};
        use duck::datatypes as old;

        let there = old::Schema::new(vec![
            old::Field::new("n", old::DataType::Int64, true),
            old::Field::new("s", old::DataType::Utf8, false),
        ]);
        let expected = Schema::new(vec![
            Field::new("n", DataType::Int64, true),
            Field::new("s", DataType::Utf8, false),
        ]);
        assert_eq!(schema_from_duckdb(&there).unwrap(), expected);
    }
}
