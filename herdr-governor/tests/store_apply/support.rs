//! Constructed inputs for the `store::apply` tests: core values built by
//! hand (no I/O, no clock), a seeded store built only through `apply`.

use governor_core::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, DedupKey, Digest, EffectId, EffectKey, EventId,
    IdempotencyKey, LaunchId, NativeSession, PaneId, ProjectRoot, RelayInstanceId, RunId,
    Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectResolution, EffectState, EffectWrite, Run, State,
    StateChange, Transition,
};
use governor_core::task::{Launch, LaunchOutcome, LaunchPhase, Task};
use herdr_governor::store::Store;
use tempfile::{TempDir, tempdir};

pub const NOW: Timestamp = Timestamp(1_790_812_800_000);
pub const LATER: Timestamp = Timestamp(1_790_812_860_000);

#[must_use]
pub fn caller(n: u8) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession(format!("sess-{n}")),
    }
}

#[must_use]
pub fn binding(n: u8) -> StateChange {
    StateChange::BindCaller(CallerBinding {
        caller: caller(n),
        relay_instance: RelayInstanceId(format!("relay-{n}")),
        pane_at_bind: PaneId("pane-1".into()),
    })
}

#[must_use]
pub fn launch(id: &str, phase: LaunchPhase, outcome: Option<LaunchOutcome>) -> Launch {
    Launch {
        id: LaunchId(id.into()),
        caller: caller(1),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(id.into()),
        digest_version: 1,
        task_digest: Digest([0xab; 32]),
        task: Task {
            objective: "o".into(),
            scope: "s".into(),
            done_when: vec!["d".into()],
            constraints: vec![],
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
            retention: None,
        },
        phase,
        decision: None,
        config_version: None,
        outcome,
    }
}

#[must_use]
pub fn run(id: &str, launch: &str) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(launch.into()),
        owner: caller(1),
        owner_generation: 0,
        version: 0,
        state: State::Reserved,
        prompt_certainty: None,
        child_name: format!("gov-{id}"),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/p".into(),
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
        max_age_deadline: Timestamp(NOW.0 + 86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

#[must_use]
pub fn effect(key: &str, launch: Option<&str>, run: Option<&str>) -> Effect {
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind: EffectKind::Prompt,
        subject_launch: launch.map(|l| LaunchId(l.into())),
        subject_run: run.map(|r| RunId(r.into())),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// The dispatch commit: `planned` → `dispatching`.
#[must_use]
pub fn dispatch(key: &str) -> StateChange {
    StateChange::WriteEffect(EffectWrite::Dispatch {
        key: EffectKey(key.into()),
    })
}

/// The result commit: `dispatching` → the resolution's state.
#[must_use]
pub fn result(key: &str, resolution: EffectResolution) -> StateChange {
    StateChange::WriteEffect(EffectWrite::Result {
        key: EffectKey(key.into()),
        resolution,
    })
}

/// The OQ-13 terminal write: a stranded row closed `failed`.
#[must_use]
pub fn terminal(key: &str, certainty: EffectCertainty) -> StateChange {
    StateChange::WriteEffect(EffectWrite::Terminal {
        key: EffectKey(key.into()),
        certainty,
    })
}

#[must_use]
pub fn event(id: &str, launch: &str) -> MailboxEvent {
    MailboxEvent {
        id: EventId(id.into()),
        dedup_key: DedupKey(format!("launch:{launch}:launch_answered")),
        subject: MailboxSubject::Launch(LaunchId(launch.into())),
        kind: MailboxEventKind::LaunchAnswered,
        body: "{}".into(),
    }
}

#[must_use]
pub fn transition(
    state_changes: Vec<StateChange>,
    events: Vec<MailboxEvent>,
    effects: Vec<Effect>,
) -> Transition {
    Transition {
        state_changes,
        events,
        effects,
    }
}

#[must_use]
pub fn changes(state_changes: Vec<StateChange>) -> Transition {
    transition(state_changes, vec![], vec![])
}

#[must_use]
pub fn store() -> (TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = Store::open(&dir.path().join("store.db")).unwrap();
    (dir, store)
}

/// A store with caller 1 bound, Launch `l-1` routed, Run `r-1` reserved
/// and the effect `run:r-1:prompt:task` planned — all through `apply`.
#[must_use]
pub fn seeded() -> (TempDir, Store) {
    let (dir, mut store) = store();
    let admit = changes(vec![
        binding(1),
        StateChange::RecordLaunch(launch("l-1", LaunchPhase::Evaluating, None)),
    ]);
    store.apply(&admit, NOW).unwrap();
    let route = transition(
        vec![
            StateChange::RecordLaunch(launch("l-1", LaunchPhase::Routed, None)),
            StateChange::ReserveRun(run("r-1", "l-1")),
        ],
        vec![],
        vec![effect("run:r-1:prompt:task", None, Some("r-1"))],
    );
    store.apply(&route, NOW).unwrap();
    (dir, store)
}

#[must_use]
pub fn count(store: &Store, table: &str) -> i64 {
    store
        .conn()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
