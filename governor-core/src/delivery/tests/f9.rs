//! F9/F17 — dispatch eligibility: the ordering barrier, the single prompt
//! slot per captured identity, and F17's safe moment.

use crate::config::Capability;
use crate::delivery::{
    OutboxMessage, OutboxState, next_dispatchable_follow_up, prompt_dispatchable,
};
use crate::identity::{ChildStatus, EffectKey, PaneId, RunId};
use crate::lifecycle::{
    Effect, EffectKind, EffectState, EffectTarget, PromptCertainty, Run, State,
};

use super::builders::{
    child_prompt_effect, child_prompt_keyed, dispatched, hint, message, qualified, run, settled_run,
};

#[test]
fn f17_queued_follow_up_is_eligible_when_idle_or_done() {
    let queue = [message(1, "k1")];
    for status in [ChildStatus::Idle, ChildStatus::Done] {
        let mut observed = run();
        observed.child_status = Some(status);
        assert_eq!(
            next_dispatchable_follow_up(&observed, &queue, &[], &[]).map(|m| m.seq),
            Some(1),
            "idle/done is the safe moment without mid_turn_input (F17)"
        );
    }
}

#[test]
fn f17_mid_turn_input_sends_immediately() {
    let queue = [message(1, "k1")];
    let mid_turn = qualified(&[Capability::MID_TURN_INPUT]);
    for status in [Some(ChildStatus::Working), None] {
        let mut observed = run();
        observed.child_status = status;
        assert_eq!(
            next_dispatchable_follow_up(&observed, &queue, &[], &mid_turn).map(|m| m.seq),
            Some(1),
            "a qualified mid_turn_input sends immediately (F17)"
        );
    }
}

#[test]
fn f17_never_sends_while_blocked() {
    let queue = [message(1, "k1")];
    let mid_turn = qualified(&[Capability::MID_TURN_INPUT]);
    let mut blocked = run();
    blocked.child_status = Some(ChildStatus::Blocked);
    assert_eq!(
        next_dispatchable_follow_up(&blocked, &queue, &[], &[]),
        None,
        "never while blocked (F17/H#17)"
    );
    assert_eq!(
        next_dispatchable_follow_up(&blocked, &queue, &[], &mid_turn),
        None,
        "mid_turn_input never beats blocked (F17/H#17)"
    );
}

#[test]
fn f17_waits_while_working_without_mid_turn_input() {
    let queue = [message(1, "k1")];
    for (status, caps) in [
        (Some(ChildStatus::Working), qualified(&[])),
        (None, qualified(&[])),
        (Some(ChildStatus::Working), qualified(&["followup_read"])),
    ] {
        let mut observed = run();
        observed.child_status = status;
        assert_eq!(
            next_dispatchable_follow_up(&observed, &queue, &[], &caps),
            None,
            "without qualified mid_turn_input the child must be idle/done (F17)"
        );
    }
}

#[test]
fn f17_nothing_dispatches_after_settlement() {
    let queue = [message(1, "k1")];
    assert_eq!(
        next_dispatchable_follow_up(&settled_run(), &queue, &[], &[]),
        None,
        "settlement ends delivery (F17/F20)"
    );
}

#[test]
fn f9_follow_ups_never_overtake_the_task_prompt() {
    let queue = [message(1, "k1")];
    for state in [State::Reserved, State::Starting, State::Prompting] {
        let mut early = run();
        early.state = state;
        assert_eq!(
            next_dispatchable_follow_up(&early, &queue, &[], &[]),
            None,
            "the Task prompt precedes every follow-up (F9)"
        );
    }
    for state in [State::Judging, State::Repair] {
        let mut supervised = run();
        supervised.state = state;
        assert_eq!(
            next_dispatchable_follow_up(&supervised, &queue, &[], &[]).map(|m| m.seq),
            Some(1),
            "judging and repair stay deliverable (F17/F24)"
        );
    }
}

#[test]
fn f9_prompts_to_one_identity_dispatch_one_at_a_time_in_order() {
    // The earliest pipeline-holding entry is the only candidate; a
    // submitted head does not block the queue behind it.
    let queue = [
        dispatched(1, "k1", OutboxState::Submitted),
        message(2, "k2"),
        message(3, "k3"),
    ];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &queue, &[], &[]).map(|m| m.seq),
        Some(2),
        "the head of the queue dispatches first (F9/F17)"
    );
    // A prompt effect in flight occupies the single slot.
    for state in [EffectState::Planned, EffectState::Dispatching] {
        assert_eq!(
            next_dispatchable_follow_up(&run(), &queue, &[child_prompt_effect(state)], &[]),
            None,
            "one prompt at a time per captured identity (F9)"
        );
    }
    // A dispatching queue entry holds the slot for the rest of the queue.
    let held = [
        dispatched(1, "k1", OutboxState::Dispatching),
        message(2, "k2"),
    ];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &held, &[], &[]),
        None,
        "a dispatching entry is the slot-holder, not a later queued one (F9)"
    );
}

#[test]
fn f9_an_unconfirmed_prompt_is_an_ordering_barrier() {
    let queue = [message(1, "k1")];
    // A journaled prompt effect left unconfirmed bars the queue.
    assert_eq!(
        next_dispatchable_follow_up(
            &run(),
            &queue,
            &[child_prompt_effect(EffectState::Unconfirmed)],
            &[],
        ),
        None,
        "an unconfirmed prompt effect bars delivery (F9)"
    );
    // The Task prompt's own unconfirmed certainty bars it too.
    let mut unconfirmed_prompt = run();
    unconfirmed_prompt.prompt_certainty = Some(PromptCertainty::Unconfirmed);
    assert_eq!(
        next_dispatchable_follow_up(&unconfirmed_prompt, &queue, &[], &[]),
        None,
        "an unconfirmed Task prompt bars follow-ups (F9/F16)"
    );
    // And an unconfirmed entry at the head holds the queue.
    let held = [
        dispatched(1, "k1", OutboxState::Unconfirmed),
        message(2, "k2"),
    ];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &held, &[], &[]),
        None,
        "an unconfirmed outbox entry is the barrier (F9/F17)"
    );
}

#[test]
fn f9_a_resolved_prompt_frees_the_pipeline() {
    let queue = [message(1, "k1")];
    for state in [EffectState::Acknowledged, EffectState::Failed] {
        assert_eq!(
            next_dispatchable_follow_up(&run(), &queue, &[child_prompt_effect(state)], &[])
                .map(|m| m.seq),
            Some(1),
            "a resolved prompt neither bars nor occupies the slot (F9)"
        );
    }
    // An unconfirmed prompt to a *different* captured identity — a hint
    // addresses the owner's pane — is not this queue's barrier.
    let unconfirmed_hint = Effect {
        key: EffectKey("event:ev1:hint".into()),
        target: Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
        ..child_prompt_effect(EffectState::Unconfirmed)
    };
    assert_eq!(
        next_dispatchable_follow_up(&run(), &queue, &[unconfirmed_hint], &[]).map(|m| m.seq),
        Some(1),
        "an unconfirmed hint prompt is not the child's barrier (F9)"
    );
}

#[test]
fn f9_only_prompts_to_this_identity_serialize() {
    let queue = [message(1, "k1")];
    // A hint prompt targets the owner's pane — a different captured
    // identity — so it never bars the child's queue.
    let hint = Effect {
        key: EffectKey("event:ev1:hint".into()),
        subject_run: Some(RunId("r1".into())),
        target: Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
        ..child_prompt_effect(EffectState::Dispatching)
    };
    // A prompt for a different run and a non-prompt effect are likewise
    // outside this identity's pipeline.
    let foreign = Effect {
        key: EffectKey("run:r2:outbox:1".into()),
        subject_run: Some(RunId("r2".into())),
        ..child_prompt_effect(EffectState::Dispatching)
    };
    let jev = Effect {
        kind: EffectKind::JevEvaluate,
        target: None,
        ..child_prompt_effect(EffectState::Dispatching)
    };
    assert_eq!(
        next_dispatchable_follow_up(&run(), &queue, &[hint, foreign, jev], &[]).map(|m| m.seq),
        Some(1),
        "only prompts to this Run's captured identity hold its slot (F9)"
    );
}

#[test]
fn f9_empty_or_foreign_queues_dispatch_nothing() {
    assert_eq!(
        next_dispatchable_follow_up(&run(), &[], &[], &[]),
        None,
        "an empty outbox dispatches nothing"
    );
    let foreign = OutboxMessage {
        run: RunId("r2".into()),
        ..message(1, "k")
    };
    assert_eq!(
        next_dispatchable_follow_up(&run(), &[foreign], &[], &[]),
        None,
        "another Run's outbox is not this Run's queue"
    );
}

#[test]
fn f9_a_submitted_follow_up_lifts_its_journal_barrier() {
    // The journal still says `unconfirmed` (the F8 wire fact), but the
    // outbox entry it dispatched was resolved `submitted` by transcript
    // evidence: the outbox row is the barrier's source of truth (F9/F17).
    let unconfirmed = child_prompt_effect(EffectState::Unconfirmed);
    let linked = |state: OutboxState| OutboxMessage {
        effect: Some(unconfirmed.id.clone()),
        ..dispatched(1, "k1", state)
    };
    let lifted = [linked(OutboxState::Submitted), message(2, "k2")];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &lifted, core::slice::from_ref(&unconfirmed), &[])
            .map(|m| m.seq),
        Some(2),
        "a submitted follow-up lifts the barrier its unconfirmed prompt effect held (F9)"
    );
    // The linked entry still `unconfirmed` keeps the barrier.
    let held = [linked(OutboxState::Unconfirmed), message(2, "k2")];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &held, core::slice::from_ref(&unconfirmed), &[]),
        None,
        "an unresolved linked entry keeps the barrier (F9)"
    );
    // A submitted entry of another Run never lifts this Run's barrier, even
    // when it names the same effect id: the lift is scoped to the Run.
    let foreign = [
        OutboxMessage {
            run: RunId("run-other".into()),
            ..linked(OutboxState::Submitted)
        },
        message(2, "k2"),
    ];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &foreign, core::slice::from_ref(&unconfirmed), &[]),
        None,
        "only this Run's own submitted entry lifts its barrier (F9)"
    );
    // An unconfirmed effect no submitted entry links stays today's barrier.
    let unlinked = [
        dispatched(1, "k1", OutboxState::Submitted),
        message(2, "k2"),
    ];
    assert_eq!(
        next_dispatchable_follow_up(&run(), &unlinked, &[unconfirmed], &[]),
        None,
        "only the entry linked by effect_id lifts its own barrier (F9)"
    );
}

#[test]
fn f9_prompt_dispatchable_with_candidate_in_journal() {
    // The DispatchCommit revalidation gate: the candidate is itself a
    // `planned` journal row, so the F9 slot test must exclude it while
    // keeping the plan order against its siblings.
    let mut prompting = run();
    prompting.state = State::Prompting;
    prompting.prompt_certainty = None;
    for (suffix, at_state) in [
        ("prompt:task", prompting.clone()),
        ("nudge:0", run()),
        ("outbox:1", run()),
    ] {
        let candidate = child_prompt_keyed(suffix, EffectState::Planned);
        let check = |observed: &Run, slice: &[Effect]| {
            prompt_dispatchable(observed, &[], slice, &candidate.key)
        };
        assert!(
            check(&at_state, core::slice::from_ref(&candidate)),
            "a planned {suffix} prompt with a clear pipeline dispatches (F9)"
        );
        // The ordering barrier: an unlifted unconfirmed child prompt, or
        // the run's own unconfirmed prompt certainty.
        let unconfirmed_sibling = [
            child_prompt_keyed("outbox:7", EffectState::Unconfirmed),
            candidate.clone(),
        ];
        assert!(
            !check(&at_state, &unconfirmed_sibling),
            "an unconfirmed sibling prompt bars {suffix} (F9)"
        );
        let mut barred = at_state.clone();
        barred.prompt_certainty = Some(PromptCertainty::Unconfirmed);
        assert!(
            !check(&barred, core::slice::from_ref(&candidate)),
            "an unconfirmed prompt_certainty bars {suffix} (F9)"
        );
        // The single slot: another dispatching child prompt excludes it.
        let dispatching_sibling = [
            candidate.clone(),
            child_prompt_keyed("outbox:8", EffectState::Dispatching),
        ];
        assert!(
            !check(&at_state, &dispatching_sibling),
            "one prompt at a time per captured identity — {suffix} waits (F9)"
        );
        // Plan order: an earlier planned sibling precedes; a later one
        // does not block the candidate.
        let earlier = [
            child_prompt_keyed("outbox:9", EffectState::Planned),
            candidate.clone(),
        ];
        assert!(
            !check(&at_state, &earlier),
            "an earlier planned sibling holds the order against {suffix} (F9)"
        );
        let later = [
            candidate.clone(),
            child_prompt_keyed("outbox:9", EffectState::Planned),
        ];
        assert!(
            check(&at_state, &later),
            "a later planned sibling does not block {suffix} (F9)"
        );
        // Never prompt a blocked child.
        let mut blocked = at_state.clone();
        blocked.child_status = Some(ChildStatus::Blocked);
        assert!(
            !check(&blocked, core::slice::from_ref(&candidate)),
            "a blocked child is never prompted — {suffix} (F17/H#17)"
        );
    }
}

#[test]
fn f9_two_planned_prompts_dispatch_in_plan_order() {
    let first = child_prompt_keyed("outbox:1", EffectState::Planned);
    let second = child_prompt_keyed("outbox:2", EffectState::Planned);
    let journal = [first.clone(), second.clone()];
    assert!(
        prompt_dispatchable(&run(), &[], &journal, &first.key),
        "the earliest planned prompt is dispatchable (F9)"
    );
    assert!(
        !prompt_dispatchable(&run(), &[], &journal, &second.key),
        "the later planned prompt waits behind the head (F9)"
    );
    // Position in the slice is the plan order (ORDER BY planned_at,
    // effect_id) — the same two rows reversed flip the answer.
    let reversed = [second.clone(), first.clone()];
    assert!(
        prompt_dispatchable(&run(), &[], &reversed, &second.key),
        "slice order is the F9 order"
    );
    assert!(
        !prompt_dispatchable(&run(), &[], &reversed, &first.key),
        "slice order is the F9 order"
    );
}

#[test]
fn f9_hint_is_never_gated_by_the_child_queue() {
    // A hint addresses the owner's pane, a different captured identity:
    // a dispatching or unconfirmed one neither holds the child's prompt
    // slot nor bars its queue.
    let candidate = child_prompt_keyed("outbox:1", EffectState::Planned);
    let mut unconfirmed_hint = hint(EffectState::Unconfirmed);
    unconfirmed_hint.key = EffectKey("event:ev2:hint".into());
    let journal = [
        hint(EffectState::Dispatching),
        unconfirmed_hint,
        candidate.clone(),
    ];
    assert!(
        prompt_dispatchable(&run(), &[], &journal, &candidate.key),
        "in-flight hints are outside the child's serialization (F9/F18)"
    );
}
