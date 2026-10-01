//! F24 — the repair window's qualifying-dispatch predicate: only a
//! `dispatching` outbox prompt row inside `rejected_at..repair_deadline`
//! holds the settle or revives the run.

use alloc::vec::Vec;

use crate::identity::Timestamp;
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectState, Event, Settlement,
    State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, dispatched_outbox, effect_writes, journal_effect, run_in, run_result,
    settlement_of, stamped, test_policy, updated_records, updated_run,
};

#[test]
fn f24_repair_deadline_fires_despite_an_unrelated_dispatching_row() {
    // only a QUALIFYING in-window outbox dispatch holds the repair
    // deadline — a dispatching row that is not one must not count.
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let mut unrelated = journal_effect(
        &run,
        "nudge:0",
        EffectKind::Prompt,
        EffectState::Dispatching,
    );
    unrelated.dispatched_at = Some(Timestamp(300));
    let journal = Vec::from([
        unrelated,
        // dispatched before the rejection — outside the window
        dispatched_outbox(&run, 9, Timestamp(100)),
    ]);
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "neither a non-outbox nor an out-of-window dispatch holds the settle"
    );
}

#[test]
fn f24_repair_ignores_a_non_outbox_prompt_result() {
    // a prompt result that is not a `outbox:<seq>` follow-up is not the
    // repair dispatch — past the deadline it must not reach the rejected
    // settle.
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
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
        "a non-outbox prompt resolves as supervision only"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)])
    );
}

#[test]
fn f24_repair_ignores_a_non_prompt_outbox_result() {
    // an outbox-keyed result that is not a prompt is not the repair
    // follow-up either — the in-window row behind it must not revive the
    // run or fire the rejected settle.
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::JevEvaluate,
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
        "a non-prompt result never qualifies the dispatch"
    );
}

#[test]
fn f24_repair_settles_despite_a_non_dispatching_in_window_row() {
    // the "still in flight" scan counts only `dispatching` rows: a
    // resolved in-window row on another key is not pending, so the
    // provably-absent result past the deadline settles rejected.
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let mut resolved = dispatched_outbox(&run, 5, Timestamp(310));
    resolved.state = EffectState::Acknowledged;
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300)), resolved]);
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
        "a completed in-window row is not a dispatch in flight"
    );
}
