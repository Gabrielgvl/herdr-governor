//! Test fixtures for the recovery module — constructed inputs, no I/O.

use alloc::vec::Vec;
use core::time::Duration;

use crate::config::{Policy, Tier};
use crate::identity::{
    AgentKind, CallerKey, ChildStatus, LaunchId, NativeSession, Observation, PaneId, RunId,
    Timestamp,
};
use crate::lifecycle::{Run, Settlement, State};
use crate::recovery::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
use crate::task::{Refusal, Task};

pub(super) fn run(id: &str, owner: &CallerKey, settlement: Option<Settlement>) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId("launch-1".into()),
        owner: owner.clone(),
        owner_generation: 0,
        version: 3,
        state: State::Settled,
        prompt_certainty: None,
        child_name: "gov-deadbeef".into(),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/proj".into(),
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
        max_age_deadline: Timestamp(86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement,
        settled_at: settlement.map(|_| Timestamp(1_000)),
    }
}

pub(super) fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-a".into()),
    }
}

pub(super) fn policy() -> Policy {
    Policy {
        tiers: Vec::from([Tier("t0".into()), Tier("t1".into()), Tier("t2".into())]),
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.7,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

pub(super) fn task() -> Task {
    Task {
        objective: "do the thing".into(),
        scope: "the repo".into(),
        done_when: Vec::from(["it works".into()]),
        constraints: Vec::from(["stay quiet".into()]),
        tier: None,
        recovery_of: None,
        label: Some("lbl".into()),
        cwd: Some("/proj".into()),
        retention: None,
    }
}

pub(super) fn obligation(status: RecoveryStatus) -> RecoveryObligation {
    RecoveryObligation {
        predecessor: RunId("run-1".into()),
        origin: RecoveryOrigin::ProviderLimit,
        status,
        reason: None,
        successor_launch: match status {
            RecoveryStatus::Dispatched => Some(LaunchId("launch-9".into())),
            RecoveryStatus::Pending | RecoveryStatus::Blocked | RecoveryStatus::Failed => None,
        },
        expires_at: Timestamp(86_400_000),
    }
}

pub(super) fn unique(status: Option<ChildStatus>) -> Observation {
    Observation::Unique {
        status,
        pane: PaneId("w6:p1".into()),
        native_session: Some(NativeSession("sess-1".into())),
    }
}

/// `caller_admission`'s `Ok(_)` case, asserted into the obligation — an
/// `Err` means the gate that should have admitted did not.
pub(super) fn admitted(result: Result<RecoveryObligation, Refusal>) -> RecoveryObligation {
    match result {
        Ok(obligation) => obligation,
        Err(refusal) => panic!("expected the recovery to be admitted, not {refusal:?}"),
    }
}
