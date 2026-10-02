//! F26 — the `qualifications` row: a fresh verdict inserts, a repeat of the
//! same `(operating point, args digest, capability)` replaces it.

use rusqlite::Transaction;

use governor_core::config::Qualification;
use governor_core::identity::Timestamp;

use crate::store::error::ApplyError;
use crate::store::rows::qualification::QualificationRow;
use crate::store::transitions::insert;

const UPSERT: &str = "ON CONFLICT(operating_point_id, args_digest, capability) DO UPDATE SET \
     passed = excluded.passed, evidence_json = excluded.evidence_json, \
     qualified_at = excluded.qualified_at";

pub(super) fn apply(
    tx: &Transaction<'_>,
    qualification: &Qualification,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let row = QualificationRow::from_core(qualification, now)?;
    insert(tx, "INSERT", "qualifications", &row.params(), UPSERT)?;
    Ok(())
}
