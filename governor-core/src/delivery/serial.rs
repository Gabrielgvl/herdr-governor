//! F9 — the per-target ordering the `effect_id` link journals: which queued
//! follow-up, if any, may dispatch to the Run's captured identity now. The
//! gates are the ordering barrier, the single prompt slot and F17's safe
//! moment.

use alloc::collections::BTreeSet;
use alloc::format;

use crate::config::Capability;
use crate::identity::{ChildStatus, EffectId, EffectKey};
use crate::lifecycle::{
    Effect, EffectKind, EffectState, EffectTarget, PromptCertainty, Run, State,
};

use super::{OutboxMessage, OutboxState};

/// F9 — a `planned` or `dispatching` prompt effect occupies the target's
/// single prompt slot; `unconfirmed` is the separate ordering barrier (it
/// is terminal, not in flight).
fn prompt_in_flight(state: EffectState) -> bool {
    match state {
        EffectState::Planned | EffectState::Dispatching => true,
        EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed => false,
    }
}

/// F9 — whether `effect` is a `prompt` journaled for this Run and addressed
/// to its captured `Child` identity — the only rows the per-identity
/// pipeline counts. A hint addresses the *owner's* pane
/// (`EffectTarget::CallerContext`), a different captured identity, and
/// never serializes behind the child's queue.
fn prompts_to_child(run: &Run, effect: &Effect) -> bool {
    effect.kind == EffectKind::Prompt
        && effect.subject_run.as_ref() == Some(&run.id)
        && matches!(effect.target, Some(EffectTarget::Child(_)))
}

/// F9 — the ordering barrier: the Task prompt's `prompt_certainty`, or a
/// journaled `unconfirmed` prompt to the child, bars everything behind it
/// until transcript evidence or settlement resolves it. A journaled
/// `unconfirmed` prompt effect that an outbox entry of this Run links
/// (`effect_id`) is unbarred once that entry is `submitted` — the outbox
/// row is the source of truth for the lift while the journal row keeps its
/// wire fact. The lifted set is built once per call, so the two unbounded
/// histories are each scanned once.
fn barrier(run: &Run, outbox: &[OutboxMessage], prompt_effects: &[Effect]) -> bool {
    let lifted: BTreeSet<&EffectId> = outbox
        .iter()
        .filter(|m| m.run == run.id && m.state == OutboxState::Submitted)
        .filter_map(|m| m.effect.as_ref())
        .collect();
    run.prompt_certainty == Some(PromptCertainty::Unconfirmed)
        || prompt_effects.iter().any(|effect| {
            prompts_to_child(run, effect)
                && effect.state == EffectState::Unconfirmed
                && !lifted.contains(&effect.id)
        })
}

/// F9/F17 — which queued follow-up, if any, may dispatch to the Run's
/// captured identity at this moment.
///
/// The gates, in order:
/// - settlement or a pre-`active` state — the Task prompt precedes every
///   follow-up in F9's order, so nothing overtakes it;
/// - an `unconfirmed` prompt to the identity bars the queue: the Task
///   prompt's `prompt_certainty`, a journaled `unconfirmed` prompt effect,
///   or an `unconfirmed` entry at the head (F9 — until transcript evidence
///   or settlement resolves it). A journaled `unconfirmed` prompt effect
///   that an outbox entry of this Run links (`effect_id`) is unbarred once
///   that entry is `submitted`: transcript evidence resolved the follow-up
///   (F9/F17), and the outbox row is the source of truth for it while the
///   journal row keeps its wire fact. The set of such lifted effect ids is
///   built once per call, so the two unbounded histories are each scanned
///   once;
/// - a `planned` or `dispatching` prompt effect to the identity holds the
///   single slot — one prompt at a time;
/// - the head of the queue is the earliest entry still holding the
///   pipeline (`dispatching` holds the slot, `unconfirmed` bars it);
/// - F17's safe moment: never while `blocked` (H#17); a qualified
///   `mid_turn_input` sends immediately — even while `working` or before
///   the first observation — otherwise only when the child was last
///   reported `idle` or `done`.
///
/// `prompt_effects` is the run's effect-journal slice; only `prompt` rows
/// whose target is this Run's `Child` identity count — a hint effect
/// addresses the *owner's* pane, a different captured identity, and never
/// serializes behind the child's queue.
#[must_use]
pub fn next_dispatchable_follow_up<'a>(
    run: &Run,
    outbox: &'a [OutboxMessage],
    prompt_effects: &[Effect],
    qualified: &[Capability],
) -> Option<&'a OutboxMessage> {
    if run.settlement.is_some() {
        return None;
    }
    match run.state {
        State::Active | State::Judging | State::Repair => {}
        State::Reserved | State::Starting | State::Prompting | State::Settled => return None,
    }
    if barrier(run, outbox, prompt_effects) {
        return None;
    }
    let in_flight = prompt_effects
        .iter()
        .any(|effect| prompts_to_child(run, effect) && prompt_in_flight(effect.state));
    if in_flight {
        return None;
    }
    let head = outbox
        .iter()
        .filter(|m| m.run == run.id && m.state.holds_pipeline())
        .min_by_key(|m| m.seq)?;
    match head.state {
        OutboxState::Queued => {}
        OutboxState::Dispatching
        | OutboxState::Unconfirmed
        | OutboxState::Submitted
        | OutboxState::Expired => return None,
    }
    if run.child_status == Some(ChildStatus::Blocked) {
        return None;
    }
    if qualified
        .iter()
        .any(|cap| cap.as_str() == Capability::MID_TURN_INPUT)
    {
        return Some(head);
    }
    match run.child_status {
        Some(ChildStatus::Idle | ChildStatus::Done) => Some(head),
        Some(ChildStatus::Working | ChildStatus::Blocked) | None => None,
    }
}

/// F9 — the dispatch-commit revalidation: may this one journaled prompt to
/// the Run's `Child` identity dispatch now? The candidate must be present
/// in `journal` and `planned`. `journal` is the Run's effect slice in plan
/// order (`ORDER BY planned_at, effect_id` — `Effect` carries no
/// `planned_at`, so position is the order). Unlike
/// `next_dispatchable_follow_up`, which picks the queue's head, the
/// candidate is itself `planned`: it is excluded from its own slot check,
/// while ordering against its siblings is preserved — no *other* `planned`
/// child prompt may precede it and no other child prompt may be
/// `dispatching`.
///
/// The remaining gates are the shared ordering `barrier`, never while
/// `blocked` (F17/H#17), and the state lane: `prompt:task` dispatches only
/// in `prompting`, every other child prompt only in
/// `active`/`judging`/`repair`.
#[must_use]
pub fn prompt_dispatchable(
    run: &Run,
    outbox: &[OutboxMessage],
    journal: &[Effect],
    candidate: &EffectKey,
) -> bool {
    let Some((position, candidate_effect)) = journal
        .iter()
        .enumerate()
        .find(|(_, effect)| effect.key == *candidate)
    else {
        return false;
    };
    if candidate_effect.state != EffectState::Planned || !prompts_to_child(run, candidate_effect) {
        return false;
    }
    let task_prompt = candidate.0 == format!("run:{}:prompt:task", run.id.0);
    let state_allows = match run.state {
        State::Prompting => task_prompt,
        State::Active | State::Judging | State::Repair => !task_prompt,
        State::Reserved | State::Starting | State::Settled => false,
    };
    if !state_allows || barrier(run, outbox, journal) {
        return false;
    }
    if run.child_status == Some(ChildStatus::Blocked) {
        return false;
    }
    let earlier_planned = journal
        .iter()
        .take(position)
        .any(|e| prompts_to_child(run, e) && e.state == EffectState::Planned);
    let other_dispatching = journal.iter().any(|e| {
        e.key != *candidate && prompts_to_child(run, e) && e.state == EffectState::Dispatching
    });
    !(earlier_planned || other_dispatching)
}
