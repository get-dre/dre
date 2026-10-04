//! Arrow values as Jinja values, for `run_query()` rows and message results.

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{
    DataType, Date32Type, Date64Type, Decimal128Type, Float32Type, Float64Type, Int8Type, Int16Type,
    Int32Type, Int64Type, TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
    TimestampSecondType, UInt8Type, UInt16Type, UInt32Type, UInt64Type,
};
use chrono::TimeZone;

use crate::dates::{Calendar, Date, DateTime};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use minijinja::Value;

/// Every row of `batch`, as a list of values per row.
pub fn batch_rows(batch: &RecordBatch) -> Vec<Vec<Value>> {
    let opts = FormatOptions::default();
    let cols: Vec<Box<dyn Fn(usize) -> Value + '_>> = batch
        .columns()
        .iter()
        .map(|c| column(c.as_ref(), &opts))
        .collect();
    (0..batch.num_rows())
        .map(|i| cols.iter().map(|f| f(i)).collect())
        .collect()
}

/// Like [`batch_rows`], keeping more types: dates and timestamps become DRE dates and datetimes
/// (in `cal`'s timezone), decimals become numbers.
pub fn typed_rows(batch: &RecordBatch, cal: Calendar) -> Vec<Vec<Value>> {
    let opts = FormatOptions::default();
    let cols: Vec<Box<dyn Fn(usize) -> Value + '_>> = batch
        .columns()
        .iter()
        .map(|c| typed_column(c.as_ref(), &opts, cal))
        .collect();
    (0..batch.num_rows())
        .map(|i| cols.iter().map(|f| f(i)).collect())
        .collect()
}

fn typed_column<'a>(
    a: &'a dyn Array,
    opts: &'a FormatOptions<'a>,
    cal: Calendar,
) -> Box<dyn Fn(usize) -> Value + 'a> {
    let nullable = move |f: Box<dyn Fn(usize) -> Value + 'a>| -> Box<dyn Fn(usize) -> Value + 'a> {
        Box::new(move |i| if a.is_null(i) { Value::from(()) } else { f(i) })
    };
    match a.data_type() {
        DataType::Date32 | DataType::Date64 => nullable(Box::new(move |i| match date_at(a, i) {
            Some(d) => Date::value(d, cal),
            None => Value::from(()),
        })),
        DataType::Timestamp(_, tz) => {
            let tz = tz.clone();
            nullable(Box::new(move |i| match timestamp_at(a, i) {
                Some(utc) => {
                    let utc = match tz {
                        Some(_) => utc,
                        // A naive timestamp is wall time in the run's zone.
                        None => match cal.tz.from_local_datetime(&utc.naive_utc()).earliest() {
                            Some(t) => t.with_timezone(&chrono::Utc),
                            None => utc,
                        },
                    };
                    DateTime::value(utc.with_timezone(&cal.tz), cal)
                }
                None => Value::from(()),
            }))
        }
        DataType::Decimal128(_, scale) => {
            let arr = a.as_primitive::<Decimal128Type>();
            let scale = *scale as i32;
            nullable(Box::new(move |i| {
                Value::from(arr.value(i) as f64 / 10f64.powi(scale))
            }))
        }
        _ => column(a, opts),
    }
}

fn date_at(a: &dyn Array, i: usize) -> Option<chrono::NaiveDate> {
    match a.data_type() {
        DataType::Date32 => a.as_primitive::<Date32Type>().value_as_date(i),
        DataType::Date64 => a.as_primitive::<Date64Type>().value_as_date(i),
        _ => None,
    }
}

fn timestamp_at(a: &dyn Array, i: usize) -> Option<chrono::DateTime<chrono::Utc>> {
    use arrow::datatypes::TimeUnit;
    let DataType::Timestamp(unit, _) = a.data_type() else {
        return None;
    };
    let naive = match unit {
        TimeUnit::Second => a.as_primitive::<TimestampSecondType>().value_as_datetime(i),
        TimeUnit::Millisecond => a.as_primitive::<TimestampMillisecondType>().value_as_datetime(i),
        TimeUnit::Microsecond => a.as_primitive::<TimestampMicrosecondType>().value_as_datetime(i),
        TimeUnit::Nanosecond => a.as_primitive::<TimestampNanosecondType>().value_as_datetime(i),
    }?;
    Some(naive.and_utc())
}

fn column<'a>(a: &'a dyn Array, opts: &'a FormatOptions<'a>) -> Box<dyn Fn(usize) -> Value + 'a> {
    macro_rules! prim {
        ($t:ty) => {{
            let arr = a.as_primitive::<$t>();
            Box::new(move |i| {
                if arr.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(arr.value(i))
                }
            })
        }};
    }
    match a.data_type() {
        DataType::Int8 => prim!(Int8Type),
        DataType::Int16 => prim!(Int16Type),
        DataType::Int32 => prim!(Int32Type),
        DataType::Int64 => prim!(Int64Type),
        DataType::UInt8 => prim!(UInt8Type),
        DataType::UInt16 => prim!(UInt16Type),
        DataType::UInt32 => prim!(UInt32Type),
        DataType::UInt64 => prim!(UInt64Type),
        DataType::Float32 => prim!(Float32Type),
        DataType::Float64 => prim!(Float64Type),
        DataType::Boolean => {
            let arr = a.as_boolean();
            Box::new(move |i| {
                if arr.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(arr.value(i))
                }
            })
        }
        _ => match ArrayFormatter::try_new(a, opts) {
            Ok(f) => Box::new(move |i| {
                if a.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(f.value(i).to_string())
                }
            }),
            Err(_) => Box::new(|_| Value::from(())),
        },
    }
}
