//! F4 — the owner-only rule for Run and mailbox operations, plus `handover`;
//! F19 — `adopt`.

use alloc::vec::Vec;

use crate::lifecycle::{OwnerChange, Run, StateChange, Transition};
use crate::task::Refusal;

use super::{AgentRow, CallerKey, PaneId, resolve_caller_key};

/// F4 — every Run and mailbox operation requires the caller to be the
/// current owner: `observe`, `message`, `ack` and `cancel` refuse
/// `NOT_OWNER` otherwise.
pub fn require_owner(run: &Run, caller: &CallerKey) -> Result<(), Refusal> {
    if run.owner == *caller {
        Ok(())
    } else {
        Err(Refusal::NotOwner)
    }
}

/// F4/F6 — plan `handover {runIds, successorPaneId}`: the requester must be
/// the current owner (which makes it live — it just resolved through F1),
/// the successor pane is verified by the F1 caller-resolution rules, and a
/// Run is never handed to its own child — refused `CALLER_IS_RUN` (H#24).
/// The `ChangeOwner` write is conditional on the expected owner and bumps
/// `owner_generation` atomically (Appendix B).
pub fn plan_handover(
    run: &Run,
    caller: &CallerKey,
    successor_pane: &PaneId,
    agents: &[AgentRow],
) -> Result<Transition, Refusal> {
    require_owner(run, caller)?;
    let successor = resolve_caller_key(successor_pane, agents)?;
    if caller_is_run_child(run, &successor, agents) {
        return Err(Refusal::CallerIsRun);
    }
    Ok(owner_transition(run, successor))
}

/// F19 — plan `adopt {runIds}`: a fresh snapshot must show the previous
/// owner's native session gone — still present is refused
/// `ADOPT_OWNER_LIVE`; an unsettled Run is adoptable; a settled Run is
/// adoptable only for its unread events or a pending recovery, and
/// adoption never reopens it — the write changes `owner_caller_id` and
/// `owner_generation` only. A Run is never adopted by its own child —
/// refused `CALLER_IS_RUN` (F4/H#24).
pub fn plan_adoption(
    run: &Run,
    adopter: &CallerKey,
    agents: &[AgentRow],
    has_unread_events: bool,
    has_pending_recovery: bool,
) -> Result<Transition, Refusal> {
    if owner_session_present(run, agents) {
        return Err(Refusal::AdoptOwnerLive);
    }
    if caller_is_run_child(run, adopter, agents) {
        return Err(Refusal::CallerIsRun);
    }
    if run.settlement.is_some() && !has_unread_events && !has_pending_recovery {
        return Err(Refusal::NotOwner);
    }
    Ok(owner_transition(run, adopter.clone()))
}

/// H#24 — whether `caller` is the Run's own child, resolved against the
/// fresh snapshot: a row matching the captured identity's parts (terminal,
/// kind, name — and the captured session once Herdr reported it) that
/// reports `caller`'s session makes that caller the child, so a handover or
/// adoption naming it is refused `CALLER_IS_RUN`. The snapshot's report —
/// not the persisted `native_session` — is what counts: a child whose
/// session arrived on the wire but was never captured is refused all the
/// same.
fn caller_is_run_child(run: &Run, caller: &CallerKey, agents: &[AgentRow]) -> bool {
    let Some(identity) = &run.identity else {
        return false;
    };
    caller.agent_kind == identity.agent_kind
        && agents.iter().any(|row| {
            row.1 == identity.terminal_id
                && row.2.as_ref() == Some(&identity.agent_kind)
                && row.3.as_ref() == Some(&identity.agent_name)
                && row.4.as_ref() == Some(&caller.native_session)
                && match &identity.native_session {
                    Some(session) => row.4.as_ref() == Some(session),
                    None => true,
                }
        })
}

/// F19 — whether a fresh snapshot still shows the Run owner's native
/// session; adoption requires it gone.
fn owner_session_present(run: &Run, agents: &[AgentRow]) -> bool {
    agents
        .iter()
        .any(|row| row.4.as_ref() == Some(&run.owner.native_session))
}

/// Appendix B — the `handover`/`adopt` transaction: `owner_caller_id` and
/// `owner_generation + 1`, conditional on the expected owner.
fn owner_transition(run: &Run, owner: CallerKey) -> Transition {
    Transition {
        state_changes: Vec::from([StateChange::ChangeOwner(OwnerChange {
            run: run.id.clone(),
            expected_owner: run.owner.clone(),
            owner,
        })]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}
