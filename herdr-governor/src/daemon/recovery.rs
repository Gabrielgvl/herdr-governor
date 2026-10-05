//! `recovery` — §4.10's tick sweep over the `pending` obligations
//! (F21): the expiry sweep that skips an obligation whose successor is
//! in flight, and the dispatch that admits the deterministic successor
//! Launch once a fresh snapshot proves the predecessor's identity
//! `absent`. The obligation is never written here — it stays `pending`
//! through admission (the recoveries CHECK forbids a successor id while
//! pending); `dispatched` rides the successor's Route transaction and
//! `blocked` its abstention (`launch/context::recovery_move`).

use governor_core::identity::{Observation, Timestamp};
use governor_core::lifecycle::{Run, StateChange, Transition};
use governor_core::recovery::{
    RecoveryObligation, RecoveryStatus, dispatch_ready, successor_key, successor_task,
};
use governor_core::task::{Launch, LaunchPhase, admit, new_launch};

use crate::adapters::herdr::{Observed, SessionSnapshot};
use crate::daemon::coordinator::apply::apply_with_retry;
use crate::daemon::coordinator::empty;
use crate::daemon::{DaemonError, identity, ids};
use crate::store::Store;

/// §4.7 step 7 — one sweep over every `pending` obligation. Each
/// obligation's own apply re-derives its decision against fresh state:
/// an in-flight successor (found by the deterministic key, any
/// idempotency scope) skips the expiry check entirely; else
/// `expired(now)` records `failed`; else `dispatch_ready` on the
/// snapshot's classification admits the successor Launch plus its
/// planned `jev_evaluate` in one transaction.
pub(in crate::daemon) fn pass(
    store: &mut Store,
    now: Timestamp,
    snapshot: Option<&Observed<SessionSnapshot>>,
) -> Result<(), DaemonError> {
    for obligation in store.recoveries_by_state(RecoveryStatus::Pending)? {
        sweep(store, &obligation, now, snapshot)?;
    }
    Ok(())
}

/// One obligation's bounded apply — the recompute IS the decision, so
/// every CAS attempt re-checks the gates against durable state: a
/// successor that landed mid-retry (a caller's `recoveryOf`, or this
/// admission's own committed row) ends the pass, and an obligation a
/// racing commit moved off `pending` writes nothing.
fn sweep(
    store: &mut Store,
    obligation: &RecoveryObligation,
    now: Timestamp,
    snapshot: Option<&Observed<SessionSnapshot>>,
) -> Result<(), DaemonError> {
    // The successor row is minted once — a recompute that still admits
    // names the same `RecordLaunch`; a committed attempt is seen by the
    // successor read inside the recompute.
    let Some(successor) = mint_successor(store, obligation, now)? else {
        return Ok(());
    };
    apply_with_retry(store, now, |st| {
        let pending = st
            .recoveries_by_state(RecoveryStatus::Pending)
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.predecessor == obligation.predecessor);
        let Some(current) = pending else {
            return empty();
        };
        // In-flight successor — the expiry sweep skips it (§4.10): the
        // successor's own bounds (jev timeout, the restart abstention)
        // still apply. This read sees this admission's committed row too.
        let existing = st
            .recovery_successor(&obligation.predecessor)
            .ok()
            .flatten();
        if existing
            .as_ref()
            .is_some_and(|launch| launch.phase != LaunchPhase::Done)
        {
            return empty();
        }
        if let Some(failed) = current.expired(now) {
            return Transition {
                state_changes: Vec::from([StateChange::RecordRecovery(failed)]),
                events: Vec::new(),
                effects: Vec::new(),
            };
        }
        // A `done` successor with a still-pending obligation cannot arise
        // (the Route/abstention transaction moves both atomically) — but
        // the one-recovery-per-predecessor key forbids a second admission
        // whatever the row says, so the pass stops here rather than
        // commit a conflicting `RecordLaunch`.
        if existing.is_some() {
            return empty();
        }
        let Some(predecessor) = st.run(&obligation.predecessor).ok().flatten() else {
            return empty();
        };
        if !dispatch_ready(&classify(&predecessor, snapshot)) {
            return empty();
        }
        admit(&successor)
    })?;
    Ok(())
}

/// The successor `new_launch` for an obligation — built before the
/// apply so every recompute names the same row; `admit` only writes it
/// when the gates above hold. The predecessor's owner, project root
/// and Task carry over (`successor_task` re-keys `recovery_of` and adds
/// the F21 preamble).
fn mint_successor(
    store: &Store,
    obligation: &RecoveryObligation,
    now: Timestamp,
) -> Result<Option<Launch>, DaemonError> {
    let Some(predecessor) = store.run(&obligation.predecessor)? else {
        return Ok(None);
    };
    let Some(launched) = store.launch(&predecessor.launch)? else {
        return Ok(None);
    };
    Ok(Some(new_launch(
        ids::mint_launch_id(now)?,
        predecessor.owner.clone(),
        launched.project_root.clone(),
        successor_key(&obligation.predecessor),
        successor_task(&obligation.predecessor, &launched.task),
    )))
}

/// The predecessor's F3 classification over the pass's snapshot — the
/// same derivation `recovery_admission` uses: an identity the read
/// cannot re-prove is `Invalid` (never absence), and a Run that never
/// captured one provably has no live child.
fn classify(run: &Run, snapshot: Option<&Observed<SessionSnapshot>>) -> Observation {
    match (&run.identity, snapshot) {
        (Some(child), Some(observed)) => governor_core::identity::classify(
            child,
            Some(&identity::incarnation(&observed.epoch)),
            &identity::agent_rows(&observed.value),
        ),
        (Some(_), None) => Observation::Invalid,
        (None, _) => Observation::Absent,
    }
}
