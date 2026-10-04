//! F23 — when the periodic review (re)asks: only an answered set counts
//! as completed, and a changed evidence digest re-keys the ask family.

use alloc::string::String;
use alloc::vec::Vec;

use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    EffectKind, EffectReceipt, EffectState, Event, Settlement, State, acceptance_retry,
    periodic_review,
};
use crate::routing::{JudgmentOutcome, JudgmentPurpose};

use super::builders::{
    is_quiet, journal_effect, review_record, run_in, stamped, transact, updated_run,
};

#[test]
fn f23_unanswered_review_is_re_askable() {
    // F23 — only an answered set counts as a completed review: a failed,
    // transport-failed or still-unconfirmed attempt re-asks on the next
    // qualifying trigger, under a fresh attempt key.
    let run = run_in(State::Active);
    for state in [EffectState::Failed, EffectState::Unconfirmed] {
        let journal = Vec::from([journal_effect(
            &run,
            "review:0",
            EffectKind::JevEvaluate,
            state,
        )]);
        let retry = periodic_review(&run, false, &journal);
        assert_eq!(
            retry.map(|e| e.key.0),
            Some(String::from("run:r-1:review:0:1")),
            "an uncompleted review re-asks under the next attempt key"
        );
    }
    // an acknowledged transport failure the adapter could not answer is
    // re-askable too — the receipt's outcome is what counts, not the
    // journal state alone.
    let mut transport_failed = journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut record = review_record(&run, Vec::new());
    record.set.outcome = JudgmentOutcome::TransportFailed;
    transport_failed.receipt = Some(EffectReceipt::Judgments(record));
    let journal = Vec::from([transport_failed]);
    assert!(
        periodic_review(&run, false, &journal).is_some(),
        "a transport-failed review re-asks"
    );
    // and a stale-marked receipt re-asks.
    let mut stale_row = journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut stale_record = review_record(&run, Vec::new());
    stale_record.set.outcome = JudgmentOutcome::Stale;
    stale_row.receipt = Some(EffectReceipt::Judgments(stale_record));
    let journal_stale = Vec::from([stale_row]);
    assert!(
        periodic_review(&run, false, &journal_stale).is_some(),
        "a stale review re-asks"
    );
}

#[test]
pub(super) fn f23_evidence_change_rekeys_the_periodic_review() {
    // a changed transcript/git digest is recorded and bumps
    // `evidence_generation` — the generation-keyed review asks again.
    let mut run = run_in(State::Active);
    run.evidence_digest = Some(Digest([1; 32]));
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Evidence {
                digest: Digest([2; 32]),
            },
        ),
    );
    let record = updated_run(&t);
    assert_eq!(record.evidence_digest, Some(Digest([2; 32])));
    assert_eq!(
        record.evidence_generation, 1,
        "new evidence re-keys the ask family"
    );
    // a completed review under the old generation does not suppress this
    // generation's.
    let mut reviewed = journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    reviewed.receipt = Some(EffectReceipt::Judgments(review_record(&run, Vec::new())));
    let journal = Vec::from([reviewed]);
    let ask = periodic_review(record, false, &journal)
        .expect("the new evidence generation re-asks the review");
    assert_eq!(ask.key.0, "run:r-1:review:1");
}

#[test]
pub(super) fn f23_unchanged_evidence_digest_is_a_noop() {
    let mut run = run_in(State::Active);
    run.evidence_digest = Some(Digest([1; 32]));
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Evidence {
                digest: Digest([1; 32]),
            },
        ),
    );
    assert!(is_quiet(&t), "the same digest re-keys nothing");
    // and `settled` answers nothing at all.
    let mut settled = run_in(State::Settled);
    settled.settlement = Some(Settlement::Accepted);
    settled.settled_at = Some(Timestamp(1));
    let t_settled = transact(
        &settled,
        &stamped(
            &settled,
            Event::Evidence {
                digest: Digest([9; 32]),
            },
        ),
    );
    assert!(is_quiet(&t_settled), "evidence never reopens a settlement");
}

#[test]
fn f23_acceptance_retry_reasks_after_failed_attempt_only_while_judging() {
    // The `accept:<wg>:<gen>` family re-asks a failed, unconfirmed or
    // non-answered attempt under the next attempt key, only while judging.
    let mut run = run_in(State::Judging);
    run.work_generation = 0;
    run.evidence_generation = 1;
    // No attempt yet — the ask is planned under the base key.
    assert_eq!(
        acceptance_retry(&run, &[]).map(|e| e.key.0),
        Some(String::from("run:r-1:accept:0:1")),
        "a missing ask is planned"
    );
    for state in [EffectState::Failed, EffectState::Unconfirmed] {
        let journal = Vec::from([journal_effect(
            &run,
            "accept:0:1",
            EffectKind::JevEvaluate,
            state,
        )]);
        assert_eq!(
            acceptance_retry(&run, &journal).map(|e| e.key.0),
            Some(String::from("run:r-1:accept:0:1:1")),
            "a {state:?} attempt re-asks under the next attempt key"
        );
    }
    // An acknowledged attempt whose set did not answer re-asks too.
    let mut stale = journal_effect(
        &run,
        "accept:0:1",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut stale_record = review_record(&run, Vec::new());
    stale_record.set.purpose = JudgmentPurpose::Acceptance;
    stale_record.set.outcome = JudgmentOutcome::Stale;
    stale.receipt = Some(EffectReceipt::Judgments(stale_record));
    assert_eq!(
        acceptance_retry(&run, &Vec::from([stale])).map(|e| e.key.0),
        Some(String::from("run:r-1:accept:0:1:1")),
        "a stale acceptance set re-asks"
    );
    // An answered set that cannot verdict is journaled and re-asked — it
    // counts as an attempt, never a completion (F24/OQ-K). The run leaves
    // `judging` the moment a verdict lands, so an answered row while still
    // judging can only be a set `acceptance_verdict` refused.
    let mut answered = journal_effect(
        &run,
        "accept:0:1",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut answered_record = review_record(&run, Vec::new());
    answered_record.set.purpose = JudgmentPurpose::Acceptance;
    answered.receipt = Some(EffectReceipt::Judgments(answered_record));
    assert_eq!(
        acceptance_retry(&run, &Vec::from([answered])).map(|e| e.key.0),
        Some(String::from("run:r-1:accept:0:1:1")),
        "an answered acceptance set that cannot verdict re-asks (OQ-K)"
    );
    // An in-flight attempt suppresses it.
    for state in [EffectState::Planned, EffectState::Dispatching] {
        let journal = Vec::from([journal_effect(
            &run,
            "accept:0:1",
            EffectKind::JevEvaluate,
            state,
        )]);
        assert_eq!(
            acceptance_retry(&run, &journal),
            None,
            "an in-flight acceptance ask is never doubled ({state:?})"
        );
    }
    // Only `judging` retries.
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Repair,
        State::Settled,
    ] {
        let other = run_in(state);
        assert_eq!(
            acceptance_retry(&other, &[]),
            None,
            "acceptance retries only while judging, not {state:?}"
        );
    }
}

#[test]
fn f23_acceptance_retry_stops_after_too_large() {
    // A `too_large` attempt is terminal for the family: the frozen request
    // cannot shrink, so the wait falls to `judgment_deadline` (OQ-I/F30).
    let mut run = run_in(State::Judging);
    run.work_generation = 0;
    run.evidence_generation = 1;
    let mut too_large = journal_effect(
        &run,
        "accept:0:1",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut record = review_record(&run, Vec::new());
    record.set.purpose = JudgmentPurpose::Acceptance;
    record.set.outcome = JudgmentOutcome::TooLarge;
    too_large.receipt = Some(EffectReceipt::Judgments(record));
    assert_eq!(
        acceptance_retry(&run, &Vec::from([too_large])),
        None,
        "a too_large attempt ends the family's re-asks (OQ-I)"
    );
    // …under any attempt key in the family.
    let mut too_large_retry = journal_effect(
        &run,
        "accept:0:1:2",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut retry_record = review_record(&run, Vec::new());
    retry_record.set.purpose = JudgmentPurpose::Acceptance;
    retry_record.set.outcome = JudgmentOutcome::TooLarge;
    too_large_retry.receipt = Some(EffectReceipt::Judgments(retry_record));
    assert_eq!(
        acceptance_retry(&run, &Vec::from([too_large_retry])),
        None,
        "a too_large at a later attempt key is still terminal"
    );
    // A `too_large` row in a *different* generation's family does not stop
    // this one — `accept:0:2` never matches the `accept:0:1:` prefix.
    let mut other_family = journal_effect(
        &run,
        "accept:0:2",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    let mut other_record = review_record(&run, Vec::new());
    other_record.set.purpose = JudgmentPurpose::Acceptance;
    other_record.set.outcome = JudgmentOutcome::TooLarge;
    other_family.receipt = Some(EffectReceipt::Judgments(other_record));
    assert_eq!(
        acceptance_retry(&run, &Vec::from([other_family])).map(|e| e.key.0),
        Some(String::from("run:r-1:accept:0:1")),
        "another family's too_large is not this family's"
    );
}
