//! Appendix B — the conditional `runs` row write behind "Effect result",
//! "Settle" and every other Run write: the full record `WHERE run_id = ?
//! AND version = expected`, plus `AND settlement IS NULL` when the record
//! settles (F20 first-commit-wins). The stored `version` is the record's
//! own — the core already bumped it. `created_at` never moves. No match
//! is a [`ConflictKind::RunVersion`] conflict.

use rusqlite::Transaction;
use rusqlite::types::Value as SqlValue;

use governor_core::identity::Timestamp;
use governor_core::lifecycle::RunUpdate;

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::run::RunRow;
use crate::store::rows::u64_to_col;
use crate::store::transitions::{resolve_caller, update, without};

pub(super) fn apply(
    tx: &Transaction<'_>,
    change: &RunUpdate,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let record = &change.record;
    let owner = resolve_caller(tx, &record.owner)?;
    let set = without(
        RunRow::from_core(record, owner, now)?.params(),
        &["run_id", "created_at"],
    );
    let expected = u64_to_col(change.expected_version, "runs", "version")?;
    let guard = if record.settlement.is_some() {
        " AND settlement IS NULL"
    } else {
        ""
    };
    let changed = update(
        tx,
        "runs",
        &set,
        &format!("run_id = ?1 AND version = ?2{guard}"),
        &[
            SqlValue::from(record.id.0.clone()),
            SqlValue::from(expected),
        ],
    )?;
    if changed == 0 {
        return Err(ApplyError::Conflict {
            kind: ConflictKind::RunVersion,
            key: record.id.0.clone(),
        });
    }
    Ok(())
}
