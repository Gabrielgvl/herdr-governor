//! F12 — the evaluation contract: the asked question set, the request
//! bound, failure-to-abstention mapping, the interruption rule, and the
//! doneWhen rejection.

use alloc::vec::Vec;

use crate::identity::TabId;
use crate::lifecycle::{EffectCertainty, EffectOutcome};
use crate::routing::{
    ChangesFiles, JEV_REQUEST_MAX_BYTES, JudgmentOutcome, Probability, Question,
    evaluation_abstention, evaluation_effect_abstention, evaluation_questions, evaluation_verdict,
    request_size_outcome,
};
use crate::task::{AbstainReason, LaunchOutcome};

use super::builders::evaluation;

#[test]
fn f12_related_tab_asked_only_with_open_tabs() {
    let without_tabs = evaluation_questions(&[]);
    assert_eq!(
        without_tabs,
        Vec::from([
            Question::DoneWhenVerifiable,
            Question::WeakestSufficientTier,
            Question::ChangesFiles,
            Question::SecurityBoundary,
            Question::NeedsExternal,
            Question::LongRunning,
        ]),
        "no open governor tabs — related_tab is not asked"
    );
    let with_tabs = evaluation_questions(&[TabId("tab-1".into())]);
    assert_eq!(
        with_tabs.last(),
        Some(&Question::RelatedTab),
        "the caller has open tabs — related_tab joins the asked set"
    );
    assert_eq!(with_tabs.len(), 7);
}

#[test]
fn f12_oversize_request_abstains_before_send() {
    assert_eq!(
        request_size_outcome(JEV_REQUEST_MAX_BYTES),
        None,
        "at the bound the request is sent"
    );
    assert_eq!(
        request_size_outcome(JEV_REQUEST_MAX_BYTES.saturating_add(1)),
        Some(JudgmentOutcome::TooLarge),
        "over the bound the request is too_large — never sent"
    );
    assert_eq!(
        evaluation_abstention(JudgmentOutcome::TooLarge),
        Some(AbstainReason::EvaluationFailed),
        "the oversize outcome abstains evaluation_failed"
    );
}

#[test]
fn f12_failed_evaluation_abstains() {
    for outcome in [
        JudgmentOutcome::TransportFailed,
        JudgmentOutcome::AuthFailed,
        JudgmentOutcome::InvalidResponse,
        JudgmentOutcome::TooLarge,
        JudgmentOutcome::Stale,
    ] {
        assert_eq!(
            evaluation_abstention(outcome),
            Some(AbstainReason::EvaluationFailed),
            "{outcome:?} abstains evaluation_failed — transport, auth, HTTP, malformed and oversize all do"
        );
    }
    assert_eq!(
        evaluation_abstention(JudgmentOutcome::Answered),
        None,
        "an answered set proceeds to validation"
    );
}

#[test]
fn f12_interrupted_evaluation_abstains_before_decision() {
    assert_eq!(
        evaluation_effect_abstention(EffectOutcome::Unconfirmed),
        Some(AbstainReason::InterruptedBeforeDecision),
        "a dispatching evaluation interrupted by restart abstains interrupted_before_decision"
    );
    for outcome in [
        EffectOutcome::PreInteractiveFailed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent,
        },
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        assert_eq!(
            evaluation_effect_abstention(outcome),
            Some(AbstainReason::EvaluationFailed),
            "{outcome:?} is a failed evaluation, not an interruption"
        );
    }
    assert_eq!(
        evaluation_effect_abstention(EffectOutcome::Acknowledged),
        None,
        "acknowledged is not a failure"
    );
}

#[test]
fn f12_unverifiable_done_when_rejects() {
    let mut judged = evaluation("t1", ChangesFiles::Few, 0.1);
    judged.done_when_verifiable = Probability(0.4);
    assert_eq!(
        evaluation_verdict(&judged),
        Some(LaunchOutcome::Rejected),
        "doneWhen not verifiable — Jev answered, the answer rejects the Task"
    );
    judged.done_when_verifiable = Probability(0.5);
    assert_eq!(
        evaluation_verdict(&judged),
        None,
        "at the verdict bound the Task routes"
    );
}
