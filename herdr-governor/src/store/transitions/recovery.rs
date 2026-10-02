//! F21 — `recoveries` and `cooldowns`. An obligation upserts by
//! `predecessor_run_id` and only a `pending` row may move (`pending` →
//! `dispatched` | `blocked` | `failed`); a write against a row past
//! `pending` is a [`ConflictKind::Recovery`] conflict. A cooldown upserts
//! by `provider` and keeps `max(until, excluded.until)` — never shortened
//! (RFC3339-millis text compares chronologically).

use rusqlite::Transaction;

use governor_core::identity::Timestamp;
use governor_core::recovery::{Cooldown, RecoveryObligation};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::cooldown::CooldownRow;
use crate::store::rows::recovery::RecoveryRow;
use crate::store::transitions::insert;

const RECOVERY_UPSERT: &str = "ON CONFLICT(predecessor_run_id) DO UPDATE SET \
     state = excluded.state, reason = excluded.reason, \
     successor_launch_id = excluded.successor_launch_id, \
     expires_at = excluded.expires_at, updated_at = excluded.updated_at \
     WHERE recoveries.state = 'pending'";

const COOLDOWN_UPSERT: &str = "ON CONFLICT(provider) DO UPDATE SET \
     until = max(until, excluded.until), reason = excluded.reason, \
     source_run_id = excluded.source_run_id, updated_at = excluded.updated_at";

pub(super) fn record(
    tx: &Transaction<'_>,
    obligation: &RecoveryObligation,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let row = RecoveryRow::from_core(obligation, now)?;
    let changed = insert(tx, "INSERT", "recoveries", &row.params(), RECOVERY_UPSERT)?;
    if changed == 0 {
        return Err(ApplyError::Conflict {
            kind: ConflictKind::Recovery,
            key: obligation.predecessor.0.clone(),
        });
    }
    Ok(())
}

pub(super) fn set_cooldown(
    tx: &Transaction<'_>,
    cooldown: &Cooldown,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let row = CooldownRow::from_core(cooldown, now)?;
    insert(tx, "INSERT", "cooldowns", &row.params(), COOLDOWN_UPSERT)?;
    Ok(())
}
