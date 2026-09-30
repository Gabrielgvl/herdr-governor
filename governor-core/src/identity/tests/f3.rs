//! F3 — observation classes: `classify` over a fresh snapshot, the class
//! spellings, and the invalid-never-absent rule (H#74).

use alloc::vec::Vec;

use super::builders::{agent_row, child, occupied};
use crate::identity::{
    AgentRow, ChildIdentity, ChildStatus, HerdrIncarnation, NativeSession, Observation,
    ObservationClass, PaneId, classify,
};

// ---- F3 — observation classes -----------------------------------------

#[test]
fn f3_observation_class_spellings() {
    let cases = [
        (ObservationClass::Unique, "unique"),
        (ObservationClass::Absent, "absent"),
        (ObservationClass::Invalid, "invalid"),
    ];
    for (class, name) in cases {
        assert_eq!(
            class.as_str(),
            name,
            "observation class spelling must match F3"
        );
    }
}

#[test]
fn appendix_c_child_status_spellings() {
    let cases = [
        (ChildStatus::Working, "working"),
        (ChildStatus::Idle, "idle"),
        (ChildStatus::Done, "done"),
        (ChildStatus::Blocked, "blocked"),
    ];
    for (status, name) in cases {
        assert_eq!(
            status.as_str(),
            name,
            "child status spelling must match Appendix C"
        );
    }
}

#[test]
fn f3_observation_maps_to_its_class() {
    let unique = Observation::Unique {
        status: Some(ChildStatus::Working),
        pane: PaneId("w6:p1".into()),
        native_session: Some(NativeSession("sess".into())),
    };
    assert_eq!(unique.class(), ObservationClass::Unique);
    assert_eq!(Observation::Absent.class(), ObservationClass::Absent);
    assert_eq!(Observation::Invalid.class(), ObservationClass::Invalid);
}

#[test]
fn f3_unique_when_exactly_one_pane_matches() {
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            Some(ChildStatus::Done),
        ),
        occupied(
            "w6:p4",
            "term-9",
            "kind-2",
            "other-agent",
            Some("sess-9"),
            None,
        ),
    ]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Unique {
            status: Some(ChildStatus::Done),
            pane: PaneId("w6:p9".into()),
            native_session: Some(NativeSession("sess-1".into())),
        },
        "F3 — a full identity match is unique, wherever the pane sits"
    );
}

#[test]
fn f3_move_is_followed_never_a_loss() {
    // a4_workspace_move_new_locator — a workspace move re-keys the
    // pane_id; terminal, session, kind and name survive.
    let identity = child("w1:p3", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([occupied(
        "w2:p1",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-1"),
        Some(ChildStatus::Working),
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Unique {
            status: Some(ChildStatus::Working),
            pane: PaneId("w2:p1".into()),
            native_session: Some(NativeSession("sess-1".into())),
        },
        "F3 — a moved child is unique at its new locator (H#75)"
    );
}

#[test]
fn f3_absent_when_no_pane_matches() {
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    for agents in [
        Vec::new(),
        Vec::from([occupied(
            "w6:p4",
            "term-9",
            "kind-2",
            "other",
            Some("sess-9"),
            None,
        )]),
    ] {
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Absent,
            "F3 — a valid snapshot with no matching pane is absent"
        );
    }
}

#[test]
fn f3_new_session_or_terminal_in_the_pane_is_absent() {
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    // a4_native_new_replaces_session — same pane/terminal/name, new
    // session.
    let new_session = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-2"),
        None,
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &new_session),
        Observation::Absent,
        "F3 — a new native_session in the Run's pane is someone else's pane"
    );
    // a4_pane_replacement_fields — a recreated pane keeps nothing
    // stable.
    let new_terminal = Vec::from([occupied(
        "w6:p9",
        "term-9",
        "kind-1",
        "gov-deadbeef",
        Some("sess-9"),
        None,
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &new_terminal),
        Observation::Absent,
        "F3 — a new terminal_id in the Run's pane is someone else's pane"
    );
}

#[test]
fn f3_dropped_session_means_absent() {
    // a4_native_replace_new_session — the occupant's session vanished.
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        None,
        Some(ChildStatus::Idle),
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Absent,
        "F3 — a pane that no longer reports the captured session is not the Run"
    );
}

#[test]
fn f3_sessionless_identity_ignores_reported_session() {
    // F2 — before the session is captured the four parts match, and
    // the Unique carries the session Herdr now reports for capture.
    let identity = child("w6:p9", None);
    let inc = HerdrIncarnation("inc-1".into());
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-1"),
        None,
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Unique {
            status: None,
            pane: PaneId("w6:p9".into()),
            native_session: Some(NativeSession("sess-1".into())),
        },
        "F3 — a pre-capture identity matches and reports the new session"
    );
}

#[test]
fn f3_every_identity_field_is_required() {
    // One differing field at a time — no single part of the captured
    // identity may be dropped from the match.
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-1".into());
    for (label, row) in [
        (
            "terminal",
            occupied(
                "w6:p9",
                "term-9",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
        ),
        (
            "kind",
            occupied(
                "w6:p9",
                "term-1",
                "kind-9",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
        ),
        (
            "name",
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-ffffffff",
                Some("sess-1"),
                None,
            ),
        ),
        (
            "session",
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-9"),
                None,
            ),
        ),
        (
            "missing kind field",
            agent_row(
                "w6:p9",
                "term-1",
                None,
                Some("gov-deadbeef"),
                Some("sess-1"),
                None,
            ),
        ),
        (
            "missing name field",
            agent_row(
                "w6:p9",
                "term-1",
                Some("kind-1"),
                None,
                Some("sess-1"),
                None,
            ),
        ),
    ] {
        let agents = Vec::from([row]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Absent,
            "F3 — a pane differing only in {label} is not the Run"
        );
    }
}

#[test]
fn f3_ambiguous_incarnation_is_invalid() {
    let identity = child("w6:p9", Some("sess-1"));
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-1"),
        None,
    )]);
    assert_eq!(
        classify(&identity, None, &agents),
        Observation::Invalid,
        "F3 — a read ambiguous about the incarnation is invalid"
    );
}

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

#[test]
fn f3_foreign_incarnation_reproves_by_native_session() {
    // F28/A4 — after a discontinuity the unique session re-proves
    // identity; the bare ids are untrusted.
    let identity = child("w6:p9", Some("sess-1"));
    let inc = HerdrIncarnation("inc-2".into());
    let agents = Vec::from([
        occupied(
            "w9:p2",
            "term-9",
            "kind-2",
            "gov-deadbeef",
            Some("sess-1"),
            Some(ChildStatus::Idle),
        ),
        occupied("w6:p9", "term-1", "kind-1", "gov-deadbeef", None, None),
    ]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Unique {
            status: Some(ChildStatus::Idle),
            pane: PaneId("w9:p2".into()),
            native_session: Some(NativeSession("sess-1".into())),
        },
        "F3 — the session re-proves identity across an incarnation change"
    );
    assert_eq!(
        classify(&identity, Some(&inc), &agents[1..]),
        Observation::Absent,
        "F3 — without the session the Run is absent under the new incarnation"
    );
}

#[test]
fn f3_foreign_incarnation_sessionless_is_invalid_never_absent() {
    // F28 — a Run without a native session cannot re-prove itself; the
    // observation is ambiguous (invalid), never absence.
    let identity = child("w6:p9", None);
    let inc = HerdrIncarnation("inc-2".into());
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        None,
        None,
    )]);
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Invalid,
        "F3 — a sessionless identity under a foreign incarnation is unprovable"
    );
}

#[test]
fn f3_invalid_never_counts_as_absent() {
    let inc = HerdrIncarnation("inc-1".into());
    let other = HerdrIncarnation("inc-2".into());
    let sessionful = child("w6:p9", Some("sess-1"));
    let sessionless = child("w6:p9", None);
    let good = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-1"),
        None,
    )]);
    let dup_locator = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
        occupied("w6:p9", "term-2", "kind-2", "other", None, None),
    ]);
    let dup_match = Vec::from([
        occupied(
            "w6:p1",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
        occupied(
            "w6:p2",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        ),
    ]);
    let cases: Vec<(ChildIdentity, Option<&HerdrIncarnation>, Vec<AgentRow>)> = Vec::from([
        (sessionful.clone(), None, good.clone()),
        (sessionful.clone(), Some(&inc), dup_locator),
        (sessionful.clone(), Some(&inc), dup_match),
        (sessionless.clone(), Some(&other), Vec::new()),
    ]);
    for (identity, incarnation, agents) in cases {
        let observation = classify(&identity, incarnation, &agents);
        assert_eq!(
            observation.class(),
            ObservationClass::Invalid,
            "F3 — invalid never counts as absence (H#74)"
        );
    }
}
