//! F21 — `recoveryOf`: the successor Launch's derivation (key and Task)
//! and the caller-requested admission gate.

use alloc::format;

use crate::config::Policy;
use crate::identity::{CallerKey, ChildStatus, IdempotencyKey, Observation, RunId, Timestamp};
use crate::lifecycle::{Run, Settlement};
use crate::task::{Refusal, Task};

use super::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};

/// F21 — the successor Launch's idempotency key is
/// `"recovery:" ++ predecessorRunId`, which makes one recovery per
/// predecessor and never collides with a caller key.
pub const RECOVERY_KEY_PREFIX: &str = "recovery:";

/// F21 — the successor Launch's idempotency key:
/// `"recovery:" ++ predecessorRunId` — one recovery per predecessor, and it
/// never collides with a caller key.
#[must_use]
pub fn successor_key(predecessor: &RunId) -> IdempotencyKey {
    IdempotencyKey(format!("{RECOVERY_KEY_PREFIX}{}", predecessor.0))
}

/// F21 — the successor Task: the predecessor's Task plus the prescribed
/// preamble — continue from the observed git and transcript state, and do not
/// repeat side effects that already happened. `recovery_of` re-keys to the
/// immediate predecessor so routing applies the F13 step-4 recovery minimum
/// and the predecessor's provider exclusion.
#[must_use]
pub fn successor_task(predecessor: &RunId, task: &Task) -> Task {
    const PREAMBLE: &str = "This Task continues a predecessor Run's work. \
        Continue from the observed git and transcript state; do not repeat \
        side effects that already happened.";
    Task {
        objective: format!(
            "{PREAMBLE}\n\npredecessor_run_id: {}\n\n{}",
            predecessor.0, task.objective
        ),
        recovery_of: Some(predecessor.clone()),
        ..task.clone()
    }
}

/// F21/ADR-0003 — the dispatch precondition: only a fresh snapshot showing
/// the predecessor's identity `absent` permits dispatch. `unique` and
/// `invalid` never prove a stop — `invalid` never counts as absence (F3).
#[must_use]
pub fn dispatch_ready(observation: &Observation) -> bool {
    match observation {
        Observation::Absent => true,
        Observation::Unique {
            status: _,
            pane: _,
            native_session: _,
        }
        | Observation::Invalid => false,
    }
}

/// F21 — the `recoveryOf` admission rules for a caller-requested recovery:
///
/// - the caller must own the predecessor (F4 — claiming a Run's obligation
///   is a Run operation) — `Err(NOT_OWNER)`;
/// - an existing obligation is claimed iff it is an unclaimed `pending`
///   `provider_limit` one; any other existing obligation is a second
///   recovery of the same predecessor — `Err(RECOVERY_EXISTS)`;
/// - the predecessor must be settled — `Err(RECOVERY_PREDECESSOR_UNSETTLED)`;
/// - a `provider_limited` predecessor must be observed `absent` first
///   (ADR-0003); any other settled predecessor must be observed `idle`,
///   `done` or `absent` — `invalid` never proves anything (F3). While the
///   observation gate is unmet the request is refused
///   `Err(RECOVERY_PREDECESSOR_ACTIVE)`, retryable once the gate is met.
///
/// `Ok(_)` is the `pending` obligation to record — a claimed
/// `provider_limit` obligation returned unchanged, or a new `caller`-origin
/// one expiring `recovery_expiry` after `now`. The obligation stays
/// `pending` through successor admission: `dispatched` rides the
/// successor's Route transaction and `blocked` its abstention (F21,
/// Appendix B's "Recovery dispatch"), so nothing here binds a
/// `successor_launch_id` — the recoveries CHECK forbids one while pending.
pub fn caller_admission(
    predecessor: &Run,
    obligation: Option<&RecoveryObligation>,
    observation: &Observation,
    caller: &CallerKey,
    now: Timestamp,
    policy: &Policy,
) -> Result<RecoveryObligation, Refusal> {
    if predecessor.owner != *caller {
        return Err(Refusal::NotOwner);
    }
    let claim = match obligation {
        Some(existing)
            if existing.origin == RecoveryOrigin::ProviderLimit
                && existing.status == RecoveryStatus::Pending =>
        {
            Some(existing)
        }
        Some(_) => return Err(Refusal::RecoveryExists),
        None => None,
    };
    let Some(settled) = predecessor.settlement else {
        return Err(Refusal::RecoveryPredecessorUnsettled);
    };
    let observed = match settled {
        Settlement::ProviderLimited => dispatch_ready(observation),
        Settlement::Accepted
        | Settlement::Rejected
        | Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::Unresolved { reason: _ } => match observation {
            Observation::Absent => true,
            Observation::Unique {
                status,
                pane: _,
                native_session: _,
            } => match status {
                Some(ChildStatus::Idle | ChildStatus::Done) => true,
                Some(ChildStatus::Working | ChildStatus::Blocked) | None => false,
            },
            Observation::Invalid => false,
        },
    };
    if !observed {
        return Err(Refusal::RecoveryPredecessorActive);
    }
    Ok(match claim {
        Some(existing) => existing.clone(),
        None => RecoveryObligation::pending(
            predecessor.id.clone(),
            RecoveryOrigin::Caller,
            now,
            policy.recovery_expiry,
        ),
    })
}
