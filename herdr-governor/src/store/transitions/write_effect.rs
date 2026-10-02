//! F8/Appendix B — the journal-row writes, one arm per requested state,
//! every WHERE naming exactly the states the arm transitions from:
//!
//! - `dispatching` ← `planned` (the dispatch commit; `dispatched_at`
//!   stamped, `payload_digest` untouched — fixed at plan time);
//! - `unconfirmed` ← `dispatching` (the F28 restart marking: no receipt,
//!   `certainty` untouched);
//! - `failed` ← `planned` | `unconfirmed` | `dispatching` — the OQ-13
//!   terminal write and the result commit share one statement because
//!   they write the same columns: `certainty` verbatim (always `Some`,
//!   guarded at `apply` entry), `result_json` from the receipt;
//! - `acknowledged` ← `dispatching` (the result commit).
//!
//! `unconfirmed` has no exit but `failed` — never a re-dispatch. A
//! `Judgments` receipt also writes `judgment_sets`/`judgments`
//! (`INSERT OR IGNORE` — idempotent redelivery), `requested_at` being the
//! row's `dispatched_at`. No match is a [`ConflictKind::Effect`] conflict.

use rusqlite::{OptionalExtension as _, Transaction, params};

use governor_core::identity::Timestamp;
use governor_core::lifecycle::{EffectReceipt, EffectState, EffectWrite};
use governor_core::routing::{JudgmentOutcome, JudgmentRecord};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::effect::result_json;
use crate::store::rows::judgment::{JudgmentRow, JudgmentSetRow};
use crate::store::rows::ts_decode;
use crate::store::transitions::{crash_checkpoint, execute, insert, stamp};

const DISPATCH: &str = "UPDATE effects SET state = ?1, dispatched_at = ?2 \
     WHERE effect_key = ?3 AND state = ?4";

const RESTART: &str = "UPDATE effects SET state = ?1 WHERE effect_key = ?2 AND state = ?3";

const RESULT: &str = "UPDATE effects SET state = ?1, certainty = ?2, result_json = ?3, \
     completed_at = ?4 WHERE effect_key = ?5 AND state IN (?6, ?7, ?8) \
     RETURNING dispatched_at";

pub(super) fn apply(
    tx: &Transaction<'_>,
    write: &EffectWrite,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let key = &write.key.0;
    let changed = match write.state {
        EffectState::Dispatching => execute(
            tx,
            DISPATCH,
            params![
                write.state.as_str(),
                stamp(now, "effects", "dispatched_at")?,
                key,
                EffectState::Planned.as_str()
            ],
        )?,
        EffectState::Unconfirmed => execute(
            tx,
            RESTART,
            params![write.state.as_str(), key, EffectState::Dispatching.as_str()],
        )?,
        EffectState::Failed | EffectState::Acknowledged => result_commit(tx, write, now)?,
        // Refused by `check_well_formed` before the transaction opened.
        EffectState::Planned => 0,
    };
    if changed == 0 {
        return Err(ApplyError::Conflict {
            kind: ConflictKind::Effect,
            key: key.clone(),
        });
    }
    Ok(())
}

/// The result commit: `1` when a row in a source state took the write,
/// `0` when none did.
fn result_commit(
    tx: &Transaction<'_>,
    write: &EffectWrite,
    now: Timestamp,
) -> Result<usize, ApplyError> {
    // `failed` closes a stranded `planned`/`unconfirmed` row as well as a
    // `dispatching` one; `acknowledged` only ever follows `dispatching`.
    let sources = if write.state == EffectState::Failed {
        [
            EffectState::Dispatching,
            EffectState::Planned,
            EffectState::Unconfirmed,
        ]
    } else {
        [EffectState::Dispatching; 3]
    };
    let matched: Option<Option<String>> = tx
        .query_row(
            RESULT,
            params![
                write.state.as_str(),
                write.certainty.map(|c| c.as_str()),
                result_json(write.receipt.as_ref())?,
                stamp(now, "effects", "completed_at")?,
                write.key.0,
                sources[0].as_str(),
                sources[1].as_str(),
                sources[2].as_str()
            ],
            |row| row.get(0),
        )
        .optional()?;
    crash_checkpoint();
    let Some(dispatched_at) = matched else {
        return Ok(0);
    };
    if let Some(EffectReceipt::Judgments(record)) = &write.receipt {
        let requested_at = match dispatched_at {
            Some(text) => ts_decode(&text, "effects", "dispatched_at")?,
            None => now,
        };
        write_judgments(tx, record, requested_at, now)?;
    }
    Ok(1)
}

fn write_judgments(
    tx: &Transaction<'_>,
    record: &JudgmentRecord,
    requested_at: Timestamp,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let answered_at = (record.set.outcome == JudgmentOutcome::Answered).then_some(now);
    let set = JudgmentSetRow::from_core(&record.set, requested_at, answered_at)?;
    insert(tx, "INSERT OR IGNORE", "judgment_sets", &set.params(), "")?;
    for judgment in &record.judgments {
        let row = JudgmentRow::from_core(&record.set.id, judgment)?;
        insert(tx, "INSERT OR IGNORE", "judgments", &row.params(), "")?;
    }
    Ok(())
}
