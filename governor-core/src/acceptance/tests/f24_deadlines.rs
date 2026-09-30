//! F24 — the judgment and repair deadlines: each anchors once and an overdue
//! one settles.

use core::time::Duration;

use crate::acceptance::{judgment_deadline, judgment_overdue, repair_deadline, repair_overdue};
use crate::identity::Timestamp;
use crate::lifecycle::{Settlement, UnresolvedReason};

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
