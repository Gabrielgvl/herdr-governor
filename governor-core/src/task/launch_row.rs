//! F11/F13/Appendix B — the Launch-row transactions as pure `Transition`
//! values: Admit (`evaluating` + `jev_evaluate`), Route (decision + Run),
//! `routed`→`launching`, terminal `done`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{
    CallerKey, DedupKey, EffectId, EffectKey, EventId, IdempotencyKey, LaunchId, ProjectRoot,
    Timestamp, mint_agent_name,
};
use crate::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectState, EffectWrite, Run, Settlement, State,
    StateChange, Transition, UnresolvedReason, nothing, settle,
};
use crate::routing::Decision;

use super::LaunchOutcome::{Abstained, Failed, Launched, Rejected};
use super::LaunchPhase::{Done, Evaluating, Launching, Routed};
use super::{Launch, LaunchOutcome, LaunchPhase, Task};

/// F11 — a fresh `evaluating` Launch row; `task_digest` canonical.
#[must_use]
pub fn new_launch(
    launch_id: LaunchId,
    caller: CallerKey,
    project_root: ProjectRoot,
    key: IdempotencyKey,
    task: Task,
) -> Launch {
    Launch {
        id: launch_id,
        caller,
        project_root,
        idempotency_key: key,
        digest_version: Task::DIGEST_VERSION,
        task_digest: task.digest(),
        task,
        phase: Evaluating,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

/// The `launch:<id>:evaluate` key `admit`/`finish` share (OQ-1).
fn eval_key(launch: &Launch) -> EffectKey {
    EffectKey(format!("launch:{}:evaluate", launch.id.0))
}

/// A launch-bound event; `dedup_key = launch:<id>:<kind>` (F18).
fn launch_event(launch: &Launch, kind: MailboxEventKind, body: String) -> MailboxEvent {
    let dedup_key = DedupKey(format!("launch:{}:{}", launch.id.0, kind.as_str()));
    MailboxEvent {
        id: EventId(format!("evt:{}", dedup_key.0)),
        dedup_key,
        subject: MailboxSubject::Launch(launch.id.clone()),
        kind,
        body,
    }
}

/// F11/Appendix B "Admit Launch" — the `evaluating` row plus the planned
/// `jev_evaluate` effect, atomically. Nothing unless `launch.phase` is
/// `Evaluating` with no outcome/decision/`config_version` yet. The effect's
/// `payload_digest` stays `None` — not a pure function of the Task (OQ-15).
#[must_use]
pub fn admit(launch: &Launch) -> Transition {
    if launch.phase != Evaluating
        || launch.outcome.is_some()
        || launch.decision.is_some()
        || launch.config_version.is_some()
    {
        return nothing();
    }
    let key = eval_key(launch);
    let eval = Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind: EffectKind::JevEvaluate,
        subject_launch: Some(launch.id.clone()),
        subject_run: None,
        target: None,
        payload_digest: None,
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    };
    Transition {
        state_changes: Vec::from([StateChange::RecordLaunch(launch.clone())]),
        events: Vec::new(),
        effects: Vec::from([eval]),
    }
}

/// F13/Appendix B "Route" — persist `decision` + `config_version`, phase
/// `routed`, and reserve `run`, atomically. Nothing unless `launch.phase`
/// is `Evaluating`, `decision.candidates` is non-empty and `run` is the
/// `reserved_run` shape: `Reserved`, bound here, minted `child_name`,
/// `max_age_deadline` set, unsettled.
#[must_use]
pub fn decided(launch: &Launch, decision: &Decision, run: &Run) -> Transition {
    if launch.phase != Evaluating
        || decision.candidates.is_empty()
        || run.launch != launch.id
        || run.state != State::Reserved
        || run.child_name != mint_agent_name(&run.id).0
        || run.max_age_deadline == Timestamp(0)
        || run.settlement.is_some()
    {
        return nothing();
    }
    let record = Launch {
        phase: Routed,
        decision: Some(decision.clone()),
        config_version: Some(decision.config_version.clone()),
        ..launch.clone()
    };
    Transition {
        state_changes: Vec::from([
            StateChange::RecordLaunch(record),
            StateChange::ReserveRun(run.clone()),
        ]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// Phase `routed` → `launching`, concatenated with `launch_plan`'s
/// `Transition` in one transaction. Nothing unless `phase` is `Routed`; the
/// decision and `config_version` ride unchanged (F13-immutable).
#[must_use]
pub fn begin(launch: &Launch) -> Transition {
    if launch.phase != Routed {
        return nothing();
    }
    let record = Launch {
        phase: Launching,
        ..launch.clone()
    };
    Transition {
        state_changes: Vec::from([StateChange::RecordLaunch(record)]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// Phase → `done` with `outcome`, terminal (F5/OQ-2 matrix):
///
/// - `evaluating` → `Abstained` | `Rejected` (`Launched` impossible; a
///   pre-decision failure is the `evaluation_failed` abstention).
/// - `routed` → `Abstained` | `Failed{certainty: Absent}` — ends before
///   any effect ran. Requires the Run `decided` reserved (`run.launch ==
///   launch.id`, `Reserved`, unsettled) and composes `settle(reserved,
///   Unresolved{LaunchNotStarted})` in the same `Transition` (Appendix C
///   `reserved` × "launch abstains or fails before any effect"). Other
///   phases ignore `reserved`.
/// - `launching` → `Launched` | `Abstained` | `Failed`.
/// - `done` absorbing — any call is `nothing()` (F20-immutable).
///
/// Every terminal write emits `launch_answered` (OQ-12 F18 amendment);
/// `Failed` also emits `launch_failed`. An `evaluating` `Abstained` writes
/// the stranded `launch:<id>:evaluate` row (OQ-13): `planned` → `failed`/
/// `absent`; `dispatching`/`unconfirmed` → `unknown`; else none.
#[must_use]
pub fn finish(
    launch: &Launch,
    outcome: LaunchOutcome,
    eval_effect_state: Option<EffectState>,
    reserved: Option<&Run>,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    if !matches!(
        (&launch.phase, &outcome),
        (Evaluating, Abstained { .. } | Rejected)
            | (
                Routed,
                Abstained { .. }
                    | Failed {
                        certainty: EffectCertainty::Absent,
                        ..
                    },
            )
            | (
                Launching,
                Launched { .. } | Abstained { .. } | Failed { .. }
            )
    ) {
        return nothing();
    }
    // A `routed` terminal must settle the Run `decided` reserved —
    // verified before any write is shaped so a missing, mismatched,
    // non-`reserved` or already-settled Run commits nothing.
    let settled = if launch.phase == Routed {
        match reserved {
            Some(run)
                if run.launch == launch.id
                    && run.state == State::Reserved
                    && run.settlement.is_none() =>
            {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::LaunchNotStarted,
                    },
                    now,
                    policy,
                )
            }
            _ => return nothing(),
        }
    } else {
        nothing()
    };
    let stranded = stranded_eval_certainty(launch.phase, &outcome, eval_effect_state);
    let mut events = Vec::from([launch_event(
        launch,
        MailboxEventKind::LaunchAnswered,
        answered_body(&outcome),
    )]);
    if let Failed { certainty, .. } = &outcome {
        events.push(launch_event(
            launch,
            MailboxEventKind::LaunchFailed,
            format!(
                "{{\"outcome\":\"failed\",\"certainty\":\"{}\"}}",
                certainty.as_str()
            ),
        ));
    }
    let mut state_changes = Vec::from([StateChange::RecordLaunch(Launch {
        phase: Done,
        outcome: Some(outcome),
        ..launch.clone()
    })]);
    if let Some(certainty) = stranded {
        state_changes.push(StateChange::WriteEffect(EffectWrite {
            key: eval_key(launch),
            state: EffectState::Failed,
            certainty: Some(certainty),
            receipt: None,
        }));
    }
    state_changes.extend(settled.state_changes);
    events.extend(settled.events);
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}

/// OQ-13 — the stranded write's honest certainty: `planned` → `absent`;
/// `dispatching`/`unconfirmed` → `unknown`; terminal/missing → no write.
/// Only an `evaluating` `Abstained` asks.
fn stranded_eval_certainty(
    phase: LaunchPhase,
    outcome: &LaunchOutcome,
    eval_effect_state: Option<EffectState>,
) -> Option<EffectCertainty> {
    match (phase, outcome, eval_effect_state) {
        (Evaluating, Abstained { .. }, Some(EffectState::Planned)) => Some(EffectCertainty::Absent),
        (
            Evaluating,
            Abstained { .. },
            Some(EffectState::Dispatching | EffectState::Unconfirmed),
        ) => Some(EffectCertainty::Unknown),
        (_, _, _) => None,
    }
}

/// The `launch_answered` body — outcome spelling + reason (F5/F18).
fn answered_body(outcome: &LaunchOutcome) -> String {
    match outcome.reason_str() {
        Some(reason) => format!(
            "{{\"outcome\":\"{}\",\"reason\":\"{reason}\"}}",
            outcome.as_str()
        ),
        None => format!("{{\"outcome\":\"{}\"}}", outcome.as_str()),
    }
}

/// Constructed inputs for `launch_row_tests` (hosted here by the 500-line cap).
#[cfg(test)]
pub(in crate::task) mod fixtures {
    use alloc::format;
    use alloc::vec::Vec;
    use core::time::Duration;

    use super::finish;
    use crate::config::{ConfigVersion, OperatingPointId, Policy, Provider, Tier};
    use crate::delivery::{MailboxEvent, MailboxEventKind};
    use crate::identity::{
        AgentKind, AgentName, CallerKey, ChildIdentity, HerdrIncarnation, IdempotencyKey, LaunchId,
        NativeSession, PaneId, ProjectRoot, RunId, TerminalId, Timestamp,
    };
    use crate::lifecycle::{
        CreatedTopology, EffectCertainty, EffectState, EffectWrite, Run, StateChange, Transition,
        reserved_run,
    };
    use crate::routing::{Candidate, Decision, Exploration};
    use crate::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase, Task, new_launch};

    pub(in crate::task) const NOW: Timestamp = Timestamp(500);

    pub(in crate::task) fn policy() -> Policy {
        Policy {
            tiers: Vec::from([Tier("t0".into()), Tier("t1".into())]),
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

    pub(in crate::task) fn task() -> Task {
        Task {
            objective: "obj".into(),
            scope: "scope".into(),
            done_when: Vec::from(["done".into()]),
            constraints: Vec::new(),
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
        }
    }

    pub(in crate::task) fn caller() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("caller-kind".into()),
            native_session: NativeSession("caller-sess".into()),
        }
    }

    pub(in crate::task) fn launch() -> Launch {
        new_launch(
            LaunchId("l-1".into()),
            caller(),
            ProjectRoot("/repo".into()),
            IdempotencyKey("key-1".into()),
            task(),
        )
    }

    pub(in crate::task) fn candidate(index: usize) -> Candidate {
        Candidate {
            operating_point: OperatingPointId(format!("op-{index}")),
            provider: Provider(format!("prov-{index}")),
            tier: Tier(format!("t{index}")),
            harness: AgentKind("kind".into()),
            args: Vec::from(["--flag".into()]),
        }
    }

    pub(in crate::task) fn decision(count: usize) -> Decision {
        Decision {
            judged_tier: Tier("t0".into()),
            requested_tier: None,
            policy_cap: None,
            policy_floor: None,
            caller_uplift: None,
            recovery_minimum: None,
            exploration: Exploration {
                assigned: false,
                executed: false,
            },
            start_tier: Tier("t0".into()),
            candidates: (0..count).map(candidate).collect(),
            config_version: ConfigVersion("cfg-1".into()),
        }
    }

    pub(in crate::task) fn reserved(launch: &Launch) -> Run {
        reserved_run(
            launch,
            RunId("r-1".into()),
            "/repo".into(),
            None,
            NOW,
            &policy(),
        )
    }

    /// A Launch shaped as `phase` (routed+ carry decision/config; `done`
    /// a `launched` outcome).
    pub(in crate::task) fn phased(phase: LaunchPhase) -> Launch {
        Launch {
            phase,
            decision: (phase != LaunchPhase::Evaluating).then(|| decision(1)),
            config_version: (phase != LaunchPhase::Evaluating)
                .then(|| ConfigVersion("cfg-1".into())),
            outcome: (phase == LaunchPhase::Done).then(launched),
            ..launch()
        }
    }

    pub(in crate::task) fn identity() -> ChildIdentity {
        ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: AgentKind("kind-1".into()),
            agent_name: AgentName("gov-00000001".into()),
            native_session: Some(NativeSession("sess-1".into())),
            pane_id: PaneId("w0:p1".into()),
        }
    }

    pub(in crate::task) fn abstain(reason: AbstainReason) -> LaunchOutcome {
        LaunchOutcome::Abstained { reason }
    }

    pub(in crate::task) fn failed(certainty: EffectCertainty) -> LaunchOutcome {
        LaunchOutcome::Failed {
            certainty,
            run: Some(RunId("r-1".into())),
            created_topology: CreatedTopology {
                tab: None,
                panes: Vec::new(),
            },
        }
    }

    pub(in crate::task) fn launched() -> LaunchOutcome {
        LaunchOutcome::Launched {
            run: RunId("r-1".into()),
            operating_point: OperatingPointId("op-0".into()),
            requested_operating_point: None,
            tier_evidence: decision(1),
        }
    }

    pub(in crate::task) fn quiet(t: &Transition) -> bool {
        t.state_changes.is_empty() && t.events.is_empty() && t.effects.is_empty()
    }

    /// The single `RecordLaunch` a builder emits — panics otherwise.
    pub(in crate::task) fn recorded(t: &Transition) -> &Launch {
        let records: Vec<&Launch> = t
            .state_changes
            .iter()
            .filter_map(|change| {
                let StateChange::RecordLaunch(launch) = change else {
                    return None;
                };
                Some(launch)
            })
            .collect();
        assert_eq!(records.len(), 1, "exactly one RecordLaunch expected");
        records[0]
    }

    pub(in crate::task) fn writes(t: &Transition) -> Vec<&EffectWrite> {
        t.state_changes
            .iter()
            .filter_map(|change| {
                let StateChange::WriteEffect(write) = change else {
                    return None;
                };
                Some(write)
            })
            .collect()
    }

    pub(in crate::task) fn events(t: &Transition, kind: MailboxEventKind) -> Vec<&MailboxEvent> {
        t.events.iter().filter(|event| event.kind == kind).collect()
    }

    pub(in crate::task) fn ends(launch: &Launch, outcome: LaunchOutcome) -> Transition {
        finish(launch, outcome, None, None, NOW, &policy())
    }

    /// The certainty a stranded `state` earns, or `None`.
    pub(in crate::task) fn stranded_certainty(
        state: Option<EffectState>,
    ) -> Option<EffectCertainty> {
        writes(&finish(
            &launch(),
            abstain(AbstainReason::InterruptedBeforeDecision),
            state,
            None,
            NOW,
            &policy(),
        ))
        .first()
        .and_then(|write| write.certainty)
    }
}
