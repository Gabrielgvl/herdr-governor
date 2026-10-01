//! F21 — the blocked-observation trigger: the provider_limited ask is
//! once per blocked episode, in every supervised state, and a child is
//! never prompted merely for being blocked.

use alloc::vec::Vec;

use crate::identity::ChildStatus;
use crate::lifecycle::{EffectKind, EffectReceipt, EffectState, State, transition};

use super::builders::{
    NOW, effect_keys, journal_effect, obs_unique, review_record, run_in, stamped, test_policy,
    transact, updated_run,
};

#[test]
pub(super) fn f21_blocked_episode_reasks_failure_and_reopens_on_work() {
    // F21/F23 — the ask is once per blocked EPISODE: a completed periodic
    // review does not suppress it (the key family is separate), a failed
    // attempt retries under `:<n>`, and a working observation ends the
    // episode so the next blocked report opens a fresh one.
    let run = run_in(State::Active);
    // a completed periodic review in this generation does not answer the
    // episode's provider_limited question (c2a).
    let mut reviewed = journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    reviewed.receipt = Some(EffectReceipt::Judgments(review_record(&run, Vec::new())));
    let journal = Vec::from([reviewed]);
    let t = transition(
        &run,
        &stamped(&run, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        effect_keys(&t),
        Vec::from(["run:r-1:blocked:1"]),
        "a blocked observation asks provider_limited once per episode"
    );

    // the same episode retries a failed ask under the next attempt key —
    // the unanswered row does not count as completed (F23).
    let mut run_blocked = run_in(State::Active);
    run_blocked.child_status = Some(ChildStatus::Blocked);
    run_blocked.blocked_episode = 1;
    let failed = journal_effect(
        &run_blocked,
        "blocked:1",
        EffectKind::JevEvaluate,
        EffectState::Failed,
    );
    let journal_failed = Vec::from([failed]);
    let t_retry = transition(
        &run_blocked,
        &stamped(&run_blocked, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal_failed, &[]),
        "/fp",
    );
    assert_eq!(
        effect_keys(&t_retry),
        Vec::from(["run:r-1:blocked:1:1"]),
        "an unanswered episode ask retries under :1"
    );

    // the episode ends when the child works again — a blocked report after
    // a non-blocked one opens the next episode and asks under its key.
    let mut run_working = run_in(State::Active);
    run_working.child_status = Some(ChildStatus::Working);
    run_working.blocked_episode = 1;
    let t_reopen = transition(
        &run_working,
        &stamped(&run_working, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal_failed, &[]),
        "/fp",
    );
    let record = updated_run(&t_reopen);
    assert_eq!(record.blocked_episode, 2, "work ended the episode");
    assert_eq!(
        effect_keys(&t_reopen),
        Vec::from(["run:r-1:blocked:2"]),
        "a new blocked episode asks again (F21)"
    );
}

#[test]
pub(super) fn f21_blocked_observation_asks_provider_limited_in_repair_and_judging() {
    // F21's trigger is Herdr reporting `blocked` — the ask fires in every
    // supervised state, not only `active` (c2b), once per blocked
    // episode. No prompt is ever planned for a blocked child (F17).
    for state in [State::Repair, State::Judging] {
        let mut run = run_in(state);
        run.evidence_generation = 1;
        let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Blocked))));
        assert_eq!(
            effect_keys(&t),
            Vec::from(["run:r-1:blocked:1"]),
            "a blocked report in {state:?} asks provider_limited"
        );
        assert!(
            t.effects.iter().all(|e| e.kind != EffectKind::Prompt),
            "a blocked child is never prompted"
        );
        assert_eq!(updated_run(&t).blocked_episode, 1);
    }
}
