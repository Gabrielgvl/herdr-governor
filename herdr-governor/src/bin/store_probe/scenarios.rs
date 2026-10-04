//! The canned crash scenarios (P4.S4): the 12 named Appendix-B
//! transactions, the four composed launch scenarios, and the two P4.1
//! follow-up edges (`follow_up`). Every
//! `Transition` is built through the public core API — P4.0's launch-row
//! builders, `launch_plan`, `settle` and constructed values — never SQL,
//! so the matrix exercises the real writers' statement lists. A scenario
//! is a `seed` (the transitions that put the store in the pre-state, each
//! applied without crash injection) and the `target` the probe kills
//! mid-apply.

mod follow_up;

use core::time::Duration;

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{ConfigVersion, OperatingPointId, Policy, Provider, Tier};
use governor_core::delivery::{FollowUpWrite, MessageBody, OutboxMessage, OutboxState};
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, LaunchId, MessageKey, NativeSession, PaneId, ProjectRoot,
    RelayInstanceId, RunId, TerminalId, Timestamp, mint_agent_name,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectReceipt, EffectResolution, EffectState, EffectWrite, OwnerChange,
    Run, RunUpdate, Settlement, State, StateChange, Transition, launch_plan, reserved_run, settle,
};
use governor_core::recovery::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
use governor_core::routing::{Candidate, Decision, Exploration, PlacementPlan};
use governor_core::task::{
    AbstainReason, Launch, LaunchOutcome, LaunchPhase, Task, admit, begin, decided, finish,
    new_launch,
};

/// The store-stamped time every probe write uses — a constant so two
/// processes produce byte-identical rows.
pub const NOW: Timestamp = Timestamp(1_790_812_800_000);

/// The scenario called `name` as `(seed, target)` — the pre-state
/// transitions (applied in order, crash injection off) and the
/// transaction the probe aborts inside — or `None` for an unknown name.
#[must_use]
pub fn scenario(name: &str) -> Option<(Vec<Transition>, Transition)> {
    let (seed, target) = match name {
        "bind_caller" => (vec![], bind(1)),
        "admit_launch" => (vec![bind(1)], admit_launch()),
        "route" => (vec![bind(1), admit_launch()], route()),
        "plan_effect" => (routed(), plan_effect()),
        "dispatch_effect" => (
            chain(routed(), [plan_effect()]),
            dispatch(agent_effect().key),
        ),
        "effect_result" => (
            chain(routed(), [plan_effect(), dispatch(agent_effect().key)]),
            effect_result(),
        ),
        "enqueue_follow_up" => (routed(), enqueue_follow_up()),
        "dispatch_follow_up" => (follow_up::dispatch_seed(), follow_up::dispatch_follow_up()),
        "resolve_follow_up" => (follow_up::resolve_seed(), follow_up::resolve_follow_up()),
        "settle" => (chain(started(), [enqueue_follow_up()]), settle_limited()),
        "handover" => (
            vec![bind(1), bind(2), admit_launch(), route()],
            changes(vec![StateChange::ChangeOwner(OwnerChange {
                run: run_reserved().id,
                expected_owner: caller(1),
                owner: caller(2),
            })]),
        ),
        "recovery_dispatch" => (
            chain(started(), [enqueue_follow_up(), settle_limited()]),
            recovery_dispatch(),
        ),
        "freeze_handoff" => (routed(), freeze_handoff()),
        "record_evidence" => (routed(), record_evidence()),
        "launch_begin" => (routed(), launch_begin()),
        "launch_finish_done" => (
            chain(routed(), [launch_begin(), dispatch(tab_effect_key())]),
            launch_finish_done(),
        ),
        "launch_finish_routed_abandon" => (
            routed(),
            finish(
                &launch_routed(),
                abstained(),
                None,
                Some(&run_reserved()),
                NOW,
                &policy(),
            ),
        ),
        "launch_finish_abstain" => (
            vec![bind(1), admit_launch()],
            finish(
                &launch_evaluating(),
                abstained(),
                Some(EffectState::Planned),
                None,
                NOW,
                &policy(),
            ),
        ),
        _ => return None,
    };
    Some((seed, target))
}

/// `seed` followed by `more`.
fn chain<const N: usize>(mut seed: Vec<Transition>, more: [Transition; N]) -> Vec<Transition> {
    seed.extend(more);
    seed
}

/// Caller 1 bound, `l-1` admitted and routed: Run `r-1` reserved.
fn routed() -> Vec<Transition> {
    vec![bind(1), admit_launch(), route()]
}

/// [`routed`] plus the `agent_start` effect planned, dispatched and
/// acknowledged — the Run carries its identity and provider.
fn started() -> Vec<Transition> {
    chain(
        routed(),
        [plan_effect(), dispatch(agent_effect().key), effect_result()],
    )
}

fn changes(state_changes: Vec<StateChange>) -> Transition {
    Transition {
        state_changes,
        events: vec![],
        effects: vec![],
    }
}

/// `a` then `b` in one transaction — the coordinator's composition
/// (plan §P4.S4: vec concat; the probe needs no `Transition::then`).
fn concat(mut a: Transition, b: Transition) -> Transition {
    a.state_changes.extend(b.state_changes);
    a.events.extend(b.events);
    a.effects.extend(b.effects);
    a
}

fn policy() -> Policy {
    Policy {
        tiers: vec![Tier("t0".into()), Tier("t1".into())],
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

fn caller(n: u8) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession(format!("sess-{n}")),
    }
}

fn bind(n: u8) -> Transition {
    changes(vec![StateChange::BindCaller(CallerBinding {
        caller: caller(n),
        relay_instance: RelayInstanceId(format!("relay-{n}")),
        pane_at_bind: PaneId("pane-1".into()),
    })])
}

fn task() -> Task {
    Task {
        objective: "o".into(),
        scope: "s".into(),
        done_when: vec!["d".into()],
        constraints: vec![],
        tier: None,
        recovery_of: None,
        label: None,
        cwd: None,
        retention: None,
    }
}

fn launch_with_id(id: &str) -> Launch {
    new_launch(
        LaunchId(id.into()),
        caller(1),
        ProjectRoot("/p".into()),
        IdempotencyKey(format!("key-{id}")),
        task(),
    )
}

fn launch_evaluating() -> Launch {
    launch_with_id("l-1")
}

fn launch_routed() -> Launch {
    Launch {
        phase: LaunchPhase::Routed,
        decision: Some(decision()),
        config_version: Some(decision().config_version),
        ..launch_evaluating()
    }
}

fn launch_launching() -> Launch {
    Launch {
        phase: LaunchPhase::Launching,
        ..launch_routed()
    }
}

fn decision() -> Decision {
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
        candidates: vec![Candidate {
            operating_point: OperatingPointId("op-0".into()),
            provider: Provider("prov-0".into()),
            tier: Tier("t0".into()),
            harness: AgentKind("kind".into()),
            args: vec!["--flag".into()],
        }],
        config_version: ConfigVersion("cfg-1".into()),
    }
}

fn abstained() -> LaunchOutcome {
    LaunchOutcome::Abstained {
        reason: AbstainReason::EvaluationFailed,
    }
}

fn run_reserved() -> Run {
    reserved_run(
        &launch_evaluating(),
        RunId("r-1".into()),
        "/p".into(),
        None,
        NOW,
        &policy(),
    )
}

fn identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind".into()),
        agent_name: AgentName(mint_agent_name(&run_reserved().id).0),
        native_session: None,
        pane_id: PaneId("pane-2".into()),
    }
}

/// The reserved Run after the Appendix-B "Effect result" write: version 1,
/// `starting`, identity and provider known.
fn run_started() -> Run {
    Run {
        version: 1,
        state: State::Starting,
        identity: Some(identity()),
        operating_point: Some(OperatingPointId("op-0".into())),
        provider: Some(Provider("prov-0".into())),
        tier_start: Some(Tier("t0".into())),
        ..run_reserved()
    }
}

fn update_run(expected_version: u64, record: Run) -> StateChange {
    StateChange::UpdateRun(RunUpdate {
        expected_version,
        record,
    })
}

fn admit_launch() -> Transition {
    admit(&launch_evaluating())
}

fn route() -> Transition {
    decided(&launch_evaluating(), &decision(), &run_reserved())
}

fn agent_effect() -> Effect {
    Effect {
        id: EffectId("eff:run:r-1:agent_start".into()),
        key: EffectKey("run:r-1:agent_start".into()),
        kind: EffectKind::AgentStart,
        subject_launch: None,
        subject_run: Some(run_reserved().id),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// Appendix B "Plan an effect": the effect, `planned`.
fn plan_effect() -> Transition {
    Transition {
        state_changes: vec![],
        events: vec![],
        effects: vec![agent_effect()],
    }
}

/// Appendix B "Dispatch an effect": `planned` → `dispatching`.
fn dispatch(key: EffectKey) -> Transition {
    changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
        key,
    })])
}

/// Appendix B "Effect result": the `acknowledged` result commit.
fn acknowledge(key: EffectKey, receipt: Option<EffectReceipt>) -> StateChange {
    StateChange::WriteEffect(EffectWrite::Result {
        key,
        resolution: EffectResolution::Acknowledged { receipt },
    })
}

/// Appendix B "Effect result": the `agent_start` receipt plus the dependent
/// Run fields, version+1.
fn effect_result() -> Transition {
    changes(vec![
        acknowledge(
            agent_effect().key,
            Some(EffectReceipt::AgentStarted {
                identity: identity(),
            }),
        ),
        update_run(0, run_started()),
    ])
}

/// The one follow-up every outbox scenario moves: `r-1` seq 1, `queued`.
fn follow_up_message() -> OutboxMessage {
    OutboxMessage {
        run: run_reserved().id,
        seq: 1,
        message_key: MessageKey("m-1".into()),
        sender: caller(1),
        body_digest: Digest([0x44; 32]),
        body: MessageBody::Inline("hi".into()),
        state: OutboxState::Queued,
        effect: None,
        expiry_reason: None,
    }
}

/// Appendix B "Enqueue a follow-up": the queued outbox row.
fn enqueue_follow_up() -> Transition {
    changes(vec![StateChange::WriteFollowUp(FollowUpWrite::Enqueue(
        follow_up_message(),
    ))])
}

/// Appendix B "Settle" with `provider_limited`: the conditional run write,
/// the expired follow-ups, the recovery obligation and the cooldown.
fn settle_limited() -> Transition {
    settle(&run_started(), Settlement::ProviderLimited, NOW, &policy())
}

/// Appendix B "Recovery dispatch": the successor Launch's admission plus
/// `pending` → `dispatched` naming it (the FK wants the launch row first).
fn recovery_dispatch() -> Transition {
    let successor = launch_with_id("l-2");
    let mut obligation = RecoveryObligation::pending(
        run_reserved().id,
        RecoveryOrigin::ProviderLimit,
        NOW,
        policy().recovery_expiry,
    );
    obligation.status = RecoveryStatus::Dispatched;
    obligation.successor_launch = Some(successor.id.clone());
    concat(
        admit(&successor),
        changes(vec![StateChange::RecordRecovery(obligation)]),
    )
}

/// Appendix B "Freeze a handoff": the handoff row, `evidence_generation+1`
/// and the `judgment_deadline`.
fn freeze_handoff() -> Transition {
    let run = run_reserved();
    changes(vec![
        StateChange::FreezeHandoff(FrozenHandoff {
            run: run.id.clone(),
            work_generation: 0,
            digest: Digest([1; 32]),
            frozen_path: "/f".into(),
            frozen_at: NOW,
            assessed: false,
        }),
        update_run(
            0,
            Run {
                version: 1,
                evidence_generation: 1,
                judgment_deadline: Some(Timestamp(NOW.0 + 1_800_000)),
                ..run
            },
        ),
    ])
}

/// Appendix B "Record evidence": `evidence_digest`, `evidence_generation+1`.
fn record_evidence() -> Transition {
    changes(vec![update_run(
        0,
        Run {
            version: 1,
            evidence_generation: 1,
            evidence_digest: Some(Digest([2; 32])),
            ..run_reserved()
        },
    )])
}

/// The topology plan: `tab_create` journaled, Run `starting`.
fn plan_tab() -> Transition {
    launch_plan(
        &run_reserved(),
        &decision(),
        &PlacementPlan::NewTab,
        &PaneId("pane-1".into()),
    )
}

/// The planned topology effect's key, read from the plan itself.
fn tab_effect_key() -> EffectKey {
    plan_tab()
        .effects
        .first()
        .map_or_else(|| EffectKey(String::new()), |effect| effect.key.clone())
}

/// `launch_plan` ++ `begin`: the topology plan and phase `launching`.
fn launch_begin() -> Transition {
    concat(plan_tab(), begin(&launch_routed()))
}

/// The topology effect's result ++ `finish(Launched)`.
fn launch_finish_done() -> Transition {
    let starting = Run {
        version: 1,
        state: State::Starting,
        ..run_reserved()
    };
    let result = changes(vec![
        acknowledge(tab_effect_key(), None),
        update_run(
            1,
            Run {
                version: 2,
                identity: Some(identity()),
                ..starting
            },
        ),
    ]);
    let launched = LaunchOutcome::Launched {
        run: run_reserved().id,
        operating_point: OperatingPointId("op-0".into()),
        requested_operating_point: None,
        tier_evidence: decision(),
    };
    concat(
        result,
        finish(&launch_launching(), launched, None, None, NOW, &policy()),
    )
}
