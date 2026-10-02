//! F17/F20 — the `outbox` writes. `WriteFollowUp` is one conditional write
//! per [`FollowUpWrite`] edge: `Enqueue` is the insert-once whole row
//! (`UNIQUE (run_id, message_key)` / primary-key collision is a
//! [`ConflictKind::FollowUp`] conflict — the dedup decision is the
//! caller's read-first job); `Dispatch` and `Resolve` are forward-only
//! compare-and-swap `UPDATE`s whose `WHERE` names exactly the state the
//! edge leaves (F17 `queued` → `dispatching` → `submitted` | `unconfirmed`;
//! F9 `unconfirmed` → `submitted`), matching no row being the same
//! conflict. `ExpireFollowUps` expires the Run's still-`queued` rows only
//! (dispatched ones keep their last state).
//!
//! `outbox.effect_id` is an immediate FOREIGN KEY: a `Dispatch` naming an
//! effect that is not journaled is a [`ApplyError::Constraint`], so the
//! prompt effect is planned in an earlier transaction and the dispatch
//! write rides that effect's own dispatch commit.

use rusqlite::{Transaction, params};

use governor_core::delivery::{ExpiryReason, FollowUpWrite, OutboxMessage, OutboxState};
use governor_core::identity::{EffectId, RunId, Timestamp};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::outbox::OutboxRow;
use crate::store::rows::u64_to_col;
use crate::store::transitions::{execute, insert_unique, resolve_caller, stamp};

const DISPATCH: &str = "UPDATE outbox SET state = ?1, effect_id = ?2 \
     WHERE run_id = ?3 AND seq = ?4 AND state = ?5";

const RESOLVE: &str = "UPDATE outbox SET state = ?1, finished_at = ?2 \
     WHERE run_id = ?3 AND seq = ?4 AND state IN (?5, ?6)";

const EXPIRE: &str = "UPDATE outbox SET state = ?1, expiry_reason = ?2, finished_at = ?3 \
     WHERE run_id = ?4 AND state = ?5";

pub(super) fn apply(
    tx: &Transaction<'_>,
    write: &FollowUpWrite,
    now: Timestamp,
) -> Result<(), ApplyError> {
    match write {
        FollowUpWrite::Enqueue(message) => enqueue(tx, message, now),
        FollowUpWrite::Dispatch { run, seq, effect } => dispatch(tx, run, *seq, effect),
        FollowUpWrite::Resolve { run, seq, state } => resolve(tx, run, *seq, *state, now),
    }
}

/// The entry's spelling in a conflict: the key of the prompt effect that
/// dispatches it (Appendix B `effect_key`).
pub(super) fn entry_key(run: &RunId, seq: u64) -> String {
    format!("run:{}:outbox:{seq}", run.0)
}

fn enqueue(
    tx: &Transaction<'_>,
    message: &OutboxMessage,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let sender = resolve_caller(tx, &message.sender)?;
    // `Queued` only (guarded by `check_well_formed`): no `finished_at`.
    let row = OutboxRow::from_core(message, sender, now, None)?;
    insert_unique(
        tx,
        "outbox",
        &row.params(),
        ConflictKind::FollowUp,
        &message.message_key.0,
    )
}

fn dispatch(
    tx: &Transaction<'_>,
    run: &RunId,
    seq: u64,
    effect: &EffectId,
) -> Result<(), ApplyError> {
    let changed = execute(
        tx,
        DISPATCH,
        params![
            OutboxState::Dispatching.as_str(),
            effect.0,
            run.0,
            u64_to_col(seq, "outbox", "seq")?,
            OutboxState::Queued.as_str()
        ],
    )?;
    conflict_unless(changed, run, seq)
}

fn resolve(
    tx: &Transaction<'_>,
    run: &RunId,
    seq: u64,
    state: OutboxState,
    now: Timestamp,
) -> Result<(), ApplyError> {
    // `submitted` follows `dispatching` (the acknowledgement) or
    // `unconfirmed` (transcript evidence, F9); `unconfirmed` only ever
    // follows `dispatching`. Anything else was refused at `apply` entry.
    let from = if state == OutboxState::Submitted {
        OutboxState::Unconfirmed
    } else {
        OutboxState::Dispatching
    };
    let changed = execute(
        tx,
        RESOLVE,
        params![
            state.as_str(),
            stamp(now, "outbox", "finished_at")?,
            run.0,
            u64_to_col(seq, "outbox", "seq")?,
            OutboxState::Dispatching.as_str(),
            from.as_str()
        ],
    )?;
    conflict_unless(changed, run, seq)
}

fn conflict_unless(changed: usize, run: &RunId, seq: u64) -> Result<(), ApplyError> {
    if changed == 0 {
        return Err(ApplyError::Conflict {
            kind: ConflictKind::FollowUp,
            key: entry_key(run, seq),
        });
    }
    Ok(())
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
