//! F22/F24 — the `handoff` lane and the judging entry the `obs` lane
//! shares: freeze a new digest (`evidence_generation+1`, the freeze row and
//! the assessment ask) or enter `judging` on one already frozen.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::FrozenHandoff;
use crate::config::Policy;
use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    EffectKind, Run, State, StateChange, Transition, deadline_after, edited, effect_key, nothing,
    planned_effect, update_if_changed, write_run,
};

pub(super) fn on_handoff(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let frozen_for_generation = handoffs
        .0
        .iter()
        .any(|h| h.work_generation == run.work_generation && h.digest == digest);
    match run.state {
        State::Active => {
            if frozen_for_generation {
                // already frozen for this generation — judging, no re-freeze
                enter_judging(run, env)
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        // a digest not yet judged freezes and judges; one already frozen was
        // already judged (F24 — unchanged digests are never re-judged).
        State::Judging | State::Repair => {
            if frozen_for_generation {
                nothing()
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        State::Reserved | State::Starting | State::Prompting | State::Settled => nothing(),
    }
}

/// Move to `judging` when the handoff was already frozen (`obs(absent)` in
/// `active`, or a repeated `handoff` event for a known digest). The freeze
/// transaction already planned the assessment, so nothing is re-planned
/// here; `judgment_deadline` is armed defensively if unset.
pub(super) fn enter_judging(run: &Run, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    update_if_changed(run, |next| {
        next.state = State::Judging;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
    })
}

/// Freeze a new handoff digest and move to `judging` — the Appendix B freeze
/// transaction: the `handoffs` row, `evidence_generation+1`, and
/// `judgment_deadline` if not already set (it is never re-armed).
pub(super) fn freeze_and_judge(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    freeze_path: &str,
) -> Transition {
    let (now, policy) = env;
    let generation = run.evidence_generation.saturating_add(1);
    let record = edited(run, |next| {
        next.state = State::Judging;
        next.evidence_generation = generation;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
        next.idle_since = None;
        next.idle_deadline = None;
    });
    Transition {
        state_changes: Vec::from([
            StateChange::FreezeHandoff(FrozenHandoff {
                run: run.id.clone(),
                work_generation: run.work_generation,
                digest,
                frozen_path: String::from(freeze_path),
                frozen_at: now,
            }),
            write_run(run, record),
        ]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::JevEvaluate,
            effect_key(
                run,
                &format!("accept:{}:{}", run.work_generation, generation),
            ),
            None,
        )]),
    }
}
