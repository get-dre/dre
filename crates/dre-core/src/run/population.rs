//! A per-row population's disk index. Arrow batches stay in the query spool; only one batch
//! per query and one bounded chunk of row jobs are resident while an output runs.

use std::collections::HashMap;
use std::fs::File;
use std::sync::Mutex;

use arrow::array::{RecordBatch, UInt32Array};
use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::FileReader;
use arrow::util::display::{ArrayFormatter, FormatOptions};
use rusqlite::{Connection, OptionalExtension, params};

use super::Produced;
use crate::engine::CancelToken;

pub(super) const CHUNK: usize = 128;

pub(super) struct Table {
    pub name: String,
    pub schema: SchemaRef,
    reader: Mutex<Reader>,
}

struct Reader {
    file: FileReader<File>,
    batch: Option<(usize, RecordBatch)>,
}

impl Table {
    fn open(result: &Produced) -> Result<Self, String> {
        let file = FileReader::try_new(File::open(&result.spool).map_err(|e| e.to_string())?, None)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            name: result.query.clone(),
            schema: result.schema.clone(),
            reader: Mutex::new(Reader { file, batch: None }),
        })
    }

    fn batch(&self, index: usize) -> Result<RecordBatch, String> {
        let mut reader = self.reader.lock().unwrap();
        if let Some((i, batch)) = &reader.batch
            && *i == index
        {
            return Ok(batch.clone());
        }
        reader.file.set_index(index).map_err(|e| e.to_string())?;
        let batch = reader
            .file
            .next()
            .ok_or("missing batch in population spool")?
            .map_err(|e| e.to_string())?;
        reader.batch = Some((index, batch.clone()));
        Ok(batch)
    }
}

pub(super) struct Population {
    pub data: Vec<Table>,
    driver: Table,
    db: Mutex<Connection>,
    len: usize,
    pub orphan_rows: u64,
    pub orphan_keys: u64,
    pub orphan_samples: Vec<String>,
    // Keep the directory alive until SQLite and all its readers have closed.
    _dir: tempfile::TempDir,
}

impl Population {
    pub fn build(
        driver: &Produced,
        data: &[&Produced],
        key: &[String],
        cancel: &CancelToken,
    ) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut db = Connection::open(dir.path().join("population.db")).map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-1024; PRAGMA temp_store=FILE;
            CREATE TABLE driver (i INTEGER PRIMARY KEY, key TEXT UNIQUE, shown TEXT, batch INTEGER, row INTEGER, name TEXT, selected INTEGER DEFAULT 0);
            CREATE INDEX driver_shown ON driver(shown);
            CREATE TABLE detail (q INTEGER, key TEXT, batch INTEGER, row INTEGER);
            CREATE INDEX detail_key ON detail(q,key);
            CREATE TABLE names (name TEXT PRIMARY KEY, key TEXT);
            CREATE TABLE orphans (key TEXT PRIMARY KEY, shown TEXT);
            CREATE TABLE jobs (i INTEGER PRIMARY KEY, job TEXT);")
            .map_err(|e| e.to_string())?;
        let mut len = 0;
        let mut orphan_rows = 0;
        let mut orphan_samples = Vec::new();
        {
            let transaction = db.transaction().map_err(|e| e.to_string())?;
            scan(driver, key, cancel, |batch, row, index, shown| {
                let prior: Option<usize> = transaction
                    .query_row("SELECT i FROM driver WHERE key=?1", [index], |r| r.get(0))
                    .optional()
                    .map_err(|e| e.to_string())?;
                if let Some(first) = prior {
                    return Err(format!(
                        "the driver `{}` has the key `{shown}` twice (rows {} and {}); a per-row output needs one driver row per key",
                        driver.query,
                        first + 1,
                        len + 1
                    ));
                }
                transaction
                    .execute(
                        "INSERT INTO driver(i,key,shown,batch,row) VALUES(?1,?2,?3,?4,?5)",
                        params![len, index, shown, batch, row],
                    )
                    .map_err(|e| e.to_string())?;
                len += 1;
                Ok(())
            })?;
            for (q, result) in data.iter().enumerate() {
                scan(result, key, cancel, |batch, row, index, shown| {
                    let present: bool = transaction
                        .query_row("SELECT EXISTS(SELECT 1 FROM driver WHERE key=?1)", [index], |r| {
                            r.get(0)
                        })
                        .map_err(|e| e.to_string())?;
                    if present {
                        transaction
                            .execute(
                                "INSERT INTO detail VALUES(?1,?2,?3,?4)",
                                params![q, index, batch, row],
                            )
                            .map_err(|e| e.to_string())?;
                    } else {
                        orphan_rows += 1;
                        transaction
                            .execute(
                                "INSERT OR IGNORE INTO orphans VALUES(?1,?2)",
                                params![index, shown],
                            )
                            .map_err(|e| e.to_string())?;
                        if orphan_samples.len() < 10 && !orphan_samples.iter().any(|s| s == shown) {
                            orphan_samples.push(shown.to_string());
                        }
                    }
                    Ok(())
                })?;
            }
            transaction.commit().map_err(|e| e.to_string())?;
        }
        let orphan_keys = db
            .query_row("SELECT COUNT(*) FROM orphans", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            driver: Table::open(driver)?,
            data: data.iter().map(|p| Table::open(p)).collect::<Result<_, _>>()?,
            db: Mutex::new(db),
            len,
            orphan_rows,
            orphan_keys,
            orphan_samples,
            _dir: dir,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn key(&self, i: usize) -> Result<String, String> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT shown FROM driver WHERE i=?1", [i], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    pub fn contains(&self, key: &str) -> Result<bool, String> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT EXISTS(SELECT 1 FROM driver WHERE shown=?1)", [key], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())
    }

    pub fn select(&self, i: usize, name: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let key = self.key_unlocked(&db, i)?;
        let prior: Option<String> = db
            .query_row(
                "SELECT key FROM names WHERE name=?1",
                [name.to_lowercase()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(first) = prior {
            return Err(format!(
                "the rows with keys `{first}` and `{key}` would both write `{name}`; make `file_name` (or the key) unique per row"
            ));
        }
        db.execute(
            "INSERT INTO names VALUES(?1,?2)",
            params![name.to_lowercase(), key],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE driver SET selected=1,name=?1 WHERE i=?2",
            params![name, i],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn key_unlocked(&self, db: &Connection, i: usize) -> Result<String, String> {
        db.query_row("SELECT shown FROM driver WHERE i=?1", [i], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    pub fn selected(&self, after: usize) -> Result<Vec<(usize, String)>, String> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT i,name FROM driver WHERE selected=1 AND i>=?1 ORDER BY i LIMIT ?2")
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![after, CHUNK], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn driver_row(&self, i: usize) -> Result<RecordBatch, String> {
        let (batch, row): (usize, usize) = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT batch,row FROM driver WHERE i=?1", [i], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| e.to_string())?;
        Ok(self.driver.batch(batch)?.slice(row, 1))
    }

    pub fn slice_len(&self, i: usize, q: usize) -> Result<u64, String> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM detail WHERE q=?1 AND key=(SELECT key FROM driver WHERE i=?2)",
                params![q, i],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub fn slices(&self, i: usize) -> Result<Vec<RecordBatch>, String> {
        let mut out = Vec::with_capacity(self.data.len());
        for (q, table) in self.data.iter().enumerate() {
            let locations: Vec<(usize, u32)> = {
                let db = self.db.lock().unwrap();
                let mut stmt = db.prepare("SELECT batch,row FROM detail WHERE q=?1 AND key=(SELECT key FROM driver WHERE i=?2) ORDER BY batch,row").map_err(|e| e.to_string())?;
                stmt.query_map(params![q, i], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?
            };
            let mut batches = Vec::new();
            let mut grouped: HashMap<usize, Vec<u32>> = HashMap::new();
            for (batch, row) in locations {
                grouped.entry(batch).or_default().push(row);
            }
            let mut indexes: Vec<usize> = grouped.keys().copied().collect();
            indexes.sort_unstable();
            for index in indexes {
                let batch = table.batch(index)?;
                let rows = UInt32Array::from(grouped.remove(&index).expect("indexed above"));
                batches.push(arrow::compute::take_record_batch(&batch, &rows).map_err(|e| e.to_string())?);
            }
            out.push(arrow::compute::concat_batches(&table.schema, &batches).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn save_job(&self, i: usize, job: &str) -> Result<(), String> {
        self.db
            .lock()
            .unwrap()
            .execute("INSERT OR REPLACE INTO jobs VALUES(?1,?2)", params![i, job])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn jobs(&self, after: usize) -> Result<Vec<(usize, String)>, String> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT i,job FROM jobs WHERE i>=?1 ORDER BY i LIMIT ?2")
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![after, CHUNK], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())
    }
}

fn scan(
    result: &Produced,
    keys: &[String],
    cancel: &CancelToken,
    mut row: impl FnMut(usize, usize, &str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let reader = FileReader::try_new(File::open(&result.spool).map_err(|e| e.to_string())?, None)
        .map_err(|e| e.to_string())?;
    let columns: Vec<usize> = keys.iter().map(|key| result.schema.index_of(key).map_err(|_| format!("`{}` has no column `{key}`, the `for_each` key; every query of a per-row output needs the key columns (its columns: {})",result.query,result.schema.fields().iter().map(|f|f.name().as_str()).collect::<Vec<_>>().join(", ")))).collect::<Result<_,_>>()?;
    for (index, batch) in reader.enumerate() {
        let batch = batch.map_err(|e| e.to_string())?;
        let options = FormatOptions::default().with_null("");
        let formats: Vec<ArrayFormatter<'_>> = columns
            .iter()
            .map(|&c| ArrayFormatter::try_new(batch.column(c).as_ref(), &options))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        for r in 0..batch.num_rows() {
            if cancel.is_cancelled() {
                return Err("population indexing was cancelled".into());
            }
            let parts: Vec<String> = formats.iter().map(|f| f.value(r).to_string()).collect();
            row(index, r, &parts.join("\u{1f}"), &parts.join("_"))?;
        }
    }
    Ok(())
}
