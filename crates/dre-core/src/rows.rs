//! Typed, streaming JSON row export for integrations. Decimals retain their exact text;
//! columns carry Arrow types so consumers need not infer types from values or CSV.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use arrow::ipc::reader::FileReader;
use serde_json::json;

pub fn write_spool(spool: &Path, output: &Path, row_count: u64) -> Result<(), String> {
    let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let columns = column_types(&reader.schema());
    let mut out = BufWriter::new(File::create(output).map_err(|e| e.to_string())?);
    write!(
        out,
        "{{\"schema_version\":\"dre/rows/v1\",\"columns\":{},\"row_count\":{},\"rows\":[",
        serde_json::to_string(&columns).map_err(|e| e.to_string())?,
        row_count
    )
    .map_err(|e| e.to_string())?;
    let mut first = true;
    for batch in reader {
        for row in crate::values::batch_rows(&batch.map_err(|e| e.to_string())?) {
            if !first {
                out.write_all(b",").map_err(|e| e.to_string())?;
            }
            first = false;
            serde_json::to_writer(&mut out, &row).map_err(|e| e.to_string())?;
        }
    }
    out.write_all(b"]}\n").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// Each column's name and Arrow type, as `dre/rows/v1` lists them.
pub fn column_types(schema: &arrow::datatypes::Schema) -> Vec<serde_json::Value> {
    schema
        .fields()
        .iter()
        .map(|f| json!({"name": f.name(), "type": f.data_type().to_string()}))
        .collect()
}
