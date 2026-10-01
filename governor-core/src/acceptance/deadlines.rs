//! F24 — the judgment and repair deadlines: each arms once (`existing` wins),
//! and an overdue one settles the Run — `unresolved(judgment_unavailable)`
//! or `rejected`.

use core::time::Duration;

use crate::identity::Timestamp;
use crate::lifecycle::{Settlement, UnresolvedReason};

/// F24 — `judgment_deadline`: `window` after the freeze (default
/// `DEFAULT_JUDGMENT_WINDOW`, 30 minutes). The freeze transaction stamps it
/// only when unset (Appendix B) — a re-freeze never re-arms it.
#[must_use]
pub fn judgment_deadline(
    existing: Option<Timestamp>,
    frozen_at: Timestamp,
    window: Duration,
) -> Timestamp {
    existing.unwrap_or_else(|| frozen_at.after(window))
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
    existing.unwrap_or_else(|| rejected_at.after(window))
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
