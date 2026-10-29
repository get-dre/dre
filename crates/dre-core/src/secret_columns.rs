//! `secret_columns:` on a query entry: the values of those columns are secrets (a PDF or zip
//! password). As a result arrives, its secret values join [`crate::secrets`], so every surface
//! that masks `env_var()` secrets masks them too: the console, `dre.log`, `run_results.json`,
//! `rows.jsonl`, previews and errors. `--output-json` (which dre-mcp reads) gets the columns
//! themselves replaced by `*****`. Delivered files and messages keep the real values.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;

use crate::secrets::MASK;

/// Every value of `columns` in `batch`, as text.
pub fn values(batch: &RecordBatch, columns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for c in columns {
        let Some(a) = batch.column_by_name(c) else {
            continue;
        };
        let Ok(text) = arrow::compute::cast(a, &DataType::Utf8) else {
            continue;
        };
        let Some(text) = text.as_any().downcast_ref::<StringArray>() else {
            continue;
        };
        out.extend(text.iter().flatten().map(str::to_string));
    }
    out
}

/// Mask the values of `columns` in the spooled result from now on.
pub fn register(spool: &Path, columns: &[String], secrets: &crate::secrets::Secrets) -> Result<(), String> {
    if columns.is_empty() {
        return Ok(());
    }
    let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    for b in reader {
        secrets.register_values(values(&b.map_err(|e| e.to_string())?, columns));
    }
    Ok(())
}

/// `batch` with each of `columns` replaced by `*****` (nulls stay null), as text.
pub fn mask_batch(batch: &RecordBatch, columns: &[String]) -> Result<RecordBatch, String> {
    let schema = batch.schema();
    let mut fields = Vec::with_capacity(schema.fields().len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns());
    for (f, a) in schema.fields().iter().zip(batch.columns()) {
        if columns.contains(f.name()) {
            let masked: StringArray = (0..a.len()).map(|i| (!a.is_null(i)).then_some(MASK)).collect();
            fields.push(Arc::new(Field::new(f.name(), DataType::Utf8, true)));
            arrays.push(Arc::new(masked));
        } else {
            fields.push(f.clone());
            arrays.push(a.clone());
        }
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| e.to_string())
}

/// Copy the spool `from` to `to` with `columns` masked.
pub fn masked_copy(from: &Path, to: &Path, columns: &[String]) -> Result<(), String> {
    let reader =
        FileReader::try_new(File::open(from).map_err(|e| e.to_string())?, None).map_err(|e| e.to_string())?;
    let schema = reader.schema();
    let empty = mask_batch(&RecordBatch::new_empty(schema), columns)?;
    let mut w = FileWriter::try_new(File::create(to).map_err(|e| e.to_string())?, &empty.schema())
        .map_err(|e| e.to_string())?;
    for b in reader {
        w.write(&mask_batch(&b.map_err(|e| e.to_string())?, columns)?)
            .map_err(|e| e.to_string())?;
    }
    w.finish().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;

    #[test]
    fn values_and_masking_keep_other_columns_and_nulls() {
        let b = RecordBatch::try_from_iter([
            ("id", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef),
            (
                "pw",
                Arc::new(StringArray::from(vec![Some("s3cret"), None])) as ArrayRef,
            ),
            ("pin", Arc::new(Int64Array::from(vec![123456, 7])) as ArrayRef),
        ])
        .unwrap();
        let cols = vec!["pw".to_string(), "pin".to_string(), "missing".to_string()];
        assert_eq!(values(&b, &cols), vec!["s3cret", "123456", "7"]);
        let m = mask_batch(&b, &cols).unwrap();
        let pw = m
            .column_by_name("pw")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(pw.value(0), MASK);
        assert!(pw.is_null(1));
        assert_eq!(m.column_by_name("pin").unwrap().data_type(), &DataType::Utf8);
        assert_eq!(m.column_by_name("id").unwrap().data_type(), &DataType::Int64);
    }
}
