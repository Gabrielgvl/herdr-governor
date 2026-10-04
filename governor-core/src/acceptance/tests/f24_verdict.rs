//! F24 — `acceptance_verdict`: an answered `acceptance` judgment set yields
//! a verdict only when its `handoff_meets_item_k` answers cover exactly the
//! Task's doneWhen items, each once. Anything partial or malformed is not a
//! verdict — the coordinator journals the set and re-asks.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::acceptance_verdict;
use crate::config::ConfigVersion;
use crate::identity::{Digest, JudgmentSetId, RunId};
use crate::lifecycle::{JudgmentVerdict, VersionTriple};
use crate::routing::{
    Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability, Question,
    QuestionVersion,
};

/// An `acceptance` judgment set over `run-0199abcd` with `judgments` —
/// the receipt an answered `accept:<wg>:<gen>` ask carries.
fn record(
    purpose: JudgmentPurpose,
    outcome: JudgmentOutcome,
    judgments: Vec<Judgment>,
) -> JudgmentRecord {
    JudgmentRecord {
        set: JudgmentSet {
            id: JudgmentSetId("set-1".into()),
            purpose,
            launch: None,
            run: Some(RunId("run-0199abcd".into())),
            versions: Some(VersionTriple {
                version: 4,
                work_generation: 0,
                evidence_generation: 1,
            }),
            task_digest: Digest([1; 32]),
            handoff_digest: Some(Digest([2; 32])),
            evidence_digest: Some(Digest([3; 32])),
            model: "jev-1".into(),
            question_version: QuestionVersion("qv-1".into()),
            policy_version: ConfigVersion("pv-1".into()),
            outcome,
        },
        judgments,
    }
}

/// A `handoff_meets_item_<item>` noul answer calibrated at `yes`.
fn meets(item: u8, yes: f64) -> Judgment {
    Judgment {
        question: Question::HandoffMeetsItem { item },
        probabilities: BTreeMap::from([(String::from("yes"), Probability(yes))]),
        answer: String::from(if yes >= 0.5 { "yes" } else { "no" }),
        threshold: None,
    }
}

#[test]
fn f24_acceptance_verdict_rejects_one_item_prefix_of_larger_task() {
    // Answering only item 0 of a three-item Task is not a verdict — the
    // set is journaled and the family re-asks (F24/OQ-K).
    let prefix = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9)]),
    );
    assert_eq!(
        acceptance_verdict(&prefix, 3),
        None,
        "a one-item prefix of a three-item Task yields no verdict"
    );
    // A gap mid-set is just as incomplete.
    let gapped = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9), meets(2, 0.9)]),
    );
    assert_eq!(
        acceptance_verdict(&gapped, 3),
        None,
        "a missing item yields no verdict"
    );
    // An item the Task does not have is malformed, not a verdict.
    let over = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9), meets(3, 0.9)]),
    );
    assert_eq!(
        acceptance_verdict(&over, 1),
        None,
        "an out-of-range item yields no verdict"
    );
    // The same item answered twice is malformed, not a verdict.
    let duplicate = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9), meets(0, 0.2)]),
    );
    assert_eq!(
        acceptance_verdict(&duplicate, 1),
        None,
        "a duplicated item yields no verdict"
    );
    // The acceptance ask renders only item questions; any other judgment
    // makes the set malformed.
    let mut extra = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9)]),
    );
    extra.judgments.push(Judgment {
        question: Question::ProviderLimited,
        probabilities: BTreeMap::from([(String::from("yes"), Probability(0.9))]),
        answer: String::from("yes"),
        threshold: Some(0.7),
    });
    assert_eq!(
        acceptance_verdict(&extra, 1),
        None,
        "a judgment outside the asked set yields no verdict"
    );
}

#[test]
fn f24_acceptance_verdict_accepts_complete_one_item_task() {
    let complete = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(0, 0.9)]),
    );
    assert_eq!(
        acceptance_verdict(&complete, 1),
        Some(JudgmentVerdict::Accept),
        "a complete one-item acceptance set verdicts accept"
    );
    // Answer order carries no meaning; a calibrated "no" is an unmet item.
    let unordered = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([meets(2, 0.8), meets(0, 0.8), meets(1, 0.3)]),
    );
    assert_eq!(
        acceptance_verdict(&unordered, 3),
        Some(JudgmentVerdict::Reject),
        "any unmet item rejects into repair"
    );
    // A policy threshold recorded on the item shifts the noul's bound:
    // P(yes)=0.6 under 0.7 resolves "no" — in contract, and unmet.
    let mut thresholded = meets(0, 0.9);
    thresholded
        .probabilities
        .insert(String::from("yes"), Probability(0.6));
    thresholded.answer = String::from("no");
    thresholded.threshold = Some(0.7);
    let under = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([thresholded]),
    );
    assert_eq!(
        acceptance_verdict(&under, 1),
        Some(JudgmentVerdict::Reject),
        "P(yes) below the judgment's recorded threshold reads unmet"
    );
    // A distribution with no "yes" is malformed, not unmet — routing's
    // `noul_at` refuses the shape and OQ-K re-asks the set.
    let mut no_yes = meets(0, 0.9);
    no_yes.probabilities.clear();
    let unmet = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([no_yes]),
    );
    assert_eq!(
        acceptance_verdict(&unmet, 1),
        None,
        "an answered item without P(yes) is malformed, not a verdict"
    );
    // An `answer` contradicting P(yes) is malformed for the same reason —
    // P(yes)=0.9 resolving "no" is acted on by neither accept nor reject.
    let mut contradictory = meets(0, 0.9);
    contradictory.answer = String::from("no");
    let lied = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([contradictory]),
    );
    assert_eq!(
        acceptance_verdict(&lied, 1),
        None,
        "an answer contradicting P(yes) is malformed, not a verdict"
    );
    // A probability outside [0,1] is out of contract too.
    let mut wild = meets(0, 0.9);
    wild.probabilities
        .insert(String::from("yes"), Probability(1.5));
    let invalid = record(
        JudgmentPurpose::Acceptance,
        JudgmentOutcome::Answered,
        Vec::from([wild]),
    );
    assert_eq!(
        acceptance_verdict(&invalid, 1),
        None,
        "a probability outside [0,1] is malformed, not a verdict"
    );
}

#[test]
fn f24_acceptance_verdict_requires_answered_acceptance_purpose() {
    let answered = |purpose| {
        record(
            purpose,
            JudgmentOutcome::Answered,
            Vec::from([meets(0, 0.9)]),
        )
    };
    for purpose in [
        JudgmentPurpose::Launch,
        JudgmentPurpose::Review,
        JudgmentPurpose::ProviderLimit,
    ] {
        assert_eq!(
            acceptance_verdict(&answered(purpose), 1),
            None,
            "a {purpose:?} set is never an acceptance verdict"
        );
    }
    for outcome in [
        JudgmentOutcome::TransportFailed,
        JudgmentOutcome::AuthFailed,
        JudgmentOutcome::InvalidResponse,
        JudgmentOutcome::TooLarge,
        JudgmentOutcome::Stale,
    ] {
        assert_eq!(
            acceptance_verdict(
                &record(
                    JudgmentPurpose::Acceptance,
                    outcome,
                    Vec::from([meets(0, 0.9)])
                ),
                1
            ),
            None,
            "a {outcome:?} acceptance set is never a verdict"
        );
    }
}
