//! Constructed inputs for the identity tests — snapshots, bindings and
//! runs as values, never a live system.

use alloc::string::String;
use alloc::vec::Vec;

use crate::identity::{
    AgentKind, AgentName, AgentRow, CallerEnvelope, CallerKey, ChildIdentity, ChildStatus,
    HerdrIncarnation, LaunchId, NativeSession, PaneId, ProjectRoot, RelayInstanceId, RunId,
    TerminalId, Timestamp,
};
use crate::lifecycle::{OwnerChange, Run, Settlement, State, StateChange, Transition};

pub(super) fn key(kind: &str, session: &str) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind(kind.into()),
        native_session: NativeSession(session.into()),
    }
}

pub(super) const RELAY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const RELAY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub(super) const RELAY_C: &str = "cccccccccccccccccccccccccccccccc";

pub(super) fn envelope(pane: &str, root: &str, relay: &str) -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId(pane.into()),
        project_root: ProjectRoot(root.into()),
        relay_instance_id: RelayInstanceId(relay.into()),
    }
}

pub(super) fn agent_row(
    pane: &str,
    terminal: &str,
    kind: Option<&str>,
    name: Option<&str>,
    session: Option<&str>,
    status: Option<ChildStatus>,
) -> AgentRow {
    (
        PaneId(pane.into()),
        TerminalId(terminal.into()),
        kind.map(|k| AgentKind(k.into())),
        name.map(|n| AgentName(n.into())),
        session.map(|s| NativeSession(s.into())),
        status,
    )
}

pub(super) fn occupied(
    pane: &str,
    terminal: &str,
    kind: &str,
    name: &str,
    session: Option<&str>,
    status: Option<ChildStatus>,
) -> AgentRow {
    agent_row(pane, terminal, Some(kind), Some(name), session, status)
}

pub(super) fn child(pane: &str, session: Option<&str>) -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-1".into()),
        agent_name: AgentName("gov-deadbeef".into()),
        native_session: session.map(|s| NativeSession(s.into())),
        pane_id: PaneId(pane.into()),
    }
}

pub(super) fn run(
    owner: &CallerKey,
    identity: Option<ChildIdentity>,
    settlement: Option<Settlement>,
) -> Run {
    Run {
        id: RunId("018f3c2a-7b1d-7e90-8abc-0123456789ab".into()),
        launch: LaunchId("launch-1".into()),
        owner: owner.clone(),
        owner_generation: 0,
        version: 0,
        state: State::Active,
        prompt_certainty: None,
        child_name: String::from("gov-deadbeef"),
        identity,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: String::from("/work"),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(1_800_000_000_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement,
        settled_at: settlement.map(|_| Timestamp(1_700_000_000_000)),
    }
}

pub(super) fn owner_change(run: &Run, owner: CallerKey) -> Transition {
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
