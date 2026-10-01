//! F13 step 1 — validating the answered launch set into the typed
//! `Evaluation`: the exact asked set, contract-shaped answers, and
//! `related_tab` parsing.

use alloc::vec::Vec;

use crate::config::Tier;
use crate::identity::TabId;
use crate::routing::{
    ChangesFiles, JudgmentOutcome, JudgmentPurpose, Probability, Question, TabChoice,
    validate_evaluation,
};
use crate::task::AbstainReason;

use super::builders::{answered_judgments, choice_judgment, launch_set, noul_judgment, policy};

#[test]
fn f13_validate_accepts_the_answered_set() {
    let record = launch_set(JudgmentOutcome::Answered, answered_judgments("t2"));
    let judged = match validate_evaluation(&record, &policy(), &[]) {
        Ok(evaluated) => evaluated,
        Err(reason) => panic!("expected a valid evaluation: {reason:?}"),
    };
    assert_eq!(judged.weakest_sufficient_tier, Tier("t2".into()));
    assert_eq!(judged.done_when_verifiable, Probability(0.9));
    assert_eq!(judged.changes_files, ChangesFiles::Few);
    assert_eq!(judged.related_tab, None);
}

#[test]
fn f13_validate_rejects_unanswered_and_wrong_purpose() {
    let record = launch_set(JudgmentOutcome::TransportFailed, Vec::new());
    assert_eq!(
        validate_evaluation(&record, &policy(), &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    let mut wrong_purpose = launch_set(JudgmentOutcome::Answered, answered_judgments("t2"));
    wrong_purpose.set.purpose = JudgmentPurpose::Review;
    assert_eq!(
        validate_evaluation(&wrong_purpose, &policy(), &[]),
        Err(AbstainReason::EvaluationFailed)
    );
}

#[test]
fn f13_validate_rejects_missing_duplicate_and_extra_rows() {
    let policy = policy();
    let missing = launch_set(
        JudgmentOutcome::Answered,
        answered_judgments("t2")[1..].to_vec(),
    );
    assert_eq!(
        validate_evaluation(&missing, &policy, &[]),
        Err(AbstainReason::EvaluationFailed),
        "a missing row is a malformed response"
    );
    let mut duplicated = answered_judgments("t2");
    duplicated.push(noul_judgment(Question::SecurityBoundary, 0.1));
    let duplicate = launch_set(JudgmentOutcome::Answered, duplicated);
    assert_eq!(
        validate_evaluation(&duplicate, &policy, &[]),
        Err(AbstainReason::EvaluationFailed),
        "a question answered twice is a malformed response"
    );
    let mut extra_rows = answered_judgments("t2");
    extra_rows.push(noul_judgment(Question::ProviderLimited, 0.1));
    let extra = launch_set(JudgmentOutcome::Answered, extra_rows);
    assert_eq!(
        validate_evaluation(&extra, &policy, &[]),
        Err(AbstainReason::EvaluationFailed),
        "a row for a question that was never asked is malformed"
    );
}

#[test]
fn f13_validate_rejects_malformed_answers() {
    let policy = policy();
    // Noul verdict inconsistent with its own P(yes).
    let mut inconsistent_rows = answered_judgments("t2");
    inconsistent_rows[0].answer = "no".into();
    let inconsistent = launch_set(JudgmentOutcome::Answered, inconsistent_rows);
    assert_eq!(
        validate_evaluation(&inconsistent, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // Noul without the P(yes) it must record.
    let mut missing_yes_rows = answered_judgments("t2");
    missing_yes_rows[0].probabilities.clear();
    let missing_yes = launch_set(JudgmentOutcome::Answered, missing_yes_rows);
    assert_eq!(
        validate_evaluation(&missing_yes, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // A probability outside [0,1].
    let mut out_of_range_rows = answered_judgments("t2");
    out_of_range_rows[0]
        .probabilities
        .insert("yes".into(), Probability(1.5));
    let out_of_range = launch_set(JudgmentOutcome::Answered, out_of_range_rows);
    assert_eq!(
        validate_evaluation(&out_of_range, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // A tier label outside the policy order.
    let mut bad_tier_rows = answered_judgments("t2");
    bad_tier_rows[1].answer = "enterprise".into();
    bad_tier_rows[1]
        .probabilities
        .insert("enterprise".into(), Probability(0.9));
    let bad_tier = launch_set(JudgmentOutcome::Answered, bad_tier_rows);
    assert_eq!(
        validate_evaluation(&bad_tier, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // A changes_files label outside the answer space.
    let mut bad_changes_rows = answered_judgments("t2");
    bad_changes_rows[2].answer = "everything".into();
    bad_changes_rows[2]
        .probabilities
        .insert("everything".into(), Probability(0.9));
    let bad_changes = launch_set(JudgmentOutcome::Answered, bad_changes_rows);
    assert_eq!(
        validate_evaluation(&bad_changes, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // A choice answer outside its own recorded distribution.
    let mut not_offered_rows = answered_judgments("t2");
    not_offered_rows[2].answer = "none".into();
    not_offered_rows[2].probabilities.remove("none");
    let not_offered = launch_set(JudgmentOutcome::Answered, not_offered_rows);
    assert_eq!(
        validate_evaluation(&not_offered, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
}

#[test]
fn f13_validate_accepts_each_changes_files_label() {
    let policy = policy();
    for (label, expected) in [
        ("none", ChangesFiles::None),
        ("few", ChangesFiles::Few),
        ("broad", ChangesFiles::Broad),
    ] {
        let mut rows = answered_judgments("t2");
        rows[2] = choice_judgment(Question::ChangesFiles, &["none", "few", "broad"], label);
        let record = launch_set(JudgmentOutcome::Answered, rows);
        let judged = match validate_evaluation(&record, &policy, &[]) {
            Ok(evaluated) => evaluated,
            Err(reason) => panic!("{label} must validate: {reason:?}"),
        };
        assert_eq!(judged.changes_files, expected);
    }
}

#[test]
fn f13_validate_related_tab_parses_and_gates_on_open_tabs() {
    let policy = policy();
    let tabs = [TabId("tab-1".into()), TabId("tab-2".into())];
    let mut with_tab = answered_judgments("t2");
    with_tab.push(choice_judgment(
        Question::RelatedTab,
        &["tab-1", "tab-2", "new"],
        "tab-2",
    ));
    let record = launch_set(JudgmentOutcome::Answered, with_tab);
    let judged = match validate_evaluation(&record, &policy, &tabs) {
        Ok(evaluated) => evaluated,
        Err(reason) => panic!("expected a valid evaluation: {reason:?}"),
    };
    assert_eq!(
        judged.related_tab,
        Some(TabChoice::Tab(TabId("tab-2".into())))
    );
    // Without open tabs the same set is malformed — the question was never asked.
    assert_eq!(
        validate_evaluation(&record, &policy, &[]),
        Err(AbstainReason::EvaluationFailed)
    );
    // With open tabs but no related_tab row, a required answer is missing.
    let missing = launch_set(JudgmentOutcome::Answered, answered_judgments("t2"));
    assert_eq!(
        validate_evaluation(&missing, &policy, &tabs),
        Err(AbstainReason::EvaluationFailed)
    );
    // `new` parses to the New choice.
    let mut with_new = answered_judgments("t2");
    with_new.push(choice_judgment(
        Question::RelatedTab,
        &["tab-1", "new"],
        "new",
    ));
    let new_record = launch_set(JudgmentOutcome::Answered, with_new);
    let judged_new = match validate_evaluation(&new_record, &policy, &tabs) {
        Ok(evaluated) => evaluated,
        Err(reason) => panic!("expected a valid evaluation: {reason:?}"),
    };
    assert_eq!(judged_new.related_tab, Some(TabChoice::New));
}
