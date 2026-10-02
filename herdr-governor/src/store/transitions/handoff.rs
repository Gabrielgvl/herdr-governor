//! F24 — the frozen `handoffs` row, `INSERT OR IGNORE` (primary-key dedup:
//! freezing the same digest twice is idempotent). The paired Run-row bump
//! is the transition's own `UpdateRun`.

use rusqlite::Transaction;

use governor_core::acceptance::FrozenHandoff;

use crate::store::error::ApplyError;
use crate::store::rows::handoff::HandoffRow;
use crate::store::transitions::insert;

pub(super) fn apply(tx: &Transaction<'_>, frozen: &FrozenHandoff) -> Result<(), ApplyError> {
    let row = HandoffRow::from_core(frozen)?;
    insert(tx, "INSERT OR IGNORE", "handoffs", &row.params(), "")?;
    Ok(())
}
