//! F23 — when the periodic review (re)asks: only an answered set counts
//! as completed, and a changed evidence digest re-keys the ask family.

use alloc::string::String;
use alloc::vec::Vec;

use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    EffectKind, EffectReceipt, EffectState, Event, Settlement, State, periodic_review,
};
use crate::routing::JudgmentOutcome;

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
