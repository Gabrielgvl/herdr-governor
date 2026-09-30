//! F24 — handoff reading, freezing and per-item assessment binding: the
//! marked file is read once, frozen by digest per work generation, and every
//! assessment is bound to a fixed key so unchanged evidence is never
//! re-judged.

use alloc::string::String;

use crate::config::ConfigVersion;
use crate::identity::{Digest, RunId, Timestamp};
use crate::routing::QuestionVersion;

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
