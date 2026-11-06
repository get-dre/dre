//! One query's result as templates see it: `results.<query>` and its rows.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::ipc::reader::FileReader;
use minijinja::value::{Enumerator, Object, ObjectRepr, Value};

use crate::dates::Calendar;

/// One query's result as message templates see it.
#[derive(Debug)]
pub struct QueryResult {
    pub columns: Arc<Vec<String>>,
    /// At most the cap's rows.
    pub rows: Vec<Vec<Value>>,
    /// Every row the query returned.
    pub row_count: u64,
}

impl QueryResult {
    /// Read up to `max_rows` rows of a spooled result set.
    pub fn read(spool: &Path, row_count: u64, max_rows: u64, cal: Calendar) -> Result<QueryResult, String> {
        let reader = FileReader::try_new(File::open(spool).map_err(|e| e.to_string())?, None)
            .map_err(|e| e.to_string())?;
        let columns = Arc::new(
            reader
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().to_string())
                .collect(),
        );
        let mut rows = Vec::new();
        for b in reader {
            if rows.len() as u64 >= max_rows {
                break;
            }
            let b = b.map_err(|e| e.to_string())?;
            let left = (max_rows - rows.len() as u64) as usize;
            let b = if b.num_rows() > left { b.slice(0, left) } else { b };
            rows.extend(crate::values::typed_rows(&b, cal));
        }
        Ok(QueryResult {
            columns,
            rows,
            row_count,
        })
    }

    pub fn capped(&self) -> bool {
        (self.rows.len() as u64) < self.row_count
    }

    /// The template value: an object with `value`, `first`, `rows`, `row_count`, `columns` and
    /// `sets`.
    pub fn value(self: Arc<Self>) -> Value {
        Value::from_object(ResultValue(self))
    }

    fn row(&self, i: usize) -> Option<Value> {
        self.rows
            .get(i)
            .map(|r| Row::value(self.columns.clone(), r.clone()))
    }
}

#[derive(Debug)]
struct ResultValue(Arc<QueryResult>);

impl Object for ResultValue {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let r = &self.0;
        Some(match key.as_str()? {
            "value" => r
                .rows
                .first()
                .and_then(|row| row.first().cloned())
                .unwrap_or(Value::from(())),
            "first" => r.row(0).unwrap_or(Value::from(())),
            "rows" => Value::from((0..r.rows.len()).filter_map(|i| r.row(i)).collect::<Vec<_>>()),
            "row_count" => Value::from(r.row_count),
            "columns" => Value::from(r.columns.as_ref().clone()),
            // One .sql file makes one result set (its last statement's), so `sets` holds it once.
            "sets" => Value::from(vec![Value::from_object(ResultValue(r.clone()))]),
            _ => return None,
        })
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Str(&["value", "first", "rows", "row_count", "columns", "sets"])
    }
}

/// One `run_query()` row: accessible by column name (`row.region`, `row['region']`) or index.
#[derive(Debug)]
pub struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<Value>,
}

impl Row {
    pub fn value(columns: Arc<Vec<String>>, values: Vec<Value>) -> Value {
        Value::from_object(Row { columns, values })
    }
}

impl Object for Row {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        if let Some(name) = key.as_str() {
            let i = self.columns.iter().position(|c| c == name)?;
            return self.values.get(i).cloned();
        }
        self.values.get(key.as_usize()?).cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Seq(self.values.len())
    }
}
