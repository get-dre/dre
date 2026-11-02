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
