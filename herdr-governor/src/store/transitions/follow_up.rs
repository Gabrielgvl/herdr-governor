//! F17/F20 — the `outbox` writes: `RecordFollowUp` is the whole-row
//! `INSERT` (`UNIQUE (run_id, message_key)` collision is a
//! [`ConflictKind::FollowUp`] conflict — the dedup decision is the
//! caller's read-first job); `ExpireFollowUps` expires the Run's
//! still-`queued` rows only (dispatched ones keep their last state).

use rusqlite::{Transaction, params};

use governor_core::delivery::{ExpiryReason, OutboxMessage, OutboxState};
use governor_core::identity::{RunId, Timestamp};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::outbox::OutboxRow;
use crate::store::transitions::{execute, insert_unique, resolve_caller, stamp};

const EXPIRE: &str = "UPDATE outbox SET state = ?1, expiry_reason = ?2, finished_at = ?3 \
     WHERE run_id = ?4 AND state = ?5";

pub(super) fn record(
    tx: &Transaction<'_>,
    message: &OutboxMessage,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let sender = resolve_caller(tx, &message.sender)?;
    let finished_at = matches!(
        message.state,
        OutboxState::Submitted | OutboxState::Unconfirmed | OutboxState::Expired
    )
    .then_some(now);
    let row = OutboxRow::from_core(message, sender, now, finished_at)?;
    insert_unique(
        tx,
        "outbox",
        &row.params(),
        ConflictKind::FollowUp,
        &message.message_key.0,
    )
}

pub(super) fn expire(
    tx: &Transaction<'_>,
    run: &RunId,
    reason: ExpiryReason,
    now: Timestamp,
) -> Result<(), ApplyError> {
    execute(
        tx,
        EXPIRE,
        params![
            OutboxState::Expired.as_str(),
            reason.as_str(),
            stamp(now, "outbox", "finished_at")?,
            run.0,
            OutboxState::Queued.as_str()
        ],
    )?;
    Ok(())
}
