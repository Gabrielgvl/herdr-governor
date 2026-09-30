//! F19 — adoption: the previous owner's session must be gone, a settled
//! Run is adoptable only for unread events or a pending recovery, and
//! adoption never reopens a settled Run.

use alloc::vec::Vec;

use super::builders::{key, occupied, owner_change, run};
use crate::identity::{AgentRow, plan_adoption};
use crate::lifecycle::{Settlement, StateChange};
use crate::task::Refusal;

#[test]
fn f19_adopt_while_owner_live_refused() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    let live_owner = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-a"),
        None,
    )]);
    let adopter = key("kind-b", "sess-b");
    assert_eq!(
        plan_adoption(&run, &adopter, &live_owner, false, false),
        Err(Refusal::AdoptOwnerLive),
        "F19 — a live previous owner keeps the Run (use handover)"
    );
}

#[test]
fn f19_owner_session_absent_is_adoptable() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, None);
    // A snapshot where only another session is present: the owner's is
    // gone, so an unsettled Run is adoptable.
    let gone_owner = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-z"),
        None,
    )]);
    let adopter = key("kind-b", "sess-b");
    assert_eq!(
        plan_adoption(&run, &adopter, &gone_owner, false, false),
        Ok(owner_change(&run, adopter)),
        "F19 — the owner's session gone makes an unsettled Run adoptable"
    );
}

#[test]
fn f19_adopt_unsettled_run_writes_owner_change() {
    let owner = key("kind-a", "sess-a");
    let mut run = run(&owner, None, None);
    run.owner_generation = 3;
    let adopter = key("kind-b", "sess-b");
    let agents: Vec<AgentRow> = Vec::new();
    let transition = plan_adoption(&run, &adopter, &agents, false, false);
    let expected = owner_change(&run, adopter);
    assert_eq!(
        transition,
        Ok(expected),
        "F19 — adoption writes the conditional owner change"
    );
    if let Ok(t) = transition {
        assert_eq!(
            t.state_changes.len(),
            1,
            "F19 — one write: the owner change"
        );
        assert_eq!(
            t.events.len(),
            0,
            "F19 — adoption emits no mailbox event by itself"
        );
    }
}

#[test]
fn f19_settled_run_adoptable_only_for_unread_or_recovery() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, Some(Settlement::NoHandoff));
    let adopter = key("kind-b", "sess-b");
    let agents: Vec<AgentRow> = Vec::new();
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, false),
        Err(Refusal::NotOwner),
        "F19 — a settled Run with nothing pending is not adoptable"
    );
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, true, false),
        Ok(owner_change(&run, adopter.clone())),
        "F19 — a settled Run is adopted for its unread events"
    );
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, false, true),
        Ok(owner_change(&run, adopter.clone())),
        "F19 — a settled Run is adopted for a pending recovery"
    );
    assert_eq!(
        plan_adoption(&run, &adopter, &agents, true, true),
        Ok(owner_change(&run, adopter)),
        "F19 — either obligation suffices"
    );
}

#[test]
fn f19_adoption_never_reopens_a_settled_run() {
    let owner = key("kind-a", "sess-a");
    let run = run(&owner, None, Some(Settlement::Rejected));
    let adopter = key("kind-b", "sess-b");
    let agents: Vec<AgentRow> = Vec::new();
    let transition = plan_adoption(&run, &adopter, &agents, true, false);
    match transition {
        Ok(t) => {
            assert_eq!(
                t.state_changes.len(),
                1,
                "F19 — the only write is the owner change"
            );
            for change in &t.state_changes {
                match change {
                    StateChange::ChangeOwner(oc) => {
                        assert_eq!(
                            oc.expected_owner, run.owner,
                            "F19 — conditional on the previous owner"
                        );
                    }
                    StateChange::UpdateRun(_) => panic!(
                        "F19 — adoption must not rewrite the Run record (settlement is immutable)"
                    ),
                    StateChange::BindCaller(_)
                    | StateChange::RecordLaunch(_)
                    | StateChange::ReserveRun(_)
                    | StateChange::WriteEffect(_)
                    | StateChange::RecordFollowUp(_)
                    | StateChange::ExpireFollowUps { .. }
                    | StateChange::RecordRecovery(_)
                    | StateChange::SetCooldown(_)
                    | StateChange::FreezeHandoff(_)
                    | StateChange::AckEvent(_) => {
                        panic!("F19 — adoption writes the owner change only")
                    }
                }
            }
        }
        Err(refusal) => panic!("F19 — adoptable run refused: {refusal:?}"),
    }
}
