//! Saved query results: each query's result as the exact Arrow IPC stream core received
//! (`<name>.arrows`), plus an index (`results.json`). `dre run --output-json <DIR>` writes them
//! beside the typed JSON; `dre run --from-results <DIR>` replays them to the format plugins
//! without running any query. Types (decimals, timezones, nulls) stay exact, and the files open
//! directly in DuckDB, Polars or pandas.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::{FileReader, StreamReader};
use arrow::ipc::writer::{FileWriter, StreamWriter};
use serde::{Deserialize, Serialize};

/// The index file's name.
pub const INDEX: &str = "results.json";
pub const SCHEMA_VERSION: &str = "dre/saved-results/v1";

/// `results.json`: the saved results of one Binding's run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub schema_version: String,
    /// The run that saved them.
    pub run_id: String,
    pub results: Vec<SavedResult>,
}

/// One query's saved result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedResult {
    pub query: String,
    /// Its tab name.
    pub name: String,
    pub connection: String,
    pub rows: u64,
    /// The SHA-256 of the query's rendered SQL, to warn when it changed since.
    #[serde(default)]
    pub sql_sha256: Option<String>,
    /// The Arrow IPC stream, relative to the index.
    pub file: String,
}

impl Index {
    pub fn find(&self, query: &str) -> Option<&SavedResult> {
        self.results.iter().find(|r| r.query == query)
    }
}

/// Write the index into `dir`.
pub fn write_index(dir: &Path, index: &Index) -> std::io::Result<()> {
    crate::fs::write_atomic(&dir.join(INDEX), serde_json::to_string_pretty(index)?.as_bytes())
}

/// Read `dir`'s index. The error is a sentence.
pub fn read_index(dir: &Path) -> Result<Index, String> {
    let path = dir.join(INDEX);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let index: Index =
        serde_json::from_str(&text).map_err(|e| format!("{} is malformed: {e}", path.display()))?;
    if index.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "{} is `{}`, which this DRE doesn't read (it reads `{SCHEMA_VERSION}`)",
            path.display(),
            index.schema_version
        ));
    }
    Ok(index)
}

/// Copy a spool (an Arrow IPC file) to `to` as an Arrow IPC stream.
pub fn spool_to_stream(spool: &Path, to: &Path) -> Result<(), String> {
    let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let schema = reader.schema();
    let mut w = StreamWriter::try_new(
        BufWriter::new(File::create(to).map_err(|e| e.to_string())?),
        &schema,
    )
    .map_err(|e| e.to_string())?;
    for b in reader {
        w.write(&b.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    w.finish().map_err(|e| e.to_string())
}

/// Copy a saved stream back into a spool (an Arrow IPC file): its schema and row count.
pub fn stream_to_spool(from: &Path, spool: &Path) -> Result<(SchemaRef, u64), String> {
    let reader = StreamReader::try_new(
        BufReader::new(File::open(from).map_err(|e| format!("can't read {}: {e}", from.display()))?),
        None,
    )
    .map_err(|e| format!("{} isn't an Arrow IPC stream: {e}", from.display()))?;
    let schema = reader.schema();
    let mut w = FileWriter::try_new(File::create(spool).map_err(|e| e.to_string())?, &schema)
        .map_err(|e| e.to_string())?;
    let mut rows = 0u64;
    for b in reader {
        let b = b.map_err(|e| format!("{}: {e}", from.display()))?;
        rows += b.num_rows() as u64;
        w.write(&b).map_err(|e| e.to_string())?;
    }
    w.finish().map_err(|e| e.to_string())?;
    Ok((schema, rows))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Decimal128Array, RecordBatch, StringArray};

    use super::*;

    #[test]
    fn a_result_round_trips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let batch = RecordBatch::try_from_iter([
            (
                "amount",
                Arc::new(
                    Decimal128Array::from(vec![Some(12345), None])
                        .with_precision_and_scale(18, 2)
                        .unwrap(),
                ) as ArrayRef,
            ),
            (
                "name",
                Arc::new(StringArray::from(vec![Some("a"), Some("b")])) as ArrayRef,
            ),
        ])
        .unwrap();
        let spool = dir.path().join("0.arrow");
        let mut w = FileWriter::try_new(File::create(&spool).unwrap(), &batch.schema()).unwrap();
        w.write(&batch).unwrap();
        w.finish().unwrap();
        let stream = dir.path().join("q.arrows");
        spool_to_stream(&spool, &stream).unwrap();
        let back = dir.path().join("1.arrow");
        let (schema, rows) = stream_to_spool(&stream, &back).unwrap();
        assert_eq!(rows, 2);
        assert_eq!(schema, batch.schema());
        let read: Vec<RecordBatch> = FileReader::try_new(File::open(&back).unwrap(), None)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(read, vec![batch]);
    }
}
