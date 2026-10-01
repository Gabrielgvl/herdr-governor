//! F22/F24 — the `handoff` lane and the judging entry the `obs` lane
//! shares: freeze a new digest (`evidence_generation+1`, the freeze row and
//! the assessment ask) or enter `judging` on one already frozen.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::{FrozenHandoff, judgment_deadline};
use crate::config::Policy;
use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    Effect, EffectKind, Run, State, StateChange, Transition, edited, effect_key, nothing,
    planned_effect, update_if_changed, write_run,
};

pub(super) fn on_handoff(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let frozen = handoffs
        .0
        .iter()
        .find(|h| h.work_generation == run.work_generation && h.digest == digest);
    match run.state {
        State::Active => {
            if frozen.is_some() {
                // already frozen for this generation — judging, no re-freeze.
                // Unreachable through persisted states: the freeze that
                // would write such a row moves the Run to `judging` in the
                // same transaction, and every return to `active` advances
                // `work_generation`. The arm stays as the defensive answer
                // and deliberately leaves `judging_digest` unset — a later
                // re-emitted handoff then re-asks rather than stranding.
                enter_judging(run, env)
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        // F24 — an unchanged digest is never re-judged *after a completed
        // assessment*: `assessed` rows stay suppressed. A frozen row whose
        // assessment never landed splits on `judging_digest` — equal means
        // its acceptance ask is still in flight and the repeat is a no-op
        // (re-asking would bump `evidence_generation` and stale the pending
        // answer, F20); different means the handoff was rewritten, so the
        // latest content wins and judging resumes on it (the
        // `handoff(new digest)` re-freeze rule). In `repair` no ask is in
        // flight — `judging_digest` is not consulted — so every unassessed
        // row resumes.
        State::Judging => match frozen {
            Some(handoff) if handoff.assessed => nothing(),
            Some(handoff) => {
                if run.judging_digest == Some(handoff.digest) {
                    nothing()
                } else {
                    resume_judging(run, digest, env)
                }
            }
            None => freeze_and_judge(run, digest, env, handoffs.1),
        },
        State::Repair => match frozen.map(|h| h.assessed) {
            Some(true) => nothing(),
            Some(false) => resume_judging(run, digest, env),
            None => freeze_and_judge(run, digest, env, handoffs.1),
        },
        State::Reserved | State::Starting | State::Prompting | State::Settled => nothing(),
    }
}

/// Move to `judging` when the handoff was already frozen (the `active`
/// arms of `handoff` and `obs(absent)`). The freeze transaction already
/// planned the assessment, so nothing is re-planned here;
/// `judgment_deadline` is armed defensively if unset and
/// `judging_digest` stays unset — under the persisted-state invariant the
/// callers are unreachable, and an unset digest lets a repeated handoff
/// re-ask instead of stranding the Run.
pub(super) fn enter_judging(run: &Run, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    update_if_changed(run, |next| {
        next.state = State::Judging;
        next.judgment_deadline = Some(judgment_deadline(
            next.judgment_deadline,
            now,
            policy.judgment_window,
        ));
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
    let (record, ask) = judging_write(run, digest, env);
    Transition {
        state_changes: Vec::from([
            StateChange::FreezeHandoff(FrozenHandoff {
                run: run.id.clone(),
                work_generation: run.work_generation,
                digest,
                frozen_path: String::from(freeze_path),
                frozen_at: env.0,
                assessed: false,
            }),
            write_run(run, record),
        ]),
        events: Vec::new(),
        effects: Vec::from([ask]),
    }
}

/// F24 — resume judging a frozen-but-unassessed handoff: the freeze row
/// stands (no duplicate), `evidence_generation+1` re-keys the assessment ask
/// so its answer passes the F20 stamp check, `judging_digest` names the
/// digest the new ask assesses, `judgment_deadline` is armed defensively if
/// unset and `repair_deadline` is never touched.
fn resume_judging(run: &Run, digest: Digest, env: (Timestamp, &Policy)) -> Transition {
    let (record, ask) = judging_write(run, digest, env);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::from([ask]),
    }
}

/// The write every judging entry shares — `judging` at
/// `evidence_generation+1` with `judgment_deadline` armed if unset and the
/// idle episode cleared — plus the ask that names that generation. The
/// ask's key carries no digest, so `judging_digest` records on the run row
/// which frozen handoff it assesses (Appendix B `runs.judging_digest`).
/// `evidence` events reuse it to re-plan the pending ask (F23/F20).
pub(super) fn judging_write(run: &Run, digest: Digest, env: (Timestamp, &Policy)) -> (Run, Effect) {
    let (now, policy) = env;
    let generation = run.evidence_generation.saturating_add(1);
    let record = edited(run, |next| {
        next.state = State::Judging;
        next.evidence_generation = generation;
        next.judging_digest = Some(digest);
        next.judgment_deadline = Some(judgment_deadline(
            next.judgment_deadline,
            now,
            policy.judgment_window,
        ));
        next.idle_since = None;
        next.idle_deadline = None;
    });
    let ask = planned_effect(
        run,
        EffectKind::JevEvaluate,
        effect_key(
            run,
            &format!("accept:{}:{}", run.work_generation, generation),
        ),
        None,
    );
    (record, ask)
}
