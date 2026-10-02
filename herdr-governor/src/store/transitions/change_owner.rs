//! F4/Appendix B — `handover`/`adopt`: the owner write is conditional on
//! `expected_owner` still owning the Run and bumps `owner_generation` in
//! place. An expected owner that is not even a bound caller cannot own
//! anything — the same [`ConflictKind::Owner`] conflict; the successor
//! must be bound (FOREIGN KEY).

use rusqlite::{Transaction, params};

use governor_core::identity::Timestamp;
use governor_core::lifecycle::OwnerChange;

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::caller::caller_id;
use crate::store::transitions::{execute, resolve_caller, stamp};

const SQL: &str = "UPDATE runs SET owner_caller_id = ?1, \
     owner_generation = owner_generation + 1, updated_at = ?2 \
     WHERE run_id = ?3 AND owner_caller_id = ?4";

pub(super) fn apply(
    tx: &Transaction<'_>,
    change: &OwnerChange,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let lost = || ApplyError::Conflict {
        kind: ConflictKind::Owner,
        key: change.run.0.clone(),
    };
    let Some(expected) = caller_id(tx, &change.expected_owner)? else {
        return Err(lost());
    };
    let owner = resolve_caller(tx, &change.owner)?;
    let updated_at = stamp(now, "runs", "updated_at")?;
    let changed = execute(tx, SQL, params![owner, updated_at, change.run.0, expected])?;
    if changed == 0 {
        return Err(lost());
    }
    Ok(())
}
