//! F22 — the supervision lane of the total transition function:
//! `active`, `judging` and `repair`.

use alloc::vec::Vec;

use crate::acceptance::HandoffReading;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, Digest, Observation, PaneId, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectKind, EffectOutcome, EffectState, Event, JudgmentVerdict, Settlement,
    State, UnresolvedReason, transition,
};

use super::builders::{
    EMPTY_READ, NOW, effect_keys, effect_writes, event_dedups, event_kinds, frozen, frozen_writes,
    is_quiet, journal_effect, obs_unique, run_in, run_result, settlement_of, stamped, test_policy,
    transact, updated_records, updated_run,
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
        Vec::from(["run:r-1:review:0"]),
        "a blocked child is asked blocked_on_input and provider_limited"
    );
    assert_eq!(t.effects[0].kind, EffectKind::JevEvaluate);
    // the same evidence is never re-asked.
    let journal = Vec::from([journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    )]);
    let t_repeat = transition(
        &run,
        &stamped(&run, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        t_repeat.effects.is_empty(),
        "unchanged evidence is never re-asked"
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
pub(super) fn f22_judging_accept_settles_accepted() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
    );
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::Accepted));
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffAccepted, MailboxEventKind::Settled])
    );
}

#[test]
pub(super) fn f22_judging_reject_enters_repair_and_arms_deadline_once() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Reject)),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Repair);
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(900_500)),
        "the repair window arms on the first rejection"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffRejected])
    );
    assert_eq!(
        event_dedups(&t),
        Vec::from(["run:r-1:handoff_rejected:0:1"]),
        "the verdict event is scoped to the rejected binding"
    );

    // a second rejection in the same work generation never extends it (F24).
    let mut again = run_in(State::Judging);
    again.evidence_generation = 2;
    again.repair_deadline = Some(Timestamp(600));
    let t_again = transition(
        &again,
        &stamped(&again, Event::Judgment(JudgmentVerdict::Reject)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_again).repair_deadline,
        Some(Timestamp(600)),
        "an armed repair_deadline is preserved, never reset"
    );
}

#[test]
pub(super) fn f22_judging_unavailable_stays_until_deadline() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Unavailable)),
    );
    assert!(is_quiet(&t), "an armed deadline already bounds the wait");
    // with no deadline armed the transition arms it.
    let mut unarmed = run_in(State::Judging);
    unarmed.evidence_generation = 1;
    let t_unarmed = transition(
        &unarmed,
        &stamped(&unarmed, Event::Judgment(JudgmentVerdict::Unavailable)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_unarmed).judgment_deadline,
        Some(Timestamp(1_800_500))
    );
}

#[test]
pub(super) fn f22_judging_deadlines() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable
        }),
        "the bound expires → unresolved(judgment_unavailable)"
    );
    // a not-yet-passed judgment deadline is a no-op.
    run.judgment_deadline = Some(Timestamp(600));
    let t_early = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t_early));
    // the repair deadline keeps running through judging (F24).
    run.judgment_deadline = Some(Timestamp(600));
    run.repair_deadline = Some(Timestamp(400));
    let t_repair = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t_repair)),
        Some(Settlement::Rejected),
        "a re-frozen handoff does not extend the repair deadline"
    );
}

#[test]
pub(super) fn f22_judging_absent_stays_judging() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert!(is_quiet(&t), "judging waits on the judgment, not the pane");
}

#[test]
pub(super) fn f22_judging_new_digest_refreezes_same_digest_is_ignored() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    // a digest already frozen for this generation is never re-judged (F24).
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
    assert!(is_quiet(&t), "the known digest is not re-frozen");
    // a rewritten handoff is new evidence → freeze again, stay judging.
    let t_rewrite = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([10; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t_rewrite);
    assert_eq!(record.state, State::Judging);
    assert_eq!(record.evidence_generation, 2);
    assert_eq!(frozen_writes(&t_rewrite).len(), 1);
}

#[test]
pub(super) fn f22_repair_dispatch_before_deadline_advances_generation() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.work_generation, 1,
        "a dispatched repair opens a new work generation"
    );
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.repair_deadline, None,
        "the window resets for the new generation"
    );
}

#[test]
fn f22_repair_dispatch_after_deadline_does_not_count() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(400)); // before NOW
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "a late dispatch does not reopen the generation"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:outbox:3", EffectState::Acknowledged)]),
        "the journal still records what happened"
    );
}

#[test]
pub(super) fn f22_repair_deadline_settles_rejected() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "the binding expires → rejected (F24)"
    );
    // not yet passed → nothing.
    run.repair_deadline = Some(Timestamp(900));
    let t_early = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t_early));
}

#[test]
pub(super) fn f22_repair_new_handoff_freezes_keeping_deadline() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([5; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(700)),
        "the repair deadline survives a re-freeze"
    );
    // a digest already judged (it is frozen for this generation) stays repair.
    let handoffs = Vec::from([frozen(&run, 0, 5)]);
    let t_repeat = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([5; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert!(
        is_quiet(&t_repeat),
        "the rejected digest never re-enters judgment"
    );
}

#[test]
pub(super) fn f22_repair_absent_stays_repair() {
    let run = run_in(State::Repair);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert!(is_quiet(&t), "repair waits out its deadline");
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
    // a repeated identical blocked observation is a true no-op — the review
    // exists and the row is unchanged.
    let mut run_repeat = run_in(State::Active);
    run_repeat.child_status = Some(ChildStatus::Blocked);
    run_repeat
        .identity
        .as_mut()
        .expect("active has identity")
        .pane_id = PaneId("w0:p9".into());
    let journal = Vec::from([journal_effect(
        &run_repeat,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    )]);
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
fn f22_repair_dispatch_at_the_deadline_does_not_count() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(NOW);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "the follow-up must land strictly before the deadline"
    );
    // no armed deadline at all → the dispatch counts.
    let mut run_unarmed = run_in(State::Repair);
    run_unarmed.evidence_generation = 1;
    let t_unarmed = transition(
        &run_unarmed,
        &stamped(
            &run_unarmed,
            run_result(
                &run_unarmed,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(updated_run(&t_unarmed).work_generation, 1);
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
