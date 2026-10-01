//! F3 — malformed-snapshot duplicates: one repeated locator or two panes
//! matching one identity make the read `Invalid`, never silently `Absent`
//! or `Unique` (H#74).

use alloc::vec::Vec;

use super::builders::{child, occupied};
use crate::identity::{HerdrIncarnation, Observation, classify};

#[test]
fn f3_duplicate_locator_is_invalid() {
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
        occupied("w6:p9", "term-7", "kind-7", "other", Some("sess-7"), None),
    ]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Invalid,
        "F3 — a snapshot listing one locator twice is malformed"
    );
}

#[test]
fn f3_duplicate_locator_deep_in_a_snapshot_is_invalid() {
    // the malformed marker is a repeated locator anywhere in the snapshot —
    // one dup among many distinct rows still invalidates the whole read.
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let mut agents = Vec::from([
        occupied("w6:p1", "term-1", "kind-1", "agent-a", Some("sess-a"), None),
        occupied("w6:p2", "term-2", "kind-2", "agent-b", Some("sess-b"), None),
        occupied("w6:p3", "term-3", "kind-3", "agent-c", Some("sess-c"), None),
        occupied("w6:p4", "term-4", "kind-4", "agent-d", Some("sess-d"), None),
        occupied("w6:p5", "term-5", "kind-5", "agent-e", Some("sess-e"), None),
        occupied("w6:p6", "term-6", "kind-6", "agent-f", Some("sess-f"), None),
        occupied("w6:p7", "term-7", "kind-7", "agent-g", Some("sess-g"), None),
    ]);
    agents.push(occupied(
        "w6:p4",
        "term-8",
        "kind-8",
        "agent-h",
        Some("sess-h"),
        None,
    ));
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Invalid,
        "F3 — one repeated locator deep in a snapshot is still malformed"
    );
    // and without the dup the same snapshot resolves absent.
    agents.pop();
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Absent,
        "F3 — the remaining rows are unremarkable"
    );
}

#[test]
fn f3_duplicate_identity_match_is_invalid() {
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([
        occupied(
            "w6:p2",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
        occupied(
            "w6:p5",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
    ]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Invalid,
        "F3 — two panes matching one identity is ambiguous, never unique"
    );
}
