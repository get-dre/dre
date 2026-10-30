//! Per-row outputs (`for_each`): the pieces that don't need a run. The report's queries run once;
//! the driver query gives one row per copy of the output, and every other query of the output is
//! split by the same key, so each copy gets only its own slice. See `run.rs` for the run itself.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use arrow::array::{RecordBatch, UInt32Array};
use arrow::datatypes::SchemaRef;
use arrow::util::display::{ArrayFormatter, FormatOptions};

/// Joins the parts of a composite key in an index (never in a value).
const SEP: char = '\u{1f}';

/// A query's rows, in one batch, with its key column positions.
pub struct Table {
    pub name: String,
    pub batch: RecordBatch,
}

impl Table {
    /// One batch from a result's batches (an empty one keeps the schema).
    pub fn new(name: &str, schema: &SchemaRef, batches: &[RecordBatch]) -> Result<Table, String> {
        let batch = arrow::compute::concat_batches(schema, batches).map_err(|e| e.to_string())?;
        Ok(Table {
            name: name.to_string(),
            batch,
        })
    }

    fn key_columns(&self, key: &[String]) -> Result<Vec<usize>, String> {
        key.iter()
            .map(|k| {
                self.batch.schema().index_of(k).map_err(|_| {
                    format!(
                        "`{}` has no column `{k}`, the `for_each` key; every query of a per-row output needs the key columns (its columns: {})",
                        self.name,
                        self.batch
                            .schema()
                            .fields()
                            .iter()
                            .map(|f| f.name().as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            })
            .collect()
    }

    /// Every row's key: (index form, shown form `a_b`).
    fn keys(&self, cols: &[usize]) -> Result<Vec<(String, String)>, String> {
        let opts = FormatOptions::default().with_null("");
        let fmts: Vec<ArrayFormatter<'_>> = cols
            .iter()
            .map(|&c| ArrayFormatter::try_new(self.batch.column(c).as_ref(), &opts))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        Ok((0..self.batch.num_rows())
            .map(|r| {
                let parts: Vec<String> = fmts.iter().map(|f| f.value(r).to_string()).collect();
                (parts.join(&SEP.to_string()), parts.join("_"))
            })
            .collect())
    }
}

/// The population of a per-row output.
pub struct Population {
    pub driver: Table,
    /// Each driver row's key, shown (`a_b` for a composite key), in driver order.
    pub keys: Vec<String>,
    /// Each data query, and its rows by key (index form).
    pub data: Vec<(Table, HashMap<String, Vec<u32>>)>,
    /// The driver rows' keys (index form), in driver order.
    index_keys: Vec<String>,
    /// Rows of data queries whose key isn't in the driver: how many, how many keys, and up to
    /// 10 of them.
    pub orphan_rows: u64,
    pub orphan_keys: u64,
    pub orphan_samples: Vec<String>,
}

impl Population {
    /// Index the driver and each data query by `key`. A duplicate driver key is an error.
    pub fn build(driver: Table, data: Vec<Table>, key: &[String]) -> Result<Population, String> {
        let cols = driver.key_columns(key)?;
        let keys = driver.keys(&cols)?;
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, (k, shown)) in keys.iter().enumerate() {
            if let Some(first) = seen.insert(k.as_str(), i) {
                return Err(format!(
                    "the driver `{}` has the key `{shown}` twice (rows {} and {}); a per-row output needs one driver row per key",
                    driver.name,
                    first + 1,
                    i + 1
                ));
            }
        }
        let mut orphan_rows = 0;
        let mut orphans: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut orphan_samples = Vec::new();
        let mut indexed = Vec::new();
        for t in data {
            let cols = t.key_columns(key)?;
            let mut index: HashMap<String, Vec<u32>> = HashMap::new();
            for (r, (k, shown)) in t.keys(&cols)?.into_iter().enumerate() {
                if !seen.contains_key(k.as_str()) {
                    orphan_rows += 1;
                    orphans.insert(k.clone());
                    if orphan_samples.len() < 10 && !orphan_samples.contains(&shown) {
                        orphan_samples.push(shown);
                    }
                    continue;
                }
                index.entry(k).or_default().push(r as u32);
            }
            indexed.push((t, index));
        }
        let (index_keys, keys) = keys.into_iter().unzip();
        Ok(Population {
            driver,
            keys,
            data: indexed,
            index_keys,
            orphan_rows,
            orphan_keys: orphans.len() as u64,
            orphan_samples,
        })
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Row `i`'s slice of each data query, in order.
    pub fn slices(&self, i: usize) -> Result<Vec<RecordBatch>, String> {
        let k = &self.index_keys[i];
        self.data
            .iter()
            .map(|(t, index)| {
                let rows = index.get(k).map(Vec::as_slice).unwrap_or(&[]);
                take(&t.batch, rows)
            })
            .collect()
    }

    /// How many rows of data query `q` row `i` has.
    pub fn slice_len(&self, i: usize, q: usize) -> u64 {
        self.data[q]
            .1
            .get(&self.index_keys[i])
            .map_or(0, |r| r.len() as u64)
    }

    /// Row `i` of the driver, as a one-row batch.
    pub fn driver_row(&self, i: usize) -> RecordBatch {
        self.driver.batch.slice(i, 1)
    }
}

fn take(batch: &RecordBatch, rows: &[u32]) -> Result<RecordBatch, String> {
    let idx = UInt32Array::from(rows.to_vec());
    arrow::compute::take_record_batch(batch, &idx).map_err(|e| e.to_string())
}

/// A key as a file name: letters and digits of any script, `.`, `-` and `_` kept, anything else
/// `_`; never empty, `.` or `..`, at most 100 bytes (on a character boundary), so the extension
/// still fits in a file system's 255-byte limit.
pub fn safe_name(key: &str) -> String {
    let mut s = String::new();
    for c in key.chars() {
        let c = if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '_'
        };
        if s.len() + c.len_utf8() > 100 {
            break;
        }
        s.push(c);
    }
    if s.is_empty() || s.chars().all(|c| c == '.') {
        s = format!("_{s}");
    }
    s
}

/// Files of two rows that would collide: (file, first key, second key).
pub fn collision<'a>(
    names: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<(String, String, String)> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for (key, file) in names {
        if let Some(prev) = seen.insert(file.to_lowercase(), key) {
            return Some((file.to_string(), prev.to_string(), key.to_string()));
        }
    }
    None
}

/// A per-row value as logs and results show it: an email keeps its first character and domain
/// (`j***@example.com`), anything else is `*****`.
pub fn mask_value(v: &str) -> String {
    if let Some((user, domain)) = v.split_once('@')
        && !user.is_empty()
        && domain.contains('.')
        && !v.contains(char::is_whitespace)
    {
        let first: String = user.chars().take(1).collect();
        return format!("{first}***@{domain}");
    }
    crate::secrets::MASK.to_string()
}

/// Masks a row's values wherever they appear in a text (a rendered path, a plugin's error).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RowMask {
    /// (value, its mask), longest first.
    values: Arc<Vec<(String, String)>>,
}

impl RowMask {
    /// The row's string values worth hiding: every `each.*` value but the key's, at least three
    /// characters long (shorter ones would mask parts of unrelated words).
    pub fn new(each: &serde_json::Map<String, serde_json::Value>, key: &[String]) -> RowMask {
        let mut values: Vec<(String, String)> = each
            .iter()
            .filter(|(k, _)| !key.contains(k))
            .filter_map(|(_, v)| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| s.chars().count() >= 3)
            .map(|s| {
                let m = mask_value(&s);
                (s, m)
            })
            .collect();
        values.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        values.dedup();
        RowMask {
            values: Arc::new(values),
        }
    }

    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (v, m) in self.values.iter() {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), m);
            }
        }
        out
    }

    /// Mask every string in a JSON value.
    #[must_use]
    pub fn apply_json(&self, v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::String(s) => serde_json::Value::String(self.apply(s)),
            serde_json::Value::Array(a) => {
                serde_json::Value::Array(a.iter().map(|x| self.apply_json(x)).collect())
            }
            serde_json::Value::Object(o) => {
                serde_json::Value::Object(o.iter().map(|(k, x)| (k.clone(), self.apply_json(x))).collect())
            }
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Int64Array, StringArray};

    fn table(name: &str, ids: &[i64], other: &[&str]) -> Table {
        let b = RecordBatch::try_from_iter([
            ("member_id", Arc::new(Int64Array::from(ids.to_vec())) as ArrayRef),
            ("v", Arc::new(StringArray::from(other.to_vec())) as ArrayRef),
        ])
        .unwrap();
        Table::new(name, &b.schema(), &[b]).unwrap()
    }

    #[test]
    fn slices_by_key_and_counts_orphans() {
        let driver = table("members", &[1, 2, 3], &["a", "b", "c"]);
        let tx = table("tx", &[2, 1, 2, 9, 9], &["x", "y", "z", "o1", "o2"]);
        let p = Population::build(driver, vec![tx], &["member_id".into()]).unwrap();
        assert_eq!(p.keys, vec!["1", "2", "3"]);
        let s = p.slices(1).unwrap();
        let v = s[0].column(1).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(v.iter().map(Option::unwrap).collect::<Vec<_>>(), vec!["x", "z"]);
        assert_eq!(p.slices(2).unwrap()[0].num_rows(), 0);
        assert_eq!((p.orphan_rows, p.orphan_samples), (2, vec!["9".to_string()]));
    }

    #[test]
    fn composite_and_duplicate_keys() {
        let d = table("members", &[1, 1], &["a", "b"]);
        let p = Population::build(d, vec![], &["member_id".into(), "v".into()]).unwrap();
        assert_eq!(p.keys, vec!["1_a", "1_b"]);
        let d = table("members", &[1, 1], &["a", "a"]);
        let e = Population::build(d, vec![], &["member_id".into()]).err().unwrap();
        assert!(e.contains("has the key `1` twice (rows 1 and 2)"), "{e}");
        let d = table("members", &[1], &["a"]);
        let e = Population::build(d, vec![], &["nope".into()]).err().unwrap();
        assert!(e.contains("has no column `nope`"), "{e}");
    }

    #[test]
    fn file_names_are_safe_and_collisions_found() {
        assert_eq!(safe_name("ab/../c d"), "ab_.._c_d");
        assert_eq!(safe_name(".."), "_..");
        assert_eq!(safe_name(""), "_");
        // Keys in any script keep their letters, so `東京` and `大阪` get files of their own.
        assert_eq!(safe_name("東京"), "東京");
        assert_eq!(safe_name("Zoë/Ødegaard 東京"), "Zoë_Ødegaard_東京");
        assert_eq!(safe_name("株式会社 ABC"), "株式会社_ABC");
        assert_eq!(collision([("東京", "東京.csv"), ("大阪", "大阪.csv")]), None);
        // Cut at 100 bytes, never inside a character: 33 three-byte characters.
        let long = safe_name(&"東".repeat(50));
        assert_eq!(long, "東".repeat(33));
        assert_eq!(safe_name(&"a".repeat(150)).len(), 100);
        assert_eq!(
            collision([("a b", "a_b.csv"), ("a/b", "a_b.csv")]),
            Some(("a_b.csv".into(), "a b".into(), "a/b".into()))
        );
        assert_eq!(collision([("a", "a.csv"), ("b", "b.csv")]), None);
    }

    #[test]
    fn masking() {
        assert_eq!(mask_value("jane.doe@example.com"), "j***@example.com");
        assert_eq!(mask_value("Jane Doe"), "*****");
        let each = serde_json::json!({"member_id": 12345, "email": "jane@example.com", "name": "Jane Doe", "x": "ab"});
        let m = RowMask::new(each.as_object().unwrap(), &["member_id".into()]);
        assert_eq!(
            m.apply("to jane@example.com for Jane Doe, member 12345, ab"),
            "to j***@example.com for *****, member 12345, ab"
        );
    }
}
