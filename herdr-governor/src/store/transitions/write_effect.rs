//! F8/Appendix B — the journal-row writes, one conditional UPDATE per
//! [`EffectWrite`] kind, every WHERE naming exactly the states the kind
//! transitions from:
//!
//! - `Dispatch`: `dispatching` ← `planned` (`dispatched_at` stamped,
//!   `payload_digest` untouched — fixed at plan time);
//! - `Result`: the resolution's state ← `dispatching` only — `acknowledged`
//!   with its receipt, `failed` with its certainty and the OQ-11 cause as
//!   `result_json {"error": …}`, or `unconfirmed` (the F28 restart marking);
//!   `completed_at` stamped;
//! - `Terminal`: `failed` ← `planned` | `dispatching` | `unconfirmed` — the
//!   OQ-13 write closing a stranded row with the certainty the core chose,
//!   `result_json` NULL.
//!
//! `unconfirmed` has no exit but the terminal `failed` — never a re-dispatch.
//! The shape of each write is the core type's: no certainty is ever missing
//! and no receipt ever rides a failure, so nothing is refused here. A
//! `Judgments` receipt also writes `judgment_sets`/`judgments` (dedup on
//! `set_id` resp. `(set_id, question)` — idempotent redelivery),
//! `requested_at` being the row's `dispatched_at`. No match is a
//! [`ConflictKind::Effect`] conflict.

use rusqlite::{OptionalExtension as _, Transaction, params};

use governor_core::identity::Timestamp;
use governor_core::lifecycle::{EffectReceipt, EffectResolution, EffectState, EffectWrite};
use governor_core::routing::{JudgmentOutcome, JudgmentRecord};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::effect::result_json;
use crate::store::rows::judgment::{JudgmentRow, JudgmentSetRow};
use crate::store::rows::ts_decode;
use crate::store::transitions::{crash_checkpoint, execute, insert_dedup, stamp};

const DISPATCH: &str = "UPDATE effects SET state = ?1, dispatched_at = ?2 \
     WHERE effect_key = ?3 AND state = ?4";

const RESULT: &str = "UPDATE effects SET state = ?1, certainty = ?2, result_json = ?3, \
     completed_at = ?4 WHERE effect_key = ?5 AND state = ?6 \
     RETURNING dispatched_at";

const TERMINAL: &str = "UPDATE effects SET state = ?1, certainty = ?2, result_json = NULL, \
     completed_at = ?3 WHERE effect_key = ?4 AND state IN (?5, ?6, ?7)";

pub(super) fn apply(
    tx: &Transaction<'_>,
    write: &EffectWrite,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let changed = match write {
        EffectWrite::Dispatch { key } => execute(
            tx,
            DISPATCH,
            params![
                EffectState::Dispatching.as_str(),
                stamp(now, "effects", "dispatched_at")?,
                key.0,
                EffectState::Planned.as_str()
            ],
        )?,
        EffectWrite::Result { key, resolution } => {
            result_commit(tx, &key.0, (write.state(), resolution), now)?
        }
        EffectWrite::Terminal { key, certainty } => execute(
            tx,
            TERMINAL,
            params![
                EffectState::Failed.as_str(),
                certainty.as_str(),
                stamp(now, "effects", "completed_at")?,
                key.0,
                EffectState::Planned.as_str(),
                EffectState::Dispatching.as_str(),
                EffectState::Unconfirmed.as_str()
            ],
        )?,
    };
    if changed == 0 {
        return Err(ApplyError::Conflict {
            kind: ConflictKind::Effect,
            key: write.key().0.clone(),
        });
    }
    Ok(())
}

/// The result commit: `1` when the `dispatching` row took the write, `0`
/// when none did.
fn result_commit(
    tx: &Transaction<'_>,
    key: &str,
    (state, resolution): (EffectState, &EffectResolution),
    now: Timestamp,
) -> Result<usize, ApplyError> {
    let matched: Option<Option<String>> = tx
        .query_row(
            RESULT,
            params![
                state.as_str(),
                resolution.certainty().map(|c| c.as_str()),
                result_json(resolution)?,
                stamp(now, "effects", "completed_at")?,
                key,
                EffectState::Dispatching.as_str()
            ],
            |row| row.get(0),
        )
        .optional()?;
    crash_checkpoint();
    let Some(dispatched_at) = matched else {
        return Ok(0);
    };
    if let Some(EffectReceipt::Judgments(record)) = resolution.receipt() {
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
    insert_dedup(tx, "judgment_sets", &set.params(), &["set_id"])?;
    for judgment in &record.judgments {
        let row = JudgmentRow::from_core(&record.set.id, judgment)?;
        insert_dedup(tx, "judgments", &row.params(), &["set_id", "question"])?;
    }
    Ok(())
}
