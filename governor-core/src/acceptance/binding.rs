//! F24 — the freeze record and the per-item assessment binding: the frozen
//! copy keyed `(run_id, work_generation, digest)`, the fixed `AssessmentKey`
//! unchanged evidence is never re-judged under, the outstanding-assessment
//! set and the final verdict.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::ConfigVersion;
use crate::identity::{Digest, RunId, Timestamp};
use crate::lifecycle::JudgmentVerdict;
use crate::routing::{Question, QuestionVersion};

/// F24/Appendix B `handoffs` — the frozen copy of a valid marked file, keyed
/// `(run_id, work_generation, digest)`. `frozen_path`/`frozen_at` are supplied
/// by the coordinator as inputs when the freeze is planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenHandoff {
    /// `run_id`.
    pub run: RunId,
    /// `work_generation` — which generation of work this copy belongs to.
    pub work_generation: u64,
    /// `digest` — the frozen bytes' digest.
    pub digest: Digest,
    /// `frozen_path` — where the immutable copy lives.
    pub frozen_path: String,
    /// `frozen_at` — when the copy was taken.
    pub frozen_at: Timestamp,
    /// `assessed` — `true` when an `answered` acceptance `judgment_sets` row
    /// exists for `(run_id, work_generation, handoff_digest)`; the read
    /// computes this Appendix B derivation — it is not a stored column —
    /// so a freeze write always carries `false`. F24: an unchanged digest
    /// is never re-judged after a completed assessment; an assessed row
    /// suppresses the re-ask, an unassessed one resumes judging.
    pub assessed: bool,
}

/// F24 — the binding every doneWhen-item assessment is recorded under: Task
/// digest, handoff digest, work generation, question version and policy
/// version, plus the item index (`handoff_meets_item_k`). An unchanged key is
/// never re-judged after a completed assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssessmentKey {
    /// The canonical Task's digest.
    pub task_digest: Digest,
    /// The frozen handoff's digest.
    pub handoff_digest: Digest,
    /// The work generation being judged.
    pub work_generation: u64,
    /// The question-set version.
    pub question_version: QuestionVersion,
    /// The policy version.
    pub policy_version: ConfigVersion,
    /// Which `done_when` item the assessment covers.
    pub item: u8,
}

impl AssessmentKey {
    /// F24 — the Jev question this assessment answers.
    #[must_use]
    pub fn question(&self) -> Question {
        Question::HandoffMeetsItem { item: self.item }
    }

    /// F24 — the stored question spelling, `handoff_meets_item_<item>`
    /// (`Question::as_str` carries the family prefix; the item suffix belongs
    /// to the binding).
    #[must_use]
    pub fn question_name(&self) -> String {
        alloc::format!("{}_{}", self.question().as_str(), self.item)
    }

    /// F24 — an unchanged digest is never re-judged after a completed
    /// assessment: a key already in `completed` stands, and any changed field
    /// (task, handoff, generation, versions, item) is a new key that judges
    /// afresh.
    #[must_use]
    pub fn needs_judgment(&self, completed: &[Self]) -> bool {
        completed.iter().all(|done| done != self)
    }
}

/// F24/Appendix B — the freeze record: the `handoffs` row keyed `(run,
/// work_generation, digest)`; `frozen_path`/`frozen_at` are
/// coordinator-supplied inputs when the freeze is planned.
#[must_use]
pub fn freeze_handoff(
    run: RunId,
    work_generation: u64,
    digest: Digest,
    frozen_path: String,
    frozen_at: Timestamp,
) -> FrozenHandoff {
    FrozenHandoff {
        run,
        work_generation,
        digest,
        frozen_path,
        frozen_at,
        assessed: false,
    }
}

/// F24 — the assessment key for doneWhen `item` (its 0-based index) under one
/// binding: the Task digest, the frozen handoff's digest and work generation,
/// the question-set version and the policy version.
#[must_use]
pub fn assessment_key(
    task_digest: Digest,
    handoff: &FrozenHandoff,
    question_version: QuestionVersion,
    policy_version: ConfigVersion,
    item: u8,
) -> AssessmentKey {
    AssessmentKey {
        task_digest,
        handoff_digest: handoff.digest,
        work_generation: handoff.work_generation,
        question_version,
        policy_version,
        item,
    }
}

/// F24 — the assessments still owed: keys for items `0..item_count` under the
/// current binding, minus the ones `completed` already answers. Empty means
/// the handoff digest is fully judged under this binding — a re-written file
/// with the same digest is never re-judged.
#[must_use]
pub fn unjudged_items(
    task_digest: Digest,
    handoff: &FrozenHandoff,
    question_version: &QuestionVersion,
    policy_version: &ConfigVersion,
    item_count: u8,
    completed: &[AssessmentKey],
) -> Vec<AssessmentKey> {
    (0..item_count)
        .map(|item| {
            assessment_key(
                task_digest,
                handoff,
                question_version.clone(),
                policy_version.clone(),
                item,
            )
        })
        .filter(|key| key.needs_judgment(completed))
        .collect()
}

/// F24 — the verdict once every doneWhen item has a completed assessment:
/// all items met is `accept`, any unmet item is `reject` (into `repair`).
/// `items_met[i]` is item `i`'s resolved `handoff_meets_item_i` answer; the
/// caller passes exactly the Task's `done_when` items (1–8, F5).
#[must_use]
pub fn verdict(items_met: &[bool]) -> JudgmentVerdict {
    if items_met.iter().all(|met| *met) {
        JudgmentVerdict::Accept
    } else {
        JudgmentVerdict::Reject
    }
}
