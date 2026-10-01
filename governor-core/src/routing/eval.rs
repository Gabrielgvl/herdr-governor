//! F12 — the evaluation half of routing: what the launch asks Jev, how big
//! the request may be, how failure and interruption become abstentions, how
//! the answered judgment set is validated into the typed `Evaluation` the
//! routing function reads, and when the answer rejects the Task outright.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::identity::TabId;
use crate::lifecycle::EffectOutcome;
use crate::task::{AbstainReason, LaunchOutcome};

use super::{
    ChangesFiles, Evaluation, JEV_REQUEST_MAX_BYTES, Judgment, JudgmentOutcome, JudgmentPurpose,
    JudgmentRecord, Probability, Question, TabChoice, noul_yes, valid_probability,
};

/// F12 — the launch-evaluation question set, in spec order: the six
/// questions always asked, plus `related_tab` — a choice over the caller's
/// open governor tabs plus `new` — asked only when such tabs exist.
#[must_use]
pub fn evaluation_questions(open_tabs: &[TabId]) -> Vec<Question> {
    let mut questions = Vec::from([
        Question::DoneWhenVerifiable,
        Question::WeakestSufficientTier,
        Question::ChangesFiles,
        Question::SecurityBoundary,
        Question::NeedsExternal,
        Question::LongRunning,
    ]);
    if !open_tabs.is_empty() {
        questions.push(Question::RelatedTab);
    }
    questions
}

/// N5/F12 — how a serialized Jev request resolves before it is ever sent:
/// over `JEV_REQUEST_MAX_BYTES` it is `too_large` and never leaves (the
/// abstention is `evaluation_abstention`'s).
#[must_use]
pub fn request_size_outcome(request_bytes: usize) -> Option<JudgmentOutcome> {
    if request_bytes > JEV_REQUEST_MAX_BYTES {
        Some(JudgmentOutcome::TooLarge)
    } else {
        None
    }
}

/// F12 — a launch evaluation that did not resolve `answered` becomes an
/// abstention, never a retry: transport, auth, HTTP, malformed and oversize
/// outcomes all abstain `evaluation_failed`; so does a `stale` answer — the
/// versions it was judged against no longer hold (F20), so it cannot route.
#[must_use]
pub fn evaluation_abstention(outcome: JudgmentOutcome) -> Option<AbstainReason> {
    match outcome {
        JudgmentOutcome::Answered => None,
        JudgmentOutcome::TransportFailed
        | JudgmentOutcome::AuthFailed
        | JudgmentOutcome::InvalidResponse
        | JudgmentOutcome::TooLarge
        | JudgmentOutcome::Stale => Some(AbstainReason::EvaluationFailed),
    }
}

/// F12/F28 — the same rule at the effect boundary: an evaluation effect that
/// failed before a usable response abstains `evaluation_failed`; one left
/// `dispatching` across a restart resolves `unconfirmed` (F8) and abstains
/// `interrupted_before_decision` — it is never evaluated twice.
#[must_use]
pub fn evaluation_effect_abstention(outcome: EffectOutcome) -> Option<AbstainReason> {
    match outcome {
        EffectOutcome::Acknowledged => None,
        EffectOutcome::PreInteractiveFailed | EffectOutcome::Failed { certainty: _ } => {
            Some(AbstainReason::EvaluationFailed)
        }
        EffectOutcome::Unconfirmed => Some(AbstainReason::InterruptedBeforeDecision),
    }
}

/// F5/F12 — the launch verdict of an answered evaluation: `rejected` when
/// `done_when_verifiable` resolves below the verdict bound — Jev answered,
/// and the answer was the Task's doneWhen is not verifiable. `None` lets the
/// Launch proceed to `route`.
#[must_use]
pub fn evaluation_verdict(evaluation: &Evaluation) -> Option<LaunchOutcome> {
    if noul_yes(evaluation.done_when_verifiable, None) {
        None
    } else {
        Some(LaunchOutcome::Rejected)
    }
}

/// F13 step 1 — validate a launch-purpose judgment set into the typed
/// `Evaluation` `route` reads. Valid means: `outcome` `answered`, the
/// answered set is exactly `evaluation_questions(open_tabs)` where
/// `open_tabs` is the caller's open governor tabs the request was built
/// with, and every answer is in contract —
///
/// - a noul records `probabilities["yes"]` (P(yes), in `[0,1]`) and a
///   `yes`/`no` verdict consistent with the applied threshold
///   (`Judgment::threshold`, else the verdict bound);
/// - a choice records `probabilities` over the offered labels and an
///   `answer` naming one of them; `weakest_sufficient_tier` must name a
///   policy tier.
///
/// Anything else is a malformed response — a failed evaluation, so the
/// Launch abstains `evaluation_failed` (F12).
///
/// `related_tab` validates against the stored distribution, not the current
/// tab list: a tab the answer names may legitimately have closed since the
/// request, and F14 placement degrades that to a new tab.
pub fn validate_evaluation(
    record: &JudgmentRecord,
    policy: &Policy,
    open_tabs: &[TabId],
) -> Result<Evaluation, AbstainReason> {
    if record.set.purpose != JudgmentPurpose::Launch {
        return Err(AbstainReason::EvaluationFailed);
    }
    if let Some(reason) = evaluation_abstention(record.set.outcome) {
        return Err(reason);
    }
    let mut by_question: BTreeMap<Question, &Judgment> = BTreeMap::new();
    for judgment in &record.judgments {
        let new = by_question.insert(judgment.question, judgment);
        if new.is_some()
            || !judgment
                .probabilities
                .values()
                .all(|p| valid_probability(*p))
        {
            return Err(AbstainReason::EvaluationFailed);
        }
    }
    if !evaluation_questions_match(&evaluation_questions(open_tabs), &by_question) {
        return Err(AbstainReason::EvaluationFailed);
    }
    let done_when_verifiable = noul_at(&by_question, Question::DoneWhenVerifiable)?;
    let tier_label = choice_at(&by_question, Question::WeakestSufficientTier)?;
    let weakest_sufficient_tier = policy
        .tiers
        .iter()
        .find(|tier| tier.0 == tier_label)
        .cloned()
        .ok_or(AbstainReason::EvaluationFailed)?;
    let changes_files = match choice_at(&by_question, Question::ChangesFiles)? {
        "none" => ChangesFiles::None,
        "few" => ChangesFiles::Few,
        "broad" => ChangesFiles::Broad,
        _ => return Err(AbstainReason::EvaluationFailed),
    };
    let security_boundary = noul_at(&by_question, Question::SecurityBoundary)?;
    let needs_external = noul_at(&by_question, Question::NeedsExternal)?;
    let long_running = noul_at(&by_question, Question::LongRunning)?;
    let related_tab = if open_tabs.is_empty() {
        None
    } else {
        Some(tab_at(&by_question)?)
    };
    Ok(Evaluation {
        done_when_verifiable,
        weakest_sufficient_tier,
        changes_files,
        security_boundary,
        needs_external,
        long_running,
        related_tab,
    })
}

/// The answered question set is exactly the asked set — no missing row, no
/// extra row, no wrong-purpose answer.
fn evaluation_questions_match(
    asked: &[Question],
    by_question: &BTreeMap<Question, &Judgment>,
) -> bool {
    asked.len() == by_question.len()
        && asked
            .iter()
            .all(|question| by_question.contains_key(question))
}

/// One answered noul's P(yes), with the verdict consistency checked against
/// `threshold` (the verdict bound when none was applied).
fn noul_at(
    by_question: &BTreeMap<Question, &Judgment>,
    question: Question,
) -> Result<Probability, AbstainReason> {
    let judgment = by_question
        .get(&question)
        .ok_or(AbstainReason::EvaluationFailed)?;
    let probability = judgment
        .probabilities
        .get("yes")
        .copied()
        .ok_or(AbstainReason::EvaluationFailed)?;
    let verdict = if noul_yes(probability, judgment.threshold) {
        "yes"
    } else {
        "no"
    };
    if judgment.answer == verdict {
        Ok(probability)
    } else {
        Err(AbstainReason::EvaluationFailed)
    }
}

/// One answered choice's label — it must be a member of its own recorded
/// distribution.
fn choice_at<'j>(
    by_question: &BTreeMap<Question, &'j Judgment>,
    question: Question,
) -> Result<&'j str, AbstainReason> {
    let judgment = by_question
        .get(&question)
        .ok_or(AbstainReason::EvaluationFailed)?;
    if judgment.probabilities.contains_key(&judgment.answer) {
        Ok(judgment.answer.as_str())
    } else {
        Err(AbstainReason::EvaluationFailed)
    }
}

/// The `related_tab` answer: `new` is `New`; anything else is the named tab.
fn tab_at(by_question: &BTreeMap<Question, &Judgment>) -> Result<TabChoice, AbstainReason> {
    match choice_at(by_question, Question::RelatedTab)? {
        "new" => Ok(TabChoice::New),
        tab => Ok(TabChoice::Tab(TabId(String::from(tab)))),
    }
}
