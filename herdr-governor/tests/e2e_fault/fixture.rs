//! `fixture` — the seeded-row builders the restart scenarios share:
//! caller binding, a captured `ChildIdentity`, an `active`/`starting`
//! `Run` matching the scripted occupant field-for-field, journaled
//! `Effect` rows with the honest `op_digest` (so the F8 audit would pass
//! were the row ever re-offered), the canonical `RecordLaunch` +
//! `ReserveRun` seed and the `planned`/`dispatching`/`acknowledged`
//! effect-state walks. The launch pipeline itself is never seeded —
//! only the rows a kill or a restart's global scan must find.

use std::time::Duration;

use governor_core::config::{Policy, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, ChildIdentity, EffectId, EffectKey, HerdrIncarnation,
    LaunchId, NativeSession, PaneId, RelayInstanceId, RunId, TerminalId,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectReceipt, EffectResolution, EffectState, EffectTarget, EffectWrite,
    Run, State, StateChange, Transition, op_digest,
};
use governor_core::task::LaunchPhase;
use herdr_governor::store::Store;

use super::{CALLER_PANE, NOW, RELAY, caller_key, changes, launch_row, run_row};

/// The policy `settle`/`transition` calls take — the same durations the
/// supervision suite seeds.
pub(crate) fn policy() -> Policy {
    Policy {
        tiers: vec![Tier("standard".into())],
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.6,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

/// `BindCaller` for the scripted caller — mailbox events have somewhere
/// to land.
pub(crate) fn bind_caller(store: &mut Store) {
    store
        .apply(
            &changes(vec![StateChange::BindCaller(CallerBinding {
                caller: caller_key(),
                relay_instance: RelayInstanceId(RELAY.into()),
                pane_at_bind: PaneId(CALLER_PANE.into()),
            })]),
            NOW,
        )
        .expect("bind caller");
}

/// A captured identity matching `agent_topology`'s occupant field-for-
/// field — `name` is `gov-<id>`, `kind` `kind-a`.
pub(crate) fn identity_on(
    id: &str,
    pane: &str,
    terminal: &str,
    session: Option<&str>,
    inc: &str,
) -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation(inc.to_owned()),
        terminal_id: TerminalId(terminal.to_owned()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName(format!("gov-{id}")),
        native_session: session.map(|value| NativeSession(value.to_owned())),
        pane_id: PaneId(pane.to_owned()),
    }
}

/// A `run_row` at `state` carrying the identity `identity_on` captures.
pub(crate) fn run_on(
    id: &str,
    launch: &str,
    state: State,
    pane: &str,
    terminal: &str,
    session: Option<&str>,
    inc: &str,
) -> Run {
    let mut run = run_row(id, launch, state);
    run.identity = Some(identity_on(id, pane, terminal, session, inc));
    run
}

/// The canonical seed: `RecordLaunch` walked the legal phase chain to
/// `phase` (`evaluating` → `routed` → `launching`/`done` — the P4.0
/// matrix admits no skips) plus `ReserveRun` — the `runs.launch_id` FK
/// demands the row exists first.
pub(crate) fn seed_run(store: &mut Store, run: &Run, phase: LaunchPhase) {
    let mut chain = Vec::new();
    let mut row = launch_row(&run.launch.0, LaunchPhase::Evaluating);
    chain.push(StateChange::RecordLaunch(row.clone()));
    if phase != LaunchPhase::Evaluating {
        row.phase = LaunchPhase::Routed;
        chain.push(StateChange::RecordLaunch(row.clone()));
    }
    if phase != LaunchPhase::Evaluating && phase != LaunchPhase::Routed {
        row.phase = phase;
        chain.push(StateChange::RecordLaunch(row));
    }
    chain.push(StateChange::ReserveRun(run.clone()));
    store.apply(&changes(chain), NOW).expect("seed run");
}

/// A run-bound `planned` `Effect` — the honest `op_digest` for the
/// rendered op: `prompt` digests the journal key (the payload), every
/// other run-bound kind the empty params the gate recomputes.
pub(crate) fn run_effect(
    key: &str,
    kind: EffectKind,
    run: &str,
    target: Option<&EffectTarget>,
) -> Effect {
    let params: &[u8] = match kind {
        EffectKind::Prompt => key.as_bytes(),
        EffectKind::JevEvaluate
        | EffectKind::TabCreate
        | EffectKind::PaneSplit
        | EffectKind::AgentStart
        | EffectKind::Close => &[],
    };
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind,
        subject_launch: None,
        subject_run: Some(RunId(run.into())),
        target: target.cloned(),
        payload_digest: Some(op_digest(kind, target, params)),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// A launch-bound `planned` `Effect` — `jev_evaluate`'s honest NULL
/// digest (it carries no external target).
pub(crate) fn launch_effect(key: &str, kind: EffectKind, launch: &str) -> Effect {
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind,
        subject_launch: Some(LaunchId(launch.into())),
        subject_run: None,
        target: None,
        payload_digest: None,
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// Journal `effect` `planned` then `dispatching` — the row a restart's
/// global scan must find, and the row a seam kills on.
pub(crate) fn seed_dispatching(store: &mut Store, effect: &Effect) {
    store
        .apply(
            &Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: vec![effect.clone()],
            },
            NOW,
        )
        .expect("plan");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: effect.key.clone(),
            })]),
            NOW,
        )
        .expect("dispatch");
}

/// `seed_dispatching` plus the result commit — a journaled `acknowledged`
/// row (`createdTopology`'s input).
pub(crate) fn seed_acknowledged(store: &mut Store, effect: &Effect, receipt: EffectReceipt) {
    seed_dispatching(store, effect);
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Result {
                key: effect.key.clone(),
                resolution: EffectResolution::Acknowledged {
                    receipt: Some(receipt),
                },
            })]),
            NOW,
        )
        .expect("result");
}

/// A `store.run` read by id — panics like `run_for` when absent.
pub(crate) fn read_run(store: &Store, id: &str) -> Run {
    store
        .run(&RunId(id.into()))
        .expect("run read")
        .expect("run row")
}
