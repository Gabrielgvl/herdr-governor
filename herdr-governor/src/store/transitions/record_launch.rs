//! F11/Appendix B — the phase-guarded `launches` write. `evaluating` is the
//! admission `INSERT`; every later phase is an `UPDATE … WHERE phase IN
//! (<legal predecessors>)` from the P4.0 matrix (`routed` ← `evaluating`;
//! `launching` ← `routed`; `done` ← `evaluating`/`routed`/`launching`).
//! `done` is nobody's predecessor, so a terminal row is immutable by
//! construction; a write that matches no row is a [`ApplyError::PhaseConflict`].

use rusqlite::Transaction;
use rusqlite::types::Value as SqlValue;

use governor_core::identity::Timestamp;
use governor_core::task::{Launch, LaunchPhase};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::launch::LaunchRow;
use crate::store::transitions::{insert_unique, only, resolve_caller, update};

/// The columns a phase write may change; the caller binding, the Task and
/// `created_at` are admission-immutable.
const PHASE_COLUMNS: [&str; 7] = [
    "phase",
    "outcome",
    "outcome_reason",
    "decision_json",
    "config_version",
    "result_json",
    "updated_at",
];

/// The phases a write to `phase` may find the row in (P4.0 matrix).
fn predecessors(phase: LaunchPhase) -> &'static [LaunchPhase] {
    match phase {
        LaunchPhase::Evaluating => &[],
        LaunchPhase::Routed => &[LaunchPhase::Evaluating],
        LaunchPhase::Launching => &[LaunchPhase::Routed],
        LaunchPhase::Done => &[
            LaunchPhase::Evaluating,
            LaunchPhase::Routed,
            LaunchPhase::Launching,
        ],
    }
}

pub(super) fn apply(
    tx: &Transaction<'_>,
    launch: &Launch,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let caller_id = resolve_caller(tx, &launch.caller)?;
    let row = LaunchRow::from_core(launch, caller_id, now)?;
    if launch.phase == LaunchPhase::Evaluating {
        return insert_unique(
            tx,
            "launches",
            &row.params(),
            ConflictKind::Launch,
            &launch.id.0,
        );
    }
    let legal = predecessors(launch.phase);
    let mut where_vals = vec![SqlValue::from(launch.id.0.clone())];
    where_vals.extend(legal.iter().map(|p| SqlValue::from(p.as_str().to_owned())));
    let slots = (2..)
        .take(legal.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let changed = update(
        tx,
        "launches",
        &only(row.params(), &PHASE_COLUMNS),
        &format!("launch_id = ?1 AND phase IN ({slots})"),
        &where_vals,
    )?;
    if changed == 0 {
        return Err(ApplyError::PhaseConflict {
            launch: launch.id.0.clone(),
            phase: launch.phase.as_str(),
        });
    }
    Ok(())
}
