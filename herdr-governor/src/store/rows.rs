//! `rows` — row ↔ core-type codecs, one child module per table group
//! (P4.S2): hand-mapped JSON columns (OQ-5), `as_str()` enums, and
//! RFC3339-millis time columns under the `outcomes` view's `julianday`
//! contract.
//!
//! Each child exposes a `*Row` struct whose fields are the table's columns
//! as plain Rust values (`String`/`i64`/`f64`/`Option`), plus:
//!
//! - `from_core(..)` — encode a core value into the row shape (pure);
//! - `to_core(..)` — the checked decode back to the core type (pure);
//! - `params()` — the row as `(column, value)` pairs for a writer to bind;
//! - `read(&Row)` — pull named columns out of a `rusqlite::Row`.
//!
//! Encode and decode are each other's inverse on the core-type domain —
//! `decode(encode(v)) == v`. Columns that carry no core field (store-stamped
//! `created_at`/`updated_at`, …) are documented at the row struct. Nothing
//! here writes: binding the params is the writer's job (I10).
//!
//! Persisted spellings (v1):
//!
//! - `TEXT` timestamps are RFC3339 UTC with millisecond precision
//!   (`YYYY-MM-DDTHH:MM:SS.mmmZ`) — the shape `julianday` accepts;
//! - `Digest` columns are 64-char lowercase hex;
//! - enum columns use the core `as_str()` spellings; unknown text decodes to
//!   [`StoreError::CorruptRow`], never a panic;
//! - JSON columns are hand-mapped `serde_json::Value` objects with
//!   snake_case member names; `decision_json` must carry the assigned flag
//!   at `$.exploration.assigned` and the executed flag at
//!   `$.exploration.executed` for the `outcomes` view.

pub(in crate::store) mod caller;
pub(in crate::store) mod cooldown;
pub(in crate::store) mod effect;
pub(in crate::store) mod handoff;
pub(in crate::store) mod judgment;
pub(in crate::store) mod launch;
pub(in crate::store) mod mailbox;
pub(in crate::store) mod outbox;
pub(in crate::store) mod qualification;
pub(in crate::store) mod recovery;
pub(in crate::store) mod run;
#[cfg(test)]
mod tests;

use rusqlite::Row;
use rusqlite::types::{FromSql, Value as SqlValue};
use serde_json::Value;

use governor_core::identity::{Digest, Timestamp};

use crate::store::error::StoreError;

/// A row as `(column, value)` pairs — what a writer binds. Column names are
/// bare; the writer spells the `:name` placeholders.
pub(in crate::store) type Params = Vec<(&'static str, SqlValue)>;

/// Build a [`StoreError::CorruptRow`].
fn corrupt(table: &'static str, column: &'static str, reason: impl Into<String>) -> StoreError {
    StoreError::CorruptRow {
        table,
        column,
        reason: reason.into(),
    }
}

/// Read column `column` from `row`. A type mismatch is corrupt data; any
/// other failure is an engine/query error.
fn read_col<T: FromSql>(
    row: &Row<'_>,
    table: &'static str,
    column: &'static str,
) -> Result<T, StoreError> {
    row.get::<_, T>(column).map_err(|err| {
        if matches!(err, rusqlite::Error::InvalidColumnType(..)) {
            corrupt(table, column, err.to_string())
        } else {
            StoreError::Sqlite(err)
        }
    })
}

/// `u64` core field → `INTEGER` column; values above `i64::MAX` have no
/// persisted representation.
pub(in crate::store) fn u64_to_col(
    value: u64,
    table: &'static str,
    column: &'static str,
) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_overflow| corrupt(table, column, format!("{value} does not fit INTEGER")))
}

/// `INTEGER` column → `u64` core field; negative values are corrupt.
fn i64_to_u64(value: i64, table: &'static str, column: &'static str) -> Result<u64, StoreError> {
    u64::try_from(value)
        .map_err(|_negative| corrupt(table, column, format!("negative value {value}")))
}

/// Checked enum decode: the variant whose `as_str()` equals the persisted
/// text, or [`StoreError::CorruptRow`]. The spelling table stays
/// single-sourced on the core type.
fn enum_decode<T: Copy>(
    text: &str,
    table: &'static str,
    column: &'static str,
    variants: &[T],
    as_str: fn(&T) -> &'static str,
) -> Result<T, StoreError> {
    variants
        .iter()
        .copied()
        .find(|variant| as_str(variant) == text)
        .ok_or_else(|| corrupt(table, column, format!("unknown value {text:?}")))
}

/// Optional enum column → `Option<T>` through [`enum_decode`].
fn enum_opt_decode<T: Copy>(
    text: Option<&str>,
    table: &'static str,
    column: &'static str,
    variants: &[T],
    as_str: fn(&T) -> &'static str,
) -> Result<Option<T>, StoreError> {
    text.map(|value| enum_decode(value, table, column, variants, as_str))
        .transpose()
}

/// Serialize `value` for a `TEXT` JSON column.
fn json_write(
    value: &Value,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|err| corrupt(table, column, err.to_string()))
}

/// Parse a `TEXT` JSON column into a [`Value`].
fn json_parse(text: &str, table: &'static str, column: &'static str) -> Result<Value, StoreError> {
    serde_json::from_str(text).map_err(|err| corrupt(table, column, err.to_string()))
}

/// Required member `key` of a JSON object.
fn member<'v>(
    value: &'v Value,
    key: &'static str,
    table: &'static str,
    column: &'static str,
) -> Result<&'v Value, StoreError> {
    value
        .get(key)
        .ok_or_else(|| corrupt(table, column, format!("missing member {key:?}")))
}

/// Required string member `key` of a JSON object.
fn member_str(
    value: &Value,
    key: &'static str,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    json_str(member(value, key, table, column)?, table, column)
}

/// Optional-or-null string member `key` of a JSON object.
fn member_opt_str(
    value: &Value,
    key: &'static str,
    table: &'static str,
    column: &'static str,
) -> Result<Option<String>, StoreError> {
    json_opt_str(member(value, key, table, column)?, table, column)
}

/// Required bool member `key` of a JSON object.
fn member_bool(
    value: &Value,
    key: &'static str,
    table: &'static str,
    column: &'static str,
) -> Result<bool, StoreError> {
    member(value, key, table, column)?
        .as_bool()
        .ok_or_else(|| corrupt(table, column, format!("member {key:?} is not a bool")))
}

/// Required array member `key` of a JSON object.
fn member_arr<'v>(
    value: &'v Value,
    key: &'static str,
    table: &'static str,
    column: &'static str,
) -> Result<&'v Vec<Value>, StoreError> {
    member(value, key, table, column)?
        .as_array()
        .ok_or_else(|| corrupt(table, column, format!("member {key:?} is not an array")))
}

/// `value` as a string.
fn json_str(
    value: &Value,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| corrupt(table, column, "expected a JSON string"))
}

/// `value` as `Some(String)`, or `None` when it is JSON `null`.
fn json_opt_str(
    value: &Value,
    table: &'static str,
    column: &'static str,
) -> Result<Option<String>, StoreError> {
    if value.is_null() {
        return Ok(None);
    }
    json_str(value, table, column).map(Some)
}

/// `value` as an array of strings.
fn json_str_arr(
    value: &Value,
    table: &'static str,
    column: &'static str,
) -> Result<Vec<String>, StoreError> {
    value
        .as_array()
        .ok_or_else(|| corrupt(table, column, "expected a JSON array"))?
        .iter()
        .map(|item| json_str(item, table, column))
        .collect()
}

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
const MINUTE_MS: i64 = 60_000;
const SECOND_MS: i64 = 1_000;

/// Civil date `(year, month, day)` for `days` days after 1970-01-01 —
/// Hinnant's algorithm. `days` is bounded by `i64` milliseconds, so the
/// saturating arithmetic can never saturate.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe.div_euclid(1_460))
        .saturating_add(doe.div_euclid(36_524))
        .saturating_sub(doe.div_euclid(146_096))
        .div_euclid(365);
    let year_base = era.saturating_mul(400).saturating_add(yoe);
    let doy = doe
        .saturating_sub(yoe.saturating_mul(365))
        .saturating_sub(yoe.div_euclid(4))
        .saturating_add(yoe.div_euclid(100));
    let mp = doy.saturating_mul(5).saturating_add(2).div_euclid(153);
    let day = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2).div_euclid(5))
        .saturating_add(1);
    let month = mp.saturating_add(if mp < 10 { 3 } else { -9 });
    let year = year_base.saturating_add(i64::from(month <= 2));
    (year, month, day)
}

/// Days after 1970-01-01 for the civil date — Hinnant's inverse.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let shifted = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = shifted.div_euclid(400);
    let yoe = shifted.rem_euclid(400);
    let mp = month.saturating_add(9).rem_euclid(12);
    let doy = mp
        .saturating_mul(153)
        .saturating_add(2)
        .div_euclid(5)
        .saturating_add(day)
        .saturating_sub(1);
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe.div_euclid(4))
        .saturating_sub(yoe.div_euclid(100))
        .saturating_add(doy);
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

fn leap_year(year: i64) -> bool {
    (year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0) || year.rem_euclid(400) == 0
}

fn month_days(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => i64::from(leap_year(year)).saturating_add(28),
    }
}

/// Encode a [`Timestamp`] (epoch milliseconds) as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
/// Years outside `0000..=9999` have no RFC3339 spelling (and `julianday`
/// would not parse one), so they are refused as [`StoreError::CorruptRow`]
/// rather than written in a shape the view cannot read.
pub(in crate::store) fn ts_encode(
    value: Timestamp,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    let days = value.0.div_euclid(DAY_MS);
    let rem = value.0.rem_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9_999).contains(&year) {
        return Err(corrupt(table, column, "year outside RFC3339 range"));
    }
    let hour = rem.div_euclid(HOUR_MS);
    let minute = rem.rem_euclid(HOUR_MS).div_euclid(MINUTE_MS);
    let second = rem.rem_euclid(MINUTE_MS).div_euclid(SECOND_MS);
    let millis = rem.rem_euclid(SECOND_MS);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

/// Nullable timestamp → `Option<String>` column.
fn ts_opt_encode(
    value: Option<Timestamp>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<String>, StoreError> {
    value.map(|ts| ts_encode(ts, table, column)).transpose()
}

/// A fixed-width decimal field of exactly `width` digits.
fn parse_field(
    text: &str,
    width: usize,
    table: &'static str,
    column: &'static str,
) -> Result<i64, StoreError> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(corrupt(
            table,
            column,
            format!("{text:?} is not {width} digits"),
        ));
    }
    text.parse::<i64>()
        .map_err(|_overflow| corrupt(table, column, format!("{text:?} out of range")))
}

/// Decode the exact `YYYY-MM-DDTHH:MM:SS.mmmZ` spelling [`ts_encode`]
/// produces. Anything else is a typed error — persisted data is never
/// trusted.
pub(in crate::store) fn ts_decode(
    text: &str,
    table: &'static str,
    column: &'static str,
) -> Result<Timestamp, StoreError> {
    // Fixed layout: 0123456789012345678901234
    //               YYYY-MM-DDTHH:MM:SS.mmmZ
    let bytes = text.as_bytes();
    let shape_ok = bytes.len() == 24
        && [4, 7].iter().all(|&i| bytes.get(i) == Some(&b'-'))
        && bytes.get(10) == Some(&b'T')
        && [13, 16].iter().all(|&i| bytes.get(i) == Some(&b':'))
        && bytes.get(19) == Some(&b'.')
        && bytes.get(23) == Some(&b'Z');
    if !shape_ok {
        return Err(corrupt(
            table,
            column,
            format!("{text:?} is not RFC3339 UTC millis"),
        ));
    }
    let field = |range: core::ops::Range<usize>, width: usize| {
        let slice = text
            .get(range)
            .ok_or_else(|| corrupt(table, column, "truncated timestamp"))?;
        parse_field(slice, width, table, column)
    };
    let year = field(0..4, 4)?;
    let month = field(5..7, 2)?;
    let day = field(8..10, 2)?;
    let hour = field(11..13, 2)?;
    let minute = field(14..16, 2)?;
    let second = field(17..19, 2)?;
    let millis = field(20..23, 3)?;
    if !(1..=12).contains(&month) {
        return Err(corrupt(
            table,
            column,
            format!("month {month} out of range"),
        ));
    }
    if !(1..=month_days(year, month)).contains(&day) {
        return Err(corrupt(table, column, format!("day {day} out of range")));
    }
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err(corrupt(table, column, "time field out of range"));
    }
    let days = days_from_civil(year, month, day);
    Ok(Timestamp(
        days.saturating_mul(DAY_MS)
            .saturating_add(hour.saturating_mul(HOUR_MS))
            .saturating_add(minute.saturating_mul(MINUTE_MS))
            .saturating_add(second.saturating_mul(SECOND_MS))
            .saturating_add(millis),
    ))
}

/// Nullable timestamp column → `Option<Timestamp>`.
fn ts_opt_decode(
    text: Option<&str>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<Timestamp>, StoreError> {
    text.map(|value| ts_decode(value, table, column))
        .transpose()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        _ => None,
    }
}

/// Encode a [`Digest`] as 64 lowercase hex chars.
pub(in crate::store) fn hex_encode(digest: Digest) -> String {
    use std::fmt::Write as _;
    digest
        .0
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            // Writing into a `String` cannot fail; nothing to propagate.
            write!(out, "{byte:02x}").ok();
            out
        })
}

/// Decode a 64-char lowercase hex column into a [`Digest`].
fn hex_decode(text: &str, table: &'static str, column: &'static str) -> Result<Digest, StoreError> {
    let mut out = [0_u8; 32];
    if text.len() != 64 {
        return Err(corrupt(table, column, "digest is not 64 hex chars"));
    }
    let (pairs, _rest) = text.as_bytes().as_chunks::<2>();
    for (slot, [high, low]) in out.iter_mut().zip(pairs) {
        match (hex_value(*high), hex_value(*low)) {
            (Some(h), Some(l)) => *slot = h.saturating_mul(16).saturating_add(l),
            _ => return Err(corrupt(table, column, "digest is not lowercase hex")),
        }
    }
    Ok(Digest(out))
}

/// Nullable hex column → `Option<Digest>`.
fn hex_opt_decode(
    text: Option<&str>,
    table: &'static str,
    column: &'static str,
) -> Result<Option<Digest>, StoreError> {
    text.map(|value| hex_decode(value, table, column))
        .transpose()
}
