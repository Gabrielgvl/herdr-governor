//! F24 — handoff reading, freezing and per-item assessment binding: the
//! marked file is read once, frozen by digest per work generation, and every
//! assessment is bound to a fixed key so unchanged evidence is never
//! re-judged. Everything here is pure: the file's metadata and bytes, times
//! and the completed-assessment set arrive as values; readings, freeze
//! records, deadlines, outstanding assessments and verdicts leave as values.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use sha2::Digest as _;

use crate::config::ConfigVersion;
use crate::identity::{Digest, RunId, Timestamp};
use crate::lifecycle::{JudgmentVerdict, Settlement, UnresolvedReason};
use crate::routing::{Question, QuestionVersion};

/// N5/F24 — the handoff is a regular file of at most 256 KiB; anything else
/// counts as not written yet.
pub const HANDOFF_MAX_BYTES: usize = 256 * 1024;

/// F24 — the handoff marker the file's final non-whitespace content must be:
/// `<!-- herdr-governor handoff run=<runId> -->`.
pub const HANDOFF_MARKER_PREFIX: &str = "<!-- herdr-governor handoff run=";

/// F24 — the marker's closing bytes.
pub const HANDOFF_MARKER_SUFFIX: &str = " -->";

/// F24 — the result of reading the marked file without following symlinks:
/// a regular file within `HANDOFF_MAX_BYTES` whose final non-whitespace
/// content is the marker, or "not written yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffReading {
    /// A valid marked file was read and digested.
    Valid {
        /// The frozen content's digest.
        digest: Digest,
    },
    /// Anything else counts as not written yet — never a failure, never a
    /// settlement cause by itself.
    NotWritten,
}

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

/// F24/N5 — the reading predicate over the adapter's no-follow stat and the
/// file's bytes; the core never touches a filesystem, so both arrive as
/// values. `regular_file` is the `symlink_metadata`/`O_NOFOLLOW` file-type
/// answer: a symlink, directory, other node, missing path or failed stat is
/// `false`. `size` is the metadata byte count, `bytes` the file's full
/// content (`None` when the read failed); a `size`/`bytes` disagreement is an
/// untrusted read.
///
/// `Valid` requires all of: a regular file, at most `HANDOFF_MAX_BYTES`, and
/// trailing-ASCII-whitespace-stripped content ending in the marker — the
/// free-Markdown report ahead of it is unrestricted. `digest` is the sha-256
/// of the bytes as read — the bytes that freeze. Anything else is
/// `NotWritten`: never a failure, never a settlement cause by itself.
#[must_use]
pub fn read_handoff(
    run: &RunId,
    regular_file: bool,
    size: u64,
    bytes: Option<&[u8]>,
) -> HandoffReading {
    let Ok(max) = u64::try_from(HANDOFF_MAX_BYTES) else {
        return HandoffReading::NotWritten;
    };
    if !regular_file || size > max {
        return HandoffReading::NotWritten;
    }
    let Some(content) = bytes else {
        return HandoffReading::NotWritten;
    };
    if u64::try_from(content.len()) != Ok(size) || content.len() > HANDOFF_MAX_BYTES {
        return HandoffReading::NotWritten;
    }
    let marker = alloc::format!(
        "{}{}{}",
        HANDOFF_MARKER_PREFIX,
        run.0,
        HANDOFF_MARKER_SUFFIX
    );
    if !content.trim_ascii_end().ends_with(marker.as_bytes()) {
        return HandoffReading::NotWritten;
    }
    HandoffReading::Valid {
        digest: Digest(sha2::Sha256::digest(content).into()),
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
    }
}

/// F24 — `judgment_deadline`: `window` after the freeze (default
/// `DEFAULT_JUDGMENT_WINDOW`, 30 minutes). The freeze transaction stamps it
/// only when unset (Appendix B) — a re-freeze never re-arms it.
#[must_use]
pub fn judgment_deadline(
    existing: Option<Timestamp>,
    frozen_at: Timestamp,
    window: Duration,
) -> Timestamp {
    existing.unwrap_or_else(|| deadline_after(frozen_at, window))
}

/// F24 — `repair_deadline`: `window` after the work generation's first
/// rejection (default `DEFAULT_REPAIR_WINDOW`, 15 minutes). Rewrites,
/// re-rejections and restarts never extend it — `existing` wins when set.
/// `existing` is this generation's deadline: `None` until its first rejection
/// (the repair dispatch that opens a new generation clears the field).
#[must_use]
pub fn repair_deadline(
    existing: Option<Timestamp>,
    rejected_at: Timestamp,
    window: Duration,
) -> Timestamp {
    existing.unwrap_or_else(|| deadline_after(rejected_at, window))
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

/// F24 — the judgment deadline passed with the assessment still unanswered:
/// the Run settles `unresolved(judgment_unavailable)`. The governor never
/// invents a verdict Jev did not make.
#[must_use]
pub fn judgment_overdue(now: Timestamp, deadline: Option<Timestamp>) -> Option<Settlement> {
    match deadline {
        Some(at) => (now >= at).then_some(Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable,
        }),
        None => None,
    }
}

/// F24 — the repair deadline passed with no qualifying repair: the Run
/// settles `rejected`.
#[must_use]
pub fn repair_overdue(now: Timestamp, deadline: Option<Timestamp>) -> Option<Settlement> {
    match deadline {
        Some(at) => (now >= at).then_some(Settlement::Rejected),
        None => None,
    }
}

/// An absolute deadline `window` after `at` (F22: deadlines are absolute
/// times) — saturating, so a pathological window means "effectively never"
/// rather than wrapping into the past.
fn deadline_after(at: Timestamp, window: Duration) -> Timestamp {
    let millis = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
    Timestamp(at.0.saturating_add(millis))
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::time::Duration;

    use proptest::collection::vec as prop_vec;
    use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};
    use proptest::sample::select;
    use sha2::Digest as _;

    use super::{
        AssessmentKey, FrozenHandoff, HANDOFF_MARKER_PREFIX, HANDOFF_MARKER_SUFFIX,
        HANDOFF_MAX_BYTES, HandoffReading, assessment_key, freeze_handoff, judgment_deadline,
        judgment_overdue, read_handoff, repair_deadline, repair_overdue, unjudged_items, verdict,
    };
    use crate::config::ConfigVersion;
    use crate::identity::{Digest, RunId, Timestamp};
    use crate::lifecycle::{JudgmentVerdict, Settlement, UnresolvedReason};
    use crate::routing::{Question, QuestionVersion};

    fn run() -> RunId {
        RunId("run-0199abcd".into())
    }

    fn marker(run: &RunId) -> String {
        format!("{HANDOFF_MARKER_PREFIX}{}{HANDOFF_MARKER_SUFFIX}", run.0)
    }

    /// A free-Markdown report closed by the run's end marker.
    fn handoff_body(run: &RunId) -> Vec<u8> {
        format!("## Report\n\n- done\n\n{}\n", marker(run)).into_bytes()
    }

    fn byte_len(bytes: &[u8]) -> u64 {
        u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    }

    fn sha256(bytes: &[u8]) -> Digest {
        Digest(sha2::Sha256::digest(bytes).into())
    }

    fn frozen(work_generation: u64, tag: u8) -> FrozenHandoff {
        freeze_handoff(
            run(),
            work_generation,
            Digest([tag; 32]),
            format!("/state/handoffs/{work_generation}-{tag}"),
            Timestamp(1_000),
        )
    }

    fn key(task_digest: Digest, handoff: &FrozenHandoff, item: u8) -> AssessmentKey {
        assessment_key(
            task_digest,
            handoff,
            QuestionVersion("qv-1".into()),
            ConfigVersion("pv-1".into()),
            item,
        )
    }

    #[test]
    fn f24_n5_handoff_bound_and_marker() {
        assert_eq!(
            HANDOFF_MAX_BYTES, 262_144,
            "handoff bound is 256 KiB (N5/F24)"
        );
        assert_eq!(
            HANDOFF_MARKER_PREFIX, "<!-- herdr-governor handoff run=",
            "marker prefix is the F24 opening"
        );
        assert_eq!(
            HANDOFF_MARKER_SUFFIX, " -->",
            "marker suffix is the F24 closing"
        );
    }

    #[test]
    fn f24_reading_valid_marked_regular_file() {
        let run = run();
        let bytes = handoff_body(&run);
        match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
            HandoffReading::Valid { digest } => {
                // The digest covers the bytes as read — including the trailing
                // newline — because those are the bytes that freeze (F24).
                assert_eq!(
                    digest,
                    sha256(&bytes),
                    "the frozen digest covers the raw bytes"
                );
            }
            HandoffReading::NotWritten => panic!("a regular marked file must read Valid"),
        }
    }

    #[test]
    fn f24_reading_nonregular_or_unreadable_is_not_written() {
        let run = run();
        let bytes = handoff_body(&run);
        // The read never follows symlinks, so `regular_file=false` covers a
        // symlink, a directory, any other node and a failed stat.
        assert_eq!(
            read_handoff(&run, false, byte_len(&bytes), Some(&bytes)),
            HandoffReading::NotWritten,
            "a non-regular file counts as not written yet"
        );
        assert_eq!(
            read_handoff(&run, true, byte_len(&bytes), None),
            HandoffReading::NotWritten,
            "a failed content read counts as not written yet"
        );
        assert_eq!(
            read_handoff(&run, false, 0, None),
            HandoffReading::NotWritten,
            "a missing path counts as not written yet"
        );
    }

    #[test]
    fn f24_reading_overbound_file_is_not_written() {
        let run = run();
        let mut bytes = handoff_body(&run);
        bytes.resize(HANDOFF_MAX_BYTES + 1, b' ');
        let size = byte_len(&bytes);
        assert_eq!(
            read_handoff(&run, true, size, Some(&bytes)),
            HandoffReading::NotWritten,
            "a file over 256 KiB counts as not written yet (N5)"
        );
        // The metadata bound alone decides — no content read is owed.
        assert_eq!(
            read_handoff(&run, true, size, None),
            HandoffReading::NotWritten,
            "an over-bound file refuses on metadata alone"
        );
    }

    #[test]
    fn f24_reading_at_the_bound_reads_valid() {
        let run = run();
        let marker = marker(&run);
        let mut bytes = vec![b'.'; HANDOFF_MAX_BYTES - marker.len()];
        bytes.extend_from_slice(marker.as_bytes());
        assert_eq!(
            bytes.len(),
            HANDOFF_MAX_BYTES,
            "the test file sits exactly on the bound"
        );
        match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
            HandoffReading::Valid { digest } => {
                assert_eq!(digest, sha256(&bytes), "at 256 KiB the file still reads");
            }
            HandoffReading::NotWritten => panic!("256 KiB exactly is within the bound (N5)"),
        }
    }

    #[test]
    fn f24_reading_size_and_bytes_must_agree() {
        let run = run();
        let bytes = handoff_body(&run);
        assert_eq!(
            read_handoff(&run, true, byte_len(&bytes) + 1, Some(&bytes)),
            HandoffReading::NotWritten,
            "a stat/content mismatch is an untrusted read — not written yet"
        );
    }

    #[test]
    fn f24_reading_marker_must_be_the_final_content() {
        let run = run();
        // Another Run's marker is not this Run's handoff.
        let other = handoff_body(&RunId("run-ffffffff".into()));
        assert_eq!(
            read_handoff(&run, true, byte_len(&other), Some(&other)),
            HandoffReading::NotWritten,
            "a marker naming another run is not written yet"
        );
        // Content after the marker means it is not the file's end.
        let mut appended = handoff_body(&run);
        appended.extend_from_slice(b"\nnot the end\n");
        assert_eq!(
            read_handoff(&run, true, byte_len(&appended), Some(&appended)),
            HandoffReading::NotWritten,
            "trailing content after the marker is not written yet"
        );
        // No marker at all.
        let plain = b"all done".as_slice();
        assert_eq!(
            read_handoff(&run, true, byte_len(plain), Some(plain)),
            HandoffReading::NotWritten,
            "a markerless file is not written yet"
        );
        // Whitespace only.
        let blank = b"  \n\t ".as_slice();
        assert_eq!(
            read_handoff(&run, true, byte_len(blank), Some(blank)),
            HandoffReading::NotWritten,
            "a whitespace-only file is not written yet"
        );
    }

    #[test]
    fn f24_reading_allows_trailing_whitespace_after_marker() {
        let run = run();
        let mut bytes = marker(&run).into_bytes();
        bytes.extend_from_slice(b" \t\r\n\x0c\n\n");
        match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
            HandoffReading::Valid { digest } => {
                assert_eq!(
                    digest,
                    sha256(&bytes),
                    "trailing whitespace still reads Valid"
                );
            }
            HandoffReading::NotWritten => {
                panic!("trailing whitespace past the marker must read Valid")
            }
        }
    }

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

    #[test]
    fn f24_repair_deadline_anchors_the_first_rejection() {
        assert_eq!(
            repair_deadline(None, Timestamp(60_000), Duration::from_mins(15)),
            Timestamp(960_000),
            "the deadline is first rejection + the 15-minute window"
        );
    }

    #[test]
    fn f24_repair_deadline_is_never_extended() {
        let existing = Timestamp(100);
        assert_eq!(
            repair_deadline(Some(existing), Timestamp(999_999), Duration::from_mins(15)),
            existing,
            "a re-rejection keeps the generation's first deadline"
        );
    }

    #[test]
    fn f24_judgment_deadline_anchors_the_freeze() {
        assert_eq!(
            judgment_deadline(None, Timestamp(60_000), Duration::from_mins(30)),
            Timestamp(1_860_000),
            "the deadline is freeze + the 30-minute window"
        );
    }

    #[test]
    fn f24_judgment_deadline_is_stamped_once() {
        let existing = Timestamp(1_234);
        assert_eq!(
            judgment_deadline(Some(existing), Timestamp(60_000), Duration::from_mins(30)),
            existing,
            "the freeze transaction sets judgment_deadline only when unset (Appendix B)"
        );
    }

    #[test]
    fn f24_overdue_judgment_settles_unresolved() {
        let deadline = Some(Timestamp(1_000));
        assert_eq!(
            judgment_overdue(Timestamp(1_000), deadline),
            Some(Settlement::Unresolved {
                reason: UnresolvedReason::JudgmentUnavailable,
            }),
            "the deadline instant itself is already overdue"
        );
        assert_eq!(
            judgment_overdue(Timestamp(999), deadline),
            None,
            "before the deadline nothing settles"
        );
        assert_eq!(
            judgment_overdue(Timestamp(9_999), None),
            None,
            "no judgment deadline means nothing fires"
        );
    }

    #[test]
    fn f24_overdue_repair_settles_rejected() {
        let deadline = Some(Timestamp(5_000));
        assert_eq!(
            repair_overdue(Timestamp(5_000), deadline),
            Some(Settlement::Rejected),
            "a repair window passed unmet settles rejected"
        );
        assert_eq!(
            repair_overdue(Timestamp(4_999), deadline),
            None,
            "before the repair deadline nothing settles"
        );
        assert_eq!(
            repair_overdue(Timestamp(9_999), None),
            None,
            "no repair deadline means nothing fires"
        );
    }

    proptest! {
        /// A regular in-bound file ending in its run's marker (modulo trailing
        /// whitespace) always reads Valid; a non-regular file never does.
        #[test]
        fn f24_reading_valid_iff_regular_bounded_marked(
            regular in any::<bool>(),
            head in prop_vec(any::<u8>(), 0..64),
            tail_ws in prop_vec(select(Vec::from([b' ', b'\t', b'\n', b'\r', 0x0cu8])), 0..8),
        ) {
            let run = run();
            let mut bytes = head;
            bytes.extend_from_slice(marker(&run).as_bytes());
            bytes.extend_from_slice(&tail_ws);
            match read_handoff(&run, regular, byte_len(&bytes), Some(&bytes)) {
                HandoffReading::Valid { digest } => {
                    prop_assert!(regular, "only a regular file reads Valid");
                    prop_assert_eq!(
                        digest,
                        sha256(&bytes),
                        "the digest covers the raw bytes"
                    );
                }
                HandoffReading::NotWritten => {
                    prop_assert!(!regular, "a regular marked in-bound file must read Valid");
                }
            }
        }

        /// A Valid reading is only ever produced under the three conditions —
        /// arbitrary bytes can never forge one.
        #[test]
        fn f24_reading_never_valid_without_the_conditions(
            regular in any::<bool>(),
            bytes in prop_vec(any::<u8>(), 0..256),
        ) {
            let run = run();
            if let HandoffReading::Valid { digest } =
                read_handoff(&run, regular, byte_len(&bytes), Some(&bytes))
            {
                prop_assert!(regular, "Valid requires a regular file");
                prop_assert!(
                    bytes.trim_ascii_end().ends_with(marker(&run).as_bytes()),
                    "Valid requires the marker as the final non-whitespace content"
                );
                prop_assert_eq!(digest, sha256(&bytes), "Valid digests the raw bytes");
            }
        }
    }
}
