//! F9 — the per-target ordering the `effect_id` link journals: which queued
//! follow-up, if any, may dispatch to the Run's captured identity now. The
//! gates are the ordering barrier, the single prompt slot and F17's safe
//! moment.

use alloc::collections::BTreeSet;

use crate::config::Capability;
use crate::identity::{ChildStatus, EffectId};
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
    let prompts_to_child = |effect: &Effect| {
        effect.kind == EffectKind::Prompt
            && effect.subject_run.as_ref() == Some(&run.id)
            && match &effect.target {
                Some(EffectTarget::Child(_)) => true,
                Some(
                    EffectTarget::ExistingTab(_)
                    | EffectTarget::CallerContext(_)
                    | EffectTarget::AgentPane(_),
                )
                | None => false,
            }
    };
    let lifted: BTreeSet<&EffectId> = outbox
        .iter()
        .filter(|m| m.run == run.id && m.state == OutboxState::Submitted)
        .filter_map(|m| m.effect.as_ref())
        .collect();
    let barrier = run.prompt_certainty == Some(PromptCertainty::Unconfirmed)
        || prompt_effects.iter().any(|effect| {
            prompts_to_child(effect)
                && effect.state == EffectState::Unconfirmed
                && !lifted.contains(&effect.id)
        });
    if barrier {
        return None;
    }
    let in_flight = prompt_effects
        .iter()
        .any(|effect| prompts_to_child(effect) && prompt_in_flight(effect.state));
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
