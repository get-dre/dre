//! Zero-copy Arrow C data interface bridge to DuckDB's Arrow version.

use arrow::array::{Array, RecordBatch, StructArray};
use duckdb::arrow as duck;

/// A batch from DRE, as the `arrow` DuckDB uses.
pub fn to_duckdb(batch: &RecordBatch) -> Result<duck::array::RecordBatch, String> {
    let (mut array, mut schema) =
        arrow::ffi::to_ffi(&StructArray::from(batch.clone()).to_data()).map_err(|e| e.to_string())?;
    // SAFETY: both Arrow versions use the same C ABI. from_raw moves ownership
    // and leaves empty structs for the exporting version to drop.
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

/// A batch from DuckDB, as the `arrow` DRE uses.
pub fn from_duckdb(batch: &duck::array::RecordBatch) -> Result<RecordBatch, String> {
    use duck::array::Array as _;
    let (mut array, mut schema) = duck::ffi::to_ffi(&duck::array::StructArray::from(batch.clone()).to_data())
        .map_err(|e| e.to_string())?;
    // SAFETY: both crates define these structs from the same C layout. `from_raw` moves the
    // contents out and leaves an empty struct behind, which the old crate drops harmlessly.
    let (array, schema) = unsafe {
        (
            arrow::ffi::FFI_ArrowArray::from_raw(std::ptr::from_mut(&mut array).cast()),
            arrow::ffi::FFI_ArrowSchema::from_raw(std::ptr::from_mut(&mut schema).cast()),
        )
    };
    // SAFETY: `array` and `schema` were just exported from a valid array.
    let data = unsafe { arrow::ffi::from_ffi(array, &schema) }.map_err(|e| e.to_string())?;
    Ok(RecordBatch::from(StructArray::from(data)))
}
