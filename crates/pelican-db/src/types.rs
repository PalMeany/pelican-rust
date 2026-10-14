//! Column types that tolerate the schema drift between MySQL/MariaDB, PostgreSQL
//! and SQLite in databases created by the original (Laravel) panel.
//!
//! The same logical column has different physical types per backend, e.g.
//! `nodes.public` is `smallint unsigned` (MySQL), `smallint` (PostgreSQL) and
//! `integer` (SQLite), while `nodes.behind_proxy` is `tinyint(1)` / `boolean` /
//! `tinyint(1)`. sqlx decodes strictly (an `i64` cannot be read from a Postgres
//! `int4`, an unsigned MySQL column cannot be read as signed), so every numeric
//! and boolean column goes through the wrappers below, which try all compatible
//! physical representations when decoding and pick a bind type that the
//! backend accepts in assignment context when encoding. See ADR-0004.

use std::fmt;
use std::ops::Deref;

use sea_orm::sea_query::{ArrayType, ColumnType, Nullable, Value, ValueType, ValueTypeErr};
use sea_orm::{ColIdx, DbErr, QueryResult, TryFromU64, TryGetError, TryGetable};
use serde::{Deserialize, Serialize};

/// Try a list of sqlx-decodable representations until one succeeds.
/// Returns `Ok(None)` for SQL NULL.
fn decode_first<T, I: ColIdx>(
    res: &QueryResult,
    idx: I,
    attempts: &[fn(&QueryResult, I) -> Result<Option<T>, TryGetError>],
) -> Result<Option<T>, TryGetError> {
    let mut last_err = None;
    for attempt in attempts {
        match attempt(res, idx) {
            Ok(v) => return Ok(v),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| TryGetError::DbErr(DbErr::Type("no decoder".into()))))
}

fn null_err<I: ColIdx>(idx: I) -> TryGetError {
    TryGetError::Null(format!("{idx:?}"))
}

macro_rules! via {
    ($t:ty, $map:expr) => {
        |res: &QueryResult, idx| <Option<$t> as TryGetable>::try_get_by(res, idx).map(|o| o.map($map))
    };
}

/// Decode any SQL integer (signed/unsigned, any width) or boolean as `i64`.
fn decode_i64<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Option<i64>, TryGetError> {
    decode_first(
        res,
        idx,
        &[
            via!(i64, |v| v),
            via!(i32, i64::from),
            via!(i16, i64::from),
            via!(i8, i64::from),
            via!(u32, i64::from),
            via!(u16, i64::from),
            via!(u8, i64::from),
            via!(u64, |v| v as i64),
            via!(bool, i64::from),
        ],
    )
}

macro_rules! impl_value_type_common {
    ($name:ident, $inner:ty, $array:expr, $col:expr) => {
        impl_value_type_common!($name, $inner, $array, $col, |v| { <$inner as ValueType>::try_from(v) });
    };
    ($name:ident, $inner:ty, $array:expr, $col:expr, $try_from:expr) => {
        impl ValueType for $name {
            fn try_from(v: Value) -> Result<Self, ValueTypeErr> {
                let f: fn(Value) -> Result<$inner, ValueTypeErr> = $try_from;
                f(v).map($name)
            }
            fn type_name() -> String {
                stringify!($name).to_owned()
            }
            fn array_type() -> ArrayType {
                $array
            }
            fn column_type() -> ColumnType {
                $col
            }
        }

        impl Deref for $name {
            type Target = $inner;
            fn deref(&self) -> &$inner {
                &self.0
            }
        }

        impl From<$inner> for $name {
            fn from(v: $inner) -> Self {
                $name(v)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Int
// ---------------------------------------------------------------------------

/// Any integer column. Always bound as BIGINT: PostgreSQL applies an assignment
/// cast to `int4`/`int2` columns and has cross-type comparison operators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Int(pub i64);

impl TryGetable for Int {
    fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
        decode_i64(res, idx)?.map(Int).ok_or_else(|| null_err(idx))
    }
}

impl From<Int> for Value {
    fn from(v: Int) -> Value {
        Value::BigInt(Some(v.0))
    }
}

impl Nullable for Int {
    fn null() -> Value {
        Value::BigInt(None)
    }
}

impl TryFromU64 for Int {
    fn try_from_u64(n: u64) -> Result<Self, DbErr> {
        <i64 as std::convert::TryFrom<u64>>::try_from(n).map(Int).map_err(|e| DbErr::Type(e.to_string()))
    }
}

impl_value_type_common!(Int, i64, ArrayType::BigInt, ColumnType::BigInteger);

impl PartialEq<i64> for Int {
    fn eq(&self, other: &i64) -> bool {
        self.0 == *other
    }
}

// ---------------------------------------------------------------------------
// Bool / SmallBool
// ---------------------------------------------------------------------------

/// Boolean stored as `boolean` on PostgreSQL (tinyint elsewhere): bound as BOOL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bool(pub bool);

/// Boolean-cast column stored as an integer type on PostgreSQL (`smallint`):
/// bound as BIGINT 0/1, because PostgreSQL has no assignment cast bool -> int.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SmallBool(pub bool);

macro_rules! impl_bool {
    ($name:ident, $encode:expr, $null:expr) => {
        impl TryGetable for $name {
            fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
                let v = decode_first(
                    res,
                    idx,
                    &[via!(bool, |v| v), |res, idx| decode_i64(res, idx).map(|o| o.map(|v| v != 0))],
                )?;
                v.map($name).ok_or_else(|| null_err(idx))
            }
        }

        impl From<$name> for Value {
            fn from(v: $name) -> Value {
                ($encode)(v.0)
            }
        }

        impl Nullable for $name {
            fn null() -> Value {
                $null
            }
        }

        impl_value_type_common!($name, bool, ArrayType::Bool, ColumnType::Boolean, |v| match v {
            Value::Bool(Some(b)) => Ok(b),
            Value::BigInt(Some(i)) => Ok(i != 0),
            _ => Err(ValueTypeErr),
        });
    };
}

impl_bool!(Bool, |b: bool| Value::Bool(Some(b)), Value::Bool(None));
impl_bool!(SmallBool, |b: bool| Value::BigInt(Some(i64::from(b))), Value::BigInt(None));

// ---------------------------------------------------------------------------
// Json
// ---------------------------------------------------------------------------

/// A JSON document stored in a `json` column (PostgreSQL), `longtext`+CHECK
/// (MariaDB), `json` (MySQL) or `text` (SQLite, and some PostgreSQL columns that
/// Laravel casts to array). Bound as JSON: PostgreSQL converts json -> text in
/// assignment context, the other backends receive a string.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Json(pub serde_json::Value);

impl TryGetable for Json {
    fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
        let v = decode_first(
            res,
            idx,
            &[
                |res, idx| match <Option<String> as TryGetable>::try_get_by(res, idx)? {
                    None => Ok(None),
                    Some(s) => {
                        serde_json::from_str(&s).map(Some).map_err(|e| TryGetError::DbErr(DbErr::Json(e.to_string())))
                    }
                },
                via!(serde_json::Value, |v| v),
            ],
        )?;
        v.map(Json).ok_or_else(|| null_err(idx))
    }
}

impl From<Json> for Value {
    fn from(v: Json) -> Value {
        Value::Json(Some(Box::new(v.0)))
    }
}

impl Nullable for Json {
    fn null() -> Value {
        Value::Json(None)
    }
}

impl_value_type_common!(Json, serde_json::Value, ArrayType::Json, ColumnType::Json);

// ---------------------------------------------------------------------------
// Timestamp
// ---------------------------------------------------------------------------

/// A Laravel timestamp column: wall-clock time in the app timezone with second
/// precision (`Y-m-d H:i:s`). MySQL `TIMESTAMP` is decoded by sqlx as
/// `DateTime<Utc>` — its digits are the session-time-zone wall clock, exactly
/// what Laravel reads, so the offset is dropped rather than converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub chrono::NaiveDateTime);

impl Timestamp {
    /// Truncate sub-second precision, as Laravel's `Y-m-d H:i:s` storage does.
    pub fn new(dt: chrono::NaiveDateTime) -> Self {
        use chrono::Timelike;
        Timestamp(dt.with_nanosecond(0).unwrap_or(dt))
    }
}

impl TryGetable for Timestamp {
    fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
        let v = decode_first(
            res,
            idx,
            &[
                via!(chrono::NaiveDateTime, |v| v),
                via!(chrono::DateTime<chrono::Utc>, |v| v.naive_utc()),
                |res, idx| match <Option<String> as TryGetable>::try_get_by(res, idx)? {
                    None => Ok(None),
                    Some(s) => parse_laravel_datetime(&s)
                        .map(Some)
                        .ok_or_else(|| TryGetError::DbErr(DbErr::Type(format!("bad datetime `{s}`")))),
                },
            ],
        )?;
        v.map(Timestamp).ok_or_else(|| null_err(idx))
    }
}

/// Parse the textual forms found in SQLite databases written by Laravel.
pub fn parse_laravel_datetime(s: &str) -> Option<chrono::NaiveDateTime> {
    const FORMATS: &[&str] =
        &["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    let s = s.trim();
    for f in FORMATS {
        if let Ok(v) = chrono::NaiveDateTime::parse_from_str(s, f) {
            return Some(v);
        }
    }
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.naive_local())
}

impl From<Timestamp> for Value {
    fn from(v: Timestamp) -> Value {
        Value::ChronoDateTime(Some(v.0))
    }
}

impl Nullable for Timestamp {
    fn null() -> Value {
        Value::ChronoDateTime(None)
    }
}

impl_value_type_common!(Timestamp, chrono::NaiveDateTime, ArrayType::ChronoDateTime, ColumnType::DateTime);
