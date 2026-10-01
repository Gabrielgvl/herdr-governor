//! F22 — the supervision lane of the total transition function: `active`.

use alloc::vec::Vec;

use crate::acceptance::HandoffReading;
use crate::identity::{ChildStatus, Digest, Observation, PaneId, Timestamp};
use crate::lifecycle::{
    EffectKind, EffectReceipt, EffectState, Event, Settlement, State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, effect_keys, frozen, frozen_writes, is_quiet, journal_effect, obs_unique,
    review_record, run_in, settlement_of, stamped, test_policy, transact, updated_run,
};
#[test]
pub(super) fn f22_active_working_ends_the_episode() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    run.nudged_episode = Some(0);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    let record = updated_run(&t);
    assert_eq!(record.child_status, Some(ChildStatus::Working));
    assert_eq!(record.idle_since, None, "the episode closes");
    assert_eq!(record.idle_deadline, None);
    assert_eq!(
        record.nudge_episode, 1,
        "the next stall opens a new episode"
    );
    // and the identity's locator follows the pane the observation read.
    assert_eq!(
        record.identity.as_ref().map(|i| i.pane_id.clone()),
        Some(PaneId("w0:p9".into()))
    );
}

#[test]
pub(super) fn f22_active_working_without_episode_only_records_status() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    let record = updated_run(&t);
    assert_eq!(
        record.nudge_episode, 0,
        "no episode was open — nothing bumps"
    );
    assert_eq!(record.child_status, Some(ChildStatus::Working));
    // an identical repeated observation is a true no-op: no version bump, so
    // in-flight stamped results stay valid (F20).
    let repeat = transact(
        record,
        &stamped(record, obs_unique(Some(ChildStatus::Working))),
    );
    assert!(is_quiet(&repeat), "an unchanged observation writes nothing");
}

#[test]
pub(super) fn f22_active_blocked_asks_the_supervision_questions() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Blocked))));
    assert_eq!(
        effect_keys(&t),
        Vec::from(["run:r-1:blocked:1"]),
        "a blocked child is asked blocked_on_input and provider_limited, once per blocked episode"
    );
    assert_eq!(t.effects[0].kind, EffectKind::JevEvaluate);
    assert_eq!(
        updated_run(&t).blocked_episode,
        1,
        "the blocked observation opened episode 1"
    );
    // the same episode is never re-asked — an in-flight ask suppresses.
    let journal = Vec::from([journal_effect(
        &run,
        "blocked:1",
        EffectKind::JevEvaluate,
        EffectState::Dispatching,
    )]);
    let mut run_blocked = run_in(State::Active);
    run_blocked.child_status = Some(ChildStatus::Blocked);
    run_blocked.blocked_episode = 1;
    let t_repeat = transition(
        &run_blocked,
        &stamped(&run_blocked, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        t_repeat.effects.is_empty(),
        "the episode's in-flight ask is never duplicated"
    );
}

#[test]
pub(super) fn f22_active_handoff_freezes_and_judges() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/state/handoffs/frozen-1",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    assert_eq!(
        record.evidence_generation, 1,
        "the freeze bumps evidence_generation"
    );
    assert_eq!(
        record.judgment_deadline,
        Some(Timestamp(1_800_500)),
        "judgment_deadline arms at the freeze"
    );
    assert_eq!(
        record.idle_since, None,
        "the handoff moots the idle episode"
    );
    let handoffs = frozen_writes(&t);
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].digest, Digest([9; 32]));
    assert_eq!(handoffs[0].work_generation, 0);
    assert_eq!(handoffs[0].frozen_path, "/state/handoffs/frozen-1");
    assert_eq!(handoffs[0].frozen_at, NOW);
    assert_eq!(
        effect_keys(&t),
        Vec::from(["run:r-1:accept:0:1"]),
        "the acceptance assessment is planned for the new binding"
    );
    assert_eq!(
        record.judging_digest,
        Some(Digest([9; 32])),
        "the ask's digest is recorded on the run row"
    );
}

#[test]
pub(super) fn f24_handoff_digest_frozen_in_an_old_generation_refreezes() {
    // the already-frozen arm of `active` is unreachable through persisted
    // states — a freeze row at the current work generation is written by
    // the same transaction that moves the Run to `judging`, and every
    // return to `active` advances `work_generation`. The reachable case:
    // a digest frozen only at an older generation re-freezes and re-asks
    // at the current one (F24 — the binding includes the generation).
    let mut run = run_in(State::Active);
    run.work_generation = 1;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    let writes = frozen_writes(&t);
    assert_eq!(
        writes.len(),
        1,
        "the same digest re-freezes at the new generation"
    );
    assert_eq!(writes[0].work_generation, 1);
    assert_eq!(record.judging_digest, Some(Digest([9; 32])));
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:accept:1:1"]));
}

#[test]
pub(super) fn f22_active_absent_reads_the_handoff_once() {
    let run = run_in(State::Active);
    // a valid marked file on the one-shot read freezes and judges (F25).
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: Some(HandoffReading::Valid {
                    digest: Digest([4; 32]),
                }),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(updated_run(&t).state, State::Judging);
    assert_eq!(frozen_writes(&t).len(), 1);
}

#[test]
pub(super) fn f22_active_absent_without_handoff_is_pane_lost() {
    let run = run_in(State::Active);
    for reading in [None, Some(HandoffReading::NotWritten)] {
        let t = transition(
            &run,
            &stamped(
                &run,
                Event::Obs {
                    observation: Observation::Absent,
                    handoff_reading: reading,
                },
            ),
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert_eq!(
            settlement_of(updated_run(&t)),
            Some(Settlement::PaneLost),
            "no frozen handoff and no valid reading → pane_lost"
        );
    }
}

#[test]
pub(super) fn f22_active_absent_with_frozen_handoff_judges() {
    let run = run_in(State::Active);
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.state,
        State::Judging,
        "an unjudged frozen handoff still goes to judgment"
    );
    assert!(
        frozen_writes(&t).is_empty(),
        "the handoff is already frozen — no second freeze row"
    );
    assert!(
        t.effects.is_empty(),
        "the assessment was planned at freeze time"
    );
}

#[test]
pub(super) fn f22_working_after_a_consumed_stall_nudge_ends_the_episode() {
    // the episode's nudge was spent on a stall while the child kept working —
    // no idle episode is open, yet the episode still ends on work.
    let mut run = run_in(State::Active);
    run.nudged_episode = Some(0);
    run.idle_since = None;
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    assert_eq!(
        updated_run(&t).nudge_episode,
        1,
        "either arm of the episode-open check closes the episode"
    );
}

#[test]
pub(super) fn f22_blocked_observation_records_status_and_writes_once() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Blocked))));
    assert_eq!(
        updated_run(&t).child_status,
        Some(ChildStatus::Blocked),
        "a status change writes the run row"
    );
    // a repeated identical blocked observation inside one episode is a true
    // no-op — the episode's answered ask exists and the row is unchanged.
    let mut run_repeat = run_in(State::Active);
    run_repeat.child_status = Some(ChildStatus::Blocked);
    run_repeat.blocked_episode = 1;
    run_repeat
        .identity
        .as_mut()
        .expect("active has identity")
        .pane_id = PaneId("w0:p9".into());
    let mut answered = journal_effect(
        &run_repeat,
        "blocked:1",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    answered.receipt = Some(EffectReceipt::Judgments(review_record(
        &run_repeat,
        Vec::new(),
    )));
    let journal = Vec::from([answered]);
    let t_repeat = transition(
        &run_repeat,
        &stamped(&run_repeat, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_repeat),
        "nothing changed, nothing to ask — no write"
    );
}

#[test]
pub(super) fn f22_active_absent_with_frozen_handoff_arms_deadline() {
    let mut run = run_in(State::Active);
    run.judgment_deadline = None;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert_eq!(
        updated_run(&t).judgment_deadline,
        Some(Timestamp(1_800_500)),
        "a missing judgment deadline is armed on entering judging"
    );
    // and an already-armed one is preserved.
    let mut run_armed = run_in(State::Active);
    run_armed.judgment_deadline = Some(Timestamp(700));
    let t_armed = transition(
        &run_armed,
        &stamped(
            &run_armed,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert_eq!(
        updated_run(&t_armed).judgment_deadline,
        Some(Timestamp(700))
    );
}

#[test]
pub(super) fn f22_refreeze_preserves_an_armed_judgment_deadline() {
    let mut run = run_in(State::Active);
    run.judgment_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t).judgment_deadline,
        Some(Timestamp(700)),
        "judgment_deadline is set on the first freeze only"
    );
}
