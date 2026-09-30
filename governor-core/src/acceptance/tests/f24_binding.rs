//! F24 — the freeze record and the assessment binding: key fields, the
//! never-re-judge rule, the outstanding set and the verdict.

use alloc::vec;
use alloc::vec::Vec;

use super::builders::{frozen, key, run};
use crate::acceptance::{AssessmentKey, assessment_key, freeze_handoff, unjudged_items, verdict};
use crate::config::ConfigVersion;
use crate::identity::{Digest, Timestamp};
use crate::lifecycle::JudgmentVerdict;
use crate::routing::{Question, QuestionVersion};

#[test]
fn f24_freeze_binds_run_generation_and_digest() {
    let run = run();
    let digest = Digest([9; 32]);
    let frozen = freeze_handoff(run.clone(), 2, digest, "/state/h".into(), Timestamp(42));
    assert_eq!(frozen.run, run, "the record carries the run");
    assert_eq!(frozen.work_generation, 2, "carries the work generation");
    assert_eq!(frozen.digest, digest, "carries the digest");
    assert_eq!(frozen.frozen_path, "/state/h", "coordinator path input");
    assert_eq!(frozen.frozen_at, Timestamp(42), "coordinator time input");
}

#[test]
fn f24_assessment_key_binds_digest_generation_versions_and_item() {
    let handoff = frozen(3, 9);
    let task_digest = Digest([1; 32]);
    let qv = QuestionVersion("qv-7".into());
    let pv = ConfigVersion("pv-2".into());
    let key = assessment_key(task_digest, &handoff, qv.clone(), pv.clone(), 4);
    assert_eq!(key.task_digest, task_digest, "binds the Task digest");
    assert_eq!(
        key.handoff_digest, handoff.digest,
        "binds the handoff digest"
    );
    assert_eq!(key.work_generation, 3, "binds the work generation");
    assert_eq!(key.question_version, qv, "binds the question version");
    assert_eq!(key.policy_version, pv, "binds the policy version");
    assert_eq!(key.item, 4, "binds the item index");
}

#[test]
fn f24_assessment_question_names_the_item() {
    let key = key(Digest([7; 32]), &frozen(0, 1), 2);
    assert_eq!(
        key.question(),
        Question::HandoffMeetsItem { item: 2 },
        "the assessment asks handoff_meets_item_k"
    );
    assert_eq!(
        key.question_name(),
        "handoff_meets_item_2",
        "the stored spelling appends the item"
    );
}

#[test]
fn f24_completed_assessment_is_never_rejudged() {
    let handoff = frozen(1, 2);
    let key = key(Digest([7; 32]), &handoff, 0);
    assert!(
        key.needs_judgment(&[]),
        "no completed assessment means the item is judged"
    );
    assert!(
        !key.needs_judgment(core::slice::from_ref(&key)),
        "a completed assessment under the same key is never re-judged"
    );
    let completed = [key.clone()];
    let mut new_handoff = key.clone();
    new_handoff.handoff_digest = Digest([8; 32]);
    assert!(
        new_handoff.needs_judgment(&completed),
        "a changed handoff digest is new evidence"
    );
    let mut other_item = key.clone();
    other_item.item = 1;
    assert!(
        other_item.needs_judgment(&completed),
        "a different item was never judged"
    );
    let mut new_generation = key.clone();
    new_generation.work_generation = 2;
    assert!(
        new_generation.needs_judgment(&completed),
        "a new work generation judges afresh"
    );
    let mut new_task = key.clone();
    new_task.task_digest = Digest([3; 32]);
    assert!(
        new_task.needs_judgment(&completed),
        "a changed task digest re-judges"
    );
}

#[test]
fn f24_unjudged_items_are_the_outstanding_assessments() {
    let handoff = frozen(1, 2);
    let task_digest = Digest([1; 32]);
    let qv = QuestionVersion("qv-1".into());
    let pv = ConfigVersion("pv-1".into());
    let items_of = |keys: &[AssessmentKey]| keys.iter().map(|k| k.item).collect::<Vec<u8>>();
    assert_eq!(
        items_of(&unjudged_items(task_digest, &handoff, &qv, &pv, 3, &[])),
        vec![0, 1, 2],
        "nothing judged — every item is outstanding"
    );
    let done = key(task_digest, &handoff, 1);
    assert_eq!(
        items_of(&unjudged_items(task_digest, &handoff, &qv, &pv, 3, &[done])),
        vec![0, 2],
        "a completed item drops out of the outstanding set"
    );
    let mut stale = key(task_digest, &handoff, 0);
    stale.handoff_digest = Digest([9; 32]);
    assert_eq!(
        unjudged_items(task_digest, &handoff, &qv, &pv, 3, &[stale]).len(),
        3,
        "a stale binding satisfies nothing — the new digest re-judges"
    );
    let all_done = Vec::from_iter((0..3).map(|item| key(task_digest, &handoff, item)));
    assert!(
        unjudged_items(task_digest, &handoff, &qv, &pv, 3, &all_done).is_empty(),
        "a fully judged digest has nothing outstanding"
    );
}

#[test]
fn f24_all_items_met_accepts() {
    assert_eq!(
        verdict(&[true]),
        JudgmentVerdict::Accept,
        "a single met item accepts"
    );
    assert_eq!(
        verdict(&[true, true, true]),
        JudgmentVerdict::Accept,
        "every item met accepts the handoff"
    );
}

#[test]
fn f24_any_unmet_item_repairs() {
    assert_eq!(
        verdict(&[true, false]),
        JudgmentVerdict::Reject,
        "one unmet item rejects into repair"
    );
    assert_eq!(
        verdict(&[false]),
        JudgmentVerdict::Reject,
        "an unmet single item rejects"
    );
    assert_eq!(
        verdict(&[false, false, false]),
        JudgmentVerdict::Reject,
        "all unmet rejects"
    );
}
