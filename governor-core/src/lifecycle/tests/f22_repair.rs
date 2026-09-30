//! F22 — the supervision lane of the total transition function: `repair`.

use alloc::vec::Vec;

use crate::identity::{Digest, Observation, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectKind, EffectOutcome, EffectState, Event, Settlement, State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, effect_writes, frozen, is_quiet, run_in, run_result, settlement_of, stamped,
    test_policy, transact, updated_records, updated_run,
};
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
