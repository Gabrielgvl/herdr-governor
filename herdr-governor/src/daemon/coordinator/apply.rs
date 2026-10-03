//! `coordinator/apply` — the §4.2 apply mechanics every arm shares: one
//! bounded `store.apply` loop that recomputes its `Transition` against a
//! fresh store read between CAS attempts, its outcome vocabulary, and
//! the restart-mark counts `mark_restart` reports.

use governor_core::identity::Timestamp;
use governor_core::lifecycle::Transition;

use crate::daemon::log;
use crate::store::{ApplyError, Store};

/// The apply bound §4.2 pins: re-read and recompute ≤ 3 attempts, then log
/// and drop.
const APPLY_BOUND: usize = 3;

/// The outcome of one bounded apply (§4.2's CAS contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum ApplyOutcome {
    /// The transition committed — `attempts` counts the tries taken.
    Applied {
        /// How many tries it took (1 = clean).
        attempts: usize,
    },
    /// Every attempt lost the CAS — logged and dropped, never retried past
    /// the bound.
    Dropped {
        /// Always 3 — the bound.
        attempts: usize,
    },
}

/// §4.3 step 5's counts — how many `dispatching` effects restart marked
/// `unconfirmed`, over how many Runs and stranded evaluations.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::daemon) struct Marks {
    /// `dispatching` rows seen (the input set's size).
    pub effects: usize,
    /// Runs whose restart transition applied.
    pub runs: usize,
    /// Stranded `evaluating` launches abstained.
    pub evals: usize,
}

/// §4.2 — one `store.apply` attempt per loop, recomputing the transition
/// against a fresh store read between attempts. `Conflict` retries; every
/// other `ApplyError` aborts (it is a bug or corruption, not a race).
/// An empty transition counts as a clean `Applied` without paying a
/// transaction.
pub(in crate::daemon) fn apply_with_retry(
    store: &mut Store,
    now: Timestamp,
    mut recompute: impl FnMut(&Store) -> Transition,
) -> Result<ApplyOutcome, ApplyError> {
    for attempt in 1..=APPLY_BOUND {
        let transition = recompute(store);
        let sizes = (
            transition.state_changes.len(),
            transition.events.len(),
            transition.effects.len(),
        );
        if sizes == (0, 0, 0) {
            return Ok(ApplyOutcome::Applied { attempts: attempt });
        }
        match store.apply(&transition, now) {
            Ok(()) => {
                log::applied(attempt, sizes.0, sizes.1, sizes.2);
                return Ok(ApplyOutcome::Applied { attempts: attempt });
            }
            Err(ApplyError::Conflict { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    log::apply_dropped(APPLY_BOUND, "conflict");
    Ok(ApplyOutcome::Dropped {
        attempts: APPLY_BOUND,
    })
}

/// §4.2 [r3] — concatenate `Transition`s into one apply: the governor-refused
/// path composes `[WriteEffect::Dispatch]` + `transition(Event::EffectResult)`
/// into a single commit. The three vecs append in order — `Transition` has
/// no other fields to merge.
#[expect(
    dead_code,
    reason = "the composed-transition consumer lands with P5.B1's DispatchCommit path"
)]
pub(in crate::daemon) fn concat(
    mut first: Transition,
    rest: impl IntoIterator<Item = Transition>,
) -> Transition {
    for next in rest {
        first.state_changes.extend(next.state_changes);
        first.events.extend(next.events);
        first.effects.extend(next.effects);
    }
    first
}
