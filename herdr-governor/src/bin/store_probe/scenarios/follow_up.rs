//! The P4.1 follow-up edges (Appendix B "Dispatch a follow-up" /
//! "Resolve a follow-up"): the outbox row moves in the same transaction as
//! its prompt effect's journal write — `queued` → `dispatching` with the
//! effect's dispatch commit, `dispatching` → `submitted` with its
//! acknowledgement. The prompt effect is planned in the seed (the
//! `effect_id` FOREIGN KEY is immediate).

use governor_core::delivery::{FollowUpWrite, OutboxState, follow_up_effect};
use governor_core::identity::{Digest, EffectId};
use governor_core::lifecycle::{Effect, StateChange, Transition};

use super::{
    acknowledge, chain, changes, concat, dispatch, enqueue_follow_up, follow_up_message, identity,
    routed, run_reserved,
};

/// The `prompt` effect dispatching `r-1`'s follow-up: `run:r-1:outbox:1`.
fn outbox_effect() -> Effect {
    follow_up_effect(
        &follow_up_message(),
        identity(),
        EffectId("eff:run:r-1:outbox:1".into()),
        Digest([0x45; 32]),
    )
}

/// Appendix B "Plan an effect" for the outbox prompt.
fn plan_outbox_effect() -> Transition {
    Transition {
        state_changes: vec![],
        events: vec![],
        effects: vec![outbox_effect()],
    }
}

/// `routed` + the follow-up enqueued + its prompt effect planned.
pub(super) fn dispatch_seed() -> Vec<Transition> {
    chain(routed(), [enqueue_follow_up(), plan_outbox_effect()])
}

/// The prompt effect `planned` → `dispatching` ++ the outbox row `queued`
/// → `dispatching` linking it.
pub(super) fn dispatch_follow_up() -> Transition {
    concat(
        dispatch(outbox_effect().key),
        changes(vec![StateChange::WriteFollowUp(FollowUpWrite::Dispatch {
            run: run_reserved().id,
            seq: 1,
            effect: outbox_effect().id,
        })]),
    )
}

/// [`dispatch_seed`] with the dispatch committed.
pub(super) fn resolve_seed() -> Vec<Transition> {
    chain(dispatch_seed(), [dispatch_follow_up()])
}

/// The prompt effect acknowledged ++ the outbox row `dispatching` →
/// `submitted`.
pub(super) fn resolve_follow_up() -> Transition {
    changes(vec![
        acknowledge(outbox_effect().key, None),
        StateChange::WriteFollowUp(FollowUpWrite::Resolve {
            run: run_reserved().id,
            seq: 1,
            state: OutboxState::Submitted,
        }),
    ])
}
