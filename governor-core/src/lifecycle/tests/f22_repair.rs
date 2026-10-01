//! F22 — the supervision lane of the total transition function: `repair`.

use alloc::vec::Vec;

use crate::identity::{Digest, Observation, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectState, Event, Settlement,
    State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, dispatched_outbox, effect_writes, frozen, is_quiet, run_in, run_result,
    settlement_of, stamped, test_policy, transact, updated_records, updated_run,
};
#[test]
pub(super) fn f22_repair_dispatch_before_deadline_advances_generation() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700));
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
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
        (None, &journal, &[]),
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
    assert_eq!(
        record.rejected_at, None,
        "the rejection stamp clears with it"
    );
}

// ---- F24 — the repair window binds the dispatch commit, not the result's
// arrival: the journal row's `dispatched_at` must land inside
// `[rejected_at, repair_deadline)`; a provably-absent failure never
// qualifies, and a deadline that fired while a qualifying dispatch was
// pending settles `rejected` when the non-qualifying result lands. ----

#[test]
fn f24_repair_result_arrival_time_is_not_the_window() {
    // dispatched inside the window, acknowledged after the deadline —
    // the dispatch is what counts (F24).
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // already past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
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
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.work_generation, 1,
        "an in-window dispatch counts however late its result lands"
    );
    assert_eq!(record.state, State::Active);
    assert_eq!(record.repair_deadline, None);
    assert_eq!(record.rejected_at, None);
}

#[test]
pub(super) fn f24_repair_deadline_waits_on_a_pending_dispatch() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    // the deadline fires while the qualifying dispatch is still pending —
    // the Run stays repair; the dispatch's result decides.
    let t_deadline = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_deadline),
        "a qualifying dispatch in flight holds the settle (F24)"
    );
    let t_ack = transition(
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
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t_ack);
    assert_eq!(record.state, State::Active);
    assert_eq!(record.work_generation, 1, "the in-window dispatch counts");
}

#[test]
fn f24_repair_pre_rejection_dispatch_never_qualifies() {
    // dispatched while the Run was still active — before the rejection —
    // and acknowledged inside the window: `dispatched_at < rejected_at`
    // is outside it.
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700)); // still ahead of NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(100))]);
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
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "a dispatch made before the rejection is not this generation's repair (F24)"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:outbox:3", EffectState::Acknowledged)]),
        "the late result still journals"
    );
}

#[test]
fn f24_repair_possibly_consumed_results_qualify() {
    // `unconfirmed` and `failed/unknown` are possibly consumed — they
    // qualify exactly like `acknowledged`; only a provably-absent failure
    // does not (F24).
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let mut run = run_in(State::Repair);
        run.evidence_generation = 1;
        run.rejected_at = Some(Timestamp(200));
        run.repair_deadline = Some(Timestamp(700));
        let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
        let t = transition(
            &run,
            &stamped(
                &run,
                run_result(&run, "outbox:3", EffectKind::Prompt, outcome, None),
            ),
            NOW,
            &test_policy(),
            (None, &journal, &[]),
            "/fp",
        );
        let record = updated_run(&t);
        assert_eq!(record.state, State::Active, "possibly consumed qualifies");
        assert_eq!(record.work_generation, 1);
    }
}

#[test]
pub(super) fn f24_repair_absent_result_past_deadline_settles_rejected() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    // the deadline fires while the in-window dispatch is still pending →
    // stays repair.
    let t_deadline = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(is_quiet(&t_deadline));
    // then its result lands `failed/absent` — the prompt provably never
    // ran; with nothing qualifying pending the Run settles rejected in
    // this transition.
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Failed {
                    certainty: EffectCertainty::Absent,
                },
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "a provably-absent repair past the deadline settles rejected (F24)"
    );
}

#[test]
fn f22_repair_dispatch_after_deadline_does_not_count() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // before NOW
    // the dispatch commit came after the deadline — it never qualified;
    // its late result, with nothing in-window pending, settles rejected.
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(450))]);
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
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "a post-deadline dispatch never qualifies — the late result settles"
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
    run.rejected_at = Some(Timestamp(200));
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
    run.rejected_at = Some(Timestamp(200));
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
    // a digest already judged stays repair — the frozen row carries a
    // completed assessment (F24); an unassessed one would resume judging.
    let mut assessed_row = frozen(&run, 0, 5);
    assessed_row.assessed = true;
    let handoffs = Vec::from([assessed_row]);
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
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(NOW);
    // committed exactly at the deadline — outside the half-open window;
    // the late result settles rejected with nothing qualifying pending.
    let journal = Vec::from([dispatched_outbox(&run, 3, NOW)]);
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
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "the follow-up must land strictly before the deadline"
    );
    // no armed rejection at all → there is no window; nothing qualifies.
    let mut run_unarmed = run_in(State::Repair);
    run_unarmed.evidence_generation = 1;
    let journal_unarmed = Vec::from([dispatched_outbox(&run_unarmed, 3, Timestamp(100))]);
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
        (None, &journal_unarmed, &[]),
        "/fp",
    );
    assert!(
        updated_records(&t_unarmed).is_empty(),
        "without rejected_at no dispatch can qualify as the repair follow-up"
    );
}
