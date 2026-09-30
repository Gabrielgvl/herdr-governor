//! Test fixtures for the acceptance module — constructed inputs, no I/O.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use sha2::Digest as _;

use crate::acceptance::{
    AssessmentKey, FrozenHandoff, HANDOFF_MARKER_PREFIX, HANDOFF_MARKER_SUFFIX, assessment_key,
    freeze_handoff,
};
use crate::config::ConfigVersion;
use crate::identity::{Digest, RunId, Timestamp};
use crate::routing::QuestionVersion;

pub(super) fn run() -> RunId {
    RunId("run-0199abcd".into())
}

pub(super) fn marker(run: &RunId) -> String {
    format!("{HANDOFF_MARKER_PREFIX}{}{HANDOFF_MARKER_SUFFIX}", run.0)
}

/// A free-Markdown report closed by the run's end marker.
pub(super) fn handoff_body(run: &RunId) -> Vec<u8> {
    format!("## Report\n\n- done\n\n{}\n", marker(run)).into_bytes()
}

pub(super) fn byte_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

pub(super) fn sha256(bytes: &[u8]) -> Digest {
    Digest(sha2::Sha256::digest(bytes).into())
}

pub(super) fn frozen(work_generation: u64, tag: u8) -> FrozenHandoff {
    freeze_handoff(
        run(),
        work_generation,
        Digest([tag; 32]),
        format!("/state/handoffs/{work_generation}-{tag}"),
        Timestamp(1_000),
    )
}

pub(super) fn key(task_digest: Digest, handoff: &FrozenHandoff, item: u8) -> AssessmentKey {
    assessment_key(
        task_digest,
        handoff,
        QuestionVersion("qv-1".into()),
        ConfigVersion("pv-1".into()),
        item,
    )
}
