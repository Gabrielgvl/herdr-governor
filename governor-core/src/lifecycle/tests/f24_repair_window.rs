//! F24 — the repair window's qualifying-dispatch predicate: only a
//! `dispatching` outbox prompt row inside `rejected_at..repair_deadline`
//! holds the settle or revives the run.

use alloc::vec::Vec;

use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectState, Event, JudgmentVerdict,
    Settlement, State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, dispatched_outbox, effect_keys, effect_writes, is_quiet, journal_effect,
    run_in, run_result, settlement_of, stamped, test_policy, updated_records, updated_run,
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

// ---- F24 — a judgment asked before an in-window repair dispatch resolves
// defers: the dispatch's pending result decides whether the verdict's work
// generation still stands. ----

#[test]
pub(super) fn f24_judgment_defers_while_a_qualifying_dispatch_is_in_flight() {
    // the accept verdict was asked before the repair follow-up dispatched;
    // while that dispatch is still `dispatching` inside
    // [rejected_at, repair_deadline) every verdict defers — a qualifying
    // resolution would stale the old generation's acceptance work.
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700));
    run.judging_digest = Some(Digest([8; 32]));
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    for verdict in [
        JudgmentVerdict::Accept,
        JudgmentVerdict::Reject,
        JudgmentVerdict::Unavailable,
    ] {
        let t = transition(
            &run,
            &stamped(&run, Event::Judgment(verdict)),
            NOW,
            &test_policy(),
            (None, &journal, &[]),
            "/fp",
        );
        assert!(
            is_quiet(&t),
            "a judgment defers while a qualifying repair dispatch is in flight (F24)"
        );
    }
    // a dispatching row outside the window holds nothing — the verdict
    // lands normally.
    let outside = Vec::from([dispatched_outbox(&run, 3, Timestamp(100))]);
    let t = transition(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
        NOW,
        &test_policy(),
        (None, &outside, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Accepted),
        "only a qualifying in-window dispatch defers the verdict"
    );
}

#[test]
pub(super) fn f24_qualifying_dispatch_resolution_stales_the_deferred_verdict() {
    // the verdict deferred in flight; the dispatch then acknowledges —
    // work_generation advances and the verdict's stamp answers a
    // generation that no longer holds (F20 drops it before dispatch).
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700));
    run.judging_digest = Some(Digest([8; 32]));
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let verdict = stamped(&run, Event::Judgment(JudgmentVerdict::Accept));
    let t_verdict = transition(
        &run,
        &verdict,
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_verdict),
        "the acceptance verdict defers while the dispatch is in flight"
    );
    let t_resolved = transition(
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
    let record = updated_run(&t_resolved);
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.work_generation, 1,
        "the qualifying dispatch opened a new work generation"
    );
    let t_stale = transition(
        record,
        &verdict,
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_stale),
        "the deferred verdict's stamp no longer holds (F20)"
    );
}

#[test]
pub(super) fn f24_failed_absent_dispatch_replans_the_acceptance_ask() {
    // the deferred verdict's ask cannot strand: the in-window dispatch
    // resolves provably-absent — the prompt never ran — while the deadline
    // is still ahead and nothing else qualifying is pending, so the
    // acceptance ask is re-planned for `judging_digest` at a fresh
    // evidence generation and its fresh verdict still settles.
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700)); // open at NOW
    run.judging_digest = Some(Digest([8; 32]));
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let t_verdict = transition(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_verdict),
        "the verdict defers while the dispatch is in flight"
    );
    let t_resolved = transition(
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
    let record = updated_run(&t_resolved);
    assert_eq!(
        record.state,
        State::Judging,
        "the open window keeps judging"
    );
    assert_eq!(
        record.evidence_generation, 2,
        "the re-planned ask re-keys the generation (F20)"
    );
    assert_eq!(
        record.judging_digest,
        Some(Digest([8; 32])),
        "the re-planned ask still assesses the digest under judgment"
    );
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(700)),
        "the repair window is untouched"
    );
    assert_eq!(
        effect_keys(&t_resolved),
        Vec::from(["run:r-1:accept:0:2"]),
        "the acceptance ask is re-planned under the new evidence generation"
    );
    // and a verdict answering the re-planned ask still settles accepted —
    // the journal row is resolved now, so nothing defers it.
    let mut resolved = dispatched_outbox(&run, 3, Timestamp(300));
    resolved.state = EffectState::Failed;
    let resolved_journal = Vec::from([resolved]);
    let t_accept = transition(
        record,
        &stamped(record, Event::Judgment(JudgmentVerdict::Accept)),
        NOW,
        &test_policy(),
        (None, &resolved_journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t_accept)),
        Some(Settlement::Accepted),
        "the re-planned ask's verdict settles"
    );
}
