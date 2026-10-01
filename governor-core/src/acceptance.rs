//! F24 — handoff reading, freezing and per-item assessment binding: the
//! marked file is read once, frozen by digest per work generation, and every
//! assessment is bound to a fixed key so unchanged evidence is never
//! re-judged. Everything here is pure: the file's metadata and bytes, times
//! and the completed-assessment set arrive as values; readings, freeze
//! records, deadlines, outstanding assessments and verdicts leave as values.

mod binding;
mod deadlines;
mod reading;

pub use binding::{
    AssessmentKey, FrozenHandoff, assessment_key, freeze_handoff, unjudged_items, verdict,
};
pub use deadlines::{judgment_deadline, judgment_overdue, repair_deadline, repair_overdue};
pub use reading::{
    HANDOFF_MARKER_PREFIX, HANDOFF_MARKER_SUFFIX, HANDOFF_MAX_BYTES, HandoffReading, read_handoff,
};

#[cfg(test)]
mod tests;
