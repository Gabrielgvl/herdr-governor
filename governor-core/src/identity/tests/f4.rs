//! F4 — ownership: `require_owner`, `handover` and the never-own-caller
//! rule (H#24).

use alloc::vec::Vec;

use super::builders::{child, key, occupied, owner_change, run};
use crate::identity::{PaneId, plan_adoption, plan_handover, require_owner};
use crate::task::Refusal;

// ---- F4 — ownership ----------------------------------------------------

#[test]
fn f4_observe_message_ack_cancel_refuse_not_owner() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    assert_eq!(
        require_owner(&run, &owner),
        Ok(()),
        "F4 — the current owner passes the check"
    );
    assert_eq!(
        require_owner(&run, &key("kind-a", "sess-b")),
        Err(Refusal::NotOwner),
        "F4 — any other caller is NOT_OWNER"
    );
}

#[test]
fn f4_handover_changes_owner_conditionally() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    let agents = Vec::from([occupied(
        "w6:p7",
        "t7",
        "kind-b",
        "successor",
        Some("sess-b"),
        None,
    )]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — handover writes the conditional owner change (owner_generation bumps)"
    );
}

#[test]
fn f4_handover_requires_the_current_owner() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    let agents = Vec::from([occupied(
        "w6:p7",
        "t7",
        "kind-b",
        "successor",
        Some("sess-b"),
        None,
    )]);
    assert_eq!(
        plan_handover(
            &run,
            &key("kind-a", "sess-x"),
            &PaneId("w6:p7".into()),
            &agents
        ),
        Err(Refusal::NotOwner),
        "F4 — a non-owner cannot hand the Run over"
    );
}

#[test]
fn f4_handover_verifies_successor_by_f1() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    let agents = Vec::from([occupied("w6:p7", "t7", "kind-b", "successor", None, None)]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
        Err(Refusal::CallerIdentitySessionless),
        "F4/F6 — a sessionless successor pane is refused by the F1 rules"
    );
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p0".into()), &agents),
        Err(Refusal::CallerIdentityMissing),
        "F4/F6 — a successor pane that resolves to nothing is refused"
    );
}

#[test]
fn f4_run_cannot_be_its_own_caller() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", Some("sess-child"))), None);
    let agents = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        ),
        occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
    ]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p9".into()), &agents),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — a handover to the Run's own child is CALLER_IS_RUN"
    );
    // A child key that differs only in kind is still that child's
    // caller.
    let agents_kind = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-child"),
        None,
    )]);
    assert_eq!(
        plan_adoption(
            &run,
            &key("kind-1", "sess-child"),
            &agents_kind,
            false,
            false
        ),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — the Run can never become its own caller via adopt either"
    );
}

#[test]
fn f4_child_without_session_cannot_be_self_named() {
    // With no captured session there is no child caller key to
    // forbid — handover proceeds on the verified successor.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p7",
        "t7",
        "kind-b",
        "successor",
        Some("sess-b"),
        None,
    )]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — a sessionless child has no caller key to collide with"
    );
}

#[test]
fn f4_sessionless_child_cannot_be_handed_to_itself() {
    // The persisted identity has not captured a session yet, but the fresh
    // snapshot already reports one for the child — the self-ownership check
    // resolves the child from the snapshot, so handing the Run to the
    // child's own pane is CALLER_IS_RUN all the same (H#24).
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        ),
        occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
    ]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p9".into()), &agents),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — the snapshot's session report names the successor the Run's child"
    );
    // an unrelated successor is still allowed.
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — an unrelated successor is never the child"
    );
}

#[test]
fn f4_sessionless_child_cannot_adopt_itself() {
    // Same gap through `adopt`: the owner's session is gone from the
    // snapshot (no ADOPT_OWNER_LIVE), the child's row reports the session
    // the capture never persisted — adopting as that key is still
    // CALLER_IS_RUN.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([
        occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        ),
        occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
    ]);
    assert_eq!(
        plan_adoption(&run, &key("kind-1", "sess-child"), &agents, false, false),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — the Run's child cannot adopt it, session captured or not"
    );
    // an unrelated adopter is still allowed.
    assert_eq!(
        plan_adoption(&run, &key("kind-b", "sess-b"), &agents, false, false),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — an unrelated adopter is never the child"
    );
}

#[test]
fn f4_reincarnated_child_cannot_be_handed_to_itself() {
    // F28/A4 — after a Herdr discontinuity the child re-proves by native
    // session alone: new incarnation, new terminal, new pane. The captured
    // session is still its caller key, so handing the Run to the child's
    // re-incarnated pane is CALLER_IS_RUN even though no row matches the
    // captured terminal (H#24).
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", Some("sess-child"))), None);
    let agents = Vec::from([
        occupied(
            "w6:p3",
            "term-2",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        ),
        occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
    ]);
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p3".into()), &agents),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — the captured session re-proves the child across the incarnation change"
    );
    // an unrelated successor is still allowed.
    assert_eq!(
        plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — an unrelated successor is never the child"
    );
}

#[test]
fn f4_reincarnated_child_cannot_adopt_itself() {
    // Same gap through `adopt`: the owner's session is gone from the
    // snapshot (no ADOPT_OWNER_LIVE) and the child's re-incarnated row sits
    // on a terminal the captured identity never named — adopting under the
    // re-proven key is still CALLER_IS_RUN.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", Some("sess-child"))), None);
    let agents = Vec::from([
        occupied(
            "w6:p3",
            "term-2",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        ),
        occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
    ]);
    assert_eq!(
        plan_adoption(&run, &key("kind-1", "sess-child"), &agents, false, false),
        Err(Refusal::CallerIsRun),
        "F4/H#24 — the Run's child cannot adopt it after re-proving under a new incarnation"
    );
    // an unrelated adopter is still allowed.
    assert_eq!(
        plan_adoption(&run, &key("kind-b", "sess-b"), &agents, false, false),
        Ok(owner_change(&run, key("kind-b", "sess-b"))),
        "F4 — an unrelated adopter is never the child"
    );
}

// ---- H#24 — every conjunct of the child check is individually required ----
// The persisted identity is sessionless, so the snapshot resolution below
// runs: each test reports the caller's session on a row matching the
// captured terminal, kind and name — except for the one field under test.
// The caller must be allowed through, so a `&&` weakened to `||` in
// `caller_is_run_child` surfaces as a spurious CALLER_IS_RUN.

#[test]
fn f4_caller_is_run_child_requires_caller_kind() {
    // The row matches the identity's terminal, kind, name and the
    // caller's session, but the caller's kind differs from the captured
    // agent_kind — not the child.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-child"),
        None,
    )]);
    let adopter = key("kind-2", "sess-child");
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Ok(owner_change(&run, adopter)),
        "F4/H#24 — a caller of another kind is not the Run's child"
    );
}

#[test]
fn f4_caller_is_run_child_requires_terminal() {
    // The row reports the caller's kind and session and the identity's
    // kind and name, but sits on another terminal — not the child.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-other",
        "kind-1",
        "gov-deadbeef",
        Some("sess-child"),
        None,
    )]);
    let adopter = key("kind-1", "sess-child");
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Ok(owner_change(&run, adopter)),
        "F4/H#24 — a row on another terminal is not the Run's child"
    );
}

#[test]
fn f4_caller_is_run_child_requires_row_kind() {
    // The row's terminal, name and session match but its occupant kind
    // differs from the captured agent_kind — not the child.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-2",
        "gov-deadbeef",
        Some("sess-child"),
        None,
    )]);
    let adopter = key("kind-1", "sess-child");
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Ok(owner_change(&run, adopter)),
        "F4/H#24 — a row occupied by another kind is not the Run's child"
    );
}

#[test]
fn f4_caller_is_run_child_requires_row_name() {
    // The row's terminal, kind and session match but its occupant name
    // differs from the captured agent_name — not the child.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-other",
        Some("sess-child"),
        None,
    )]);
    let adopter = key("kind-1", "sess-child");
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Ok(owner_change(&run, adopter)),
        "F4/H#24 — a row occupied by another name is not the Run's child"
    );
}

#[test]
fn f4_caller_is_run_child_requires_row_session() {
    // The row matches terminal, kind and name, but the session it reports
    // is not the caller's — the caller never reached this child.
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, Some(child("w6:p9", None)), None);
    let agents = Vec::from([occupied(
        "w6:p9",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        Some("sess-x"),
        None,
    )]);
    let adopter = key("kind-1", "sess-child");
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Ok(owner_change(&run, adopter)),
        "F4/H#24 — a row reporting another session is not the caller"
    );
}
