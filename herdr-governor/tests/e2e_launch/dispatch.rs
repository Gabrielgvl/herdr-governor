//! `dispatch` — §4.4's pipeline against `FakeHerdr`: the commit precedes
//! the wire (F8), the fresh re-verify keeps a prompt off a blocked or
//! non-unique child (F10), the task prompt and nudge render onto the wire
//! (F9/F25), the ack must name the captured identity (F16), the cancel
//! `close` leg lands (F20), and the §4.14 pre-wire gate answers shutdown
//! `Failed{Absent}` (F29). The cases live in `wire`/`holds`/`terminal`;
//! the domain fixtures stay here — the daemon, wait, seam and wire-read
//! legs are the shared `support` harness (P5.T1).
//!
//! `wire` — commit→wire→ack (F8, F9/F25, F20). `holds` — the F10
//! verify-holds. `terminal` — F16's `unknown` and F29's `absent`.

mod holds;
mod terminal;
mod wire;

use std::path::Path;
use std::time::Duration;

use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, LaunchId, NativeSession, PaneId, ProjectRoot,
    RelayInstanceId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectState, EffectTarget, Run, RunUpdate, State,
    StateChange, Transition, op_digest,
};
use governor_core::task::{Launch, LaunchPhase, Task};
use herdr_governor::daemon::Settings;
use herdr_governor::store::{ApplyError, Store};

use crate::support::daemon::{
    Catalog, DaemonDirs, TestDaemon, await_for, fixture, never, pause_at_dispatch,
    socket_incarnation,
};
use crate::support::fake_herdr::topology::agent_topology;
use crate::support::fake_herdr::{FakeHerdr, Fault};

/// `reconcile_secs = 1` — the fastest cadence the catalog accepts; drives
/// `hand_out` once a second for the interactive cases.
const TICK_SECS: u64 = 1;

// — Fixture builders —————————————————————————————————————————————————

fn caller(n: u8) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession(format!("sess-caller-{n}")),
    }
}

fn bind_caller(store: &mut Store) {
    store
        .apply(
            &changes(vec![StateChange::BindCaller(CallerBinding {
                caller: caller(1),
                relay_instance: RelayInstanceId("relay-1".into()),
                pane_at_bind: PaneId("w1:p1".into()),
            })]),
            NOW,
        )
        .expect("bind caller");
}

const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// `max_age_deadline` when the test does not want one: year 2100 — far
/// enough for every deadline, inside the store's RFC3339 range.
const FAR: Timestamp = Timestamp(4_102_444_800_000);

fn run_row_on(id: &str, launch: &str) -> Run {
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
        max_age_deadline: FAR,
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

/// An `active` run carrying a captured identity that matches `pane`'s
/// scripted occupant field-for-field (terminal, kind, name, session).
fn active_run_on(id: &str, pane: &str, terminal: &str, session: Option<&str>, inc: &str) -> Run {
    let mut run = run_row_on(id, &format!("l-{id}"));
    run.state = State::Active;
    run.identity = Some(identity_on(id, pane, terminal, session, inc));
    run
}

fn identity_on(
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
        native_session: session.map(|s| NativeSession(s.to_owned())),
        pane_id: PaneId(pane.to_owned()),
    }
}

fn launch_row(id: &str, phase: LaunchPhase) -> Launch {
    Launch {
        id: LaunchId(id.into()),
        caller: caller(1),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(id.into()),
        digest_version: 1,
        task_digest: Digest([0xab; 32]),
        task: Task {
            objective: "OBJECTIVE-MARK".into(),
            scope: "s".into(),
            done_when: vec!["DONE-MARK".into()],
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
        outcome: None,
    }
}

fn changes(changes: Vec<StateChange>) -> Transition {
    Transition {
        state_changes: changes,
        events: Vec::new(),
        effects: Vec::new(),
    }
}

fn plan(effects: Vec<Effect>) -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects,
    }
}

/// A `planned` effect row, honestly digested so the F8 audit passes: the
/// `prompt` recipe digests the key bytes; `close` takes none.
fn effect(key: &str, kind: EffectKind, run: &str, target: Option<EffectTarget>) -> Effect {
    let params: &[u8] = match kind {
        EffectKind::Prompt => key.as_bytes(),
        EffectKind::AgentStart
        | EffectKind::TabCreate
        | EffectKind::PaneSplit
        | EffectKind::Close
        | EffectKind::JevEvaluate => &[],
    };
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind,
        subject_launch: None,
        subject_run: Some(RunId(run.into())),
        payload_digest: Some(op_digest(kind, target.as_ref(), params)),
        target,
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// The canonical reserve seed: `evaluating` → `routed` + `ReserveRun`
/// (the `runs.launch_id` FK demands the row exists first).
fn seed_run(store: &mut Store, run: &Run) {
    let launch = run.launch.0.clone();
    store
        .apply(
            &changes(vec![StateChange::RecordLaunch(launch_row(
                &launch,
                LaunchPhase::Evaluating,
            ))]),
            NOW,
        )
        .expect("record evaluating");
    store
        .apply(
            &changes(vec![
                StateChange::RecordLaunch(launch_row(&launch, LaunchPhase::Routed)),
                StateChange::ReserveRun(run.clone()),
            ]),
            NOW,
        )
        .expect("route + reserve");
}

/// Move `run` to `state` by the CAS — the seeding path's own write, not a
/// journal shortcut. The coordinator's ticks can win the race, so the
/// read+write recomputes until it lands.
fn move_state(store: &mut Store, id: &str, state: State) {
    for _attempt in 0..8 {
        let mut run = read_run(store, id);
        run.state = state;
        let expected = run.version;
        run.version = expected.saturating_add(1);
        match store.apply(
            &changes(vec![StateChange::UpdateRun(RunUpdate {
                expected_version: expected,
                record: run,
            })]),
            NOW,
        ) {
            Ok(()) => return,
            Err(ApplyError::Conflict { .. }) => {}
            Err(other) => panic!("state move: {other}"),
        }
    }
    panic!("state move lost to the coordinator eight times running");
}

// — The fixture world ————————————————————————————————————————————————

/// The `DaemonDirs` + `Settings` for `TestDaemon` — like
/// `e2e_supervision`'s, plus `shutdown_grace_secs = 1` so the F29 drain
/// ends promptly after the paused runner's result lands (the seam pause
/// is well inside it). The argv carries the fake's socket and the case's
/// reconcile cadence; the catalog's own socket stays decorative.
fn world(fake: &FakeHerdr, reconcile_secs: u64) -> (DaemonDirs, Settings) {
    let mut catalog = Catalog::new(Path::new("/nonexistent/herdr.sock"), "http://127.0.0.1:9");
    catalog.tiers = vec!["standard".to_owned()];
    catalog.reconcile_secs = 3600;
    catalog.daemon_extra = "shutdown_grace_secs = 1\n".to_owned();
    let dirs = fixture(&catalog);
    let mut settings = dirs.settings();
    settings.herdr_socket = Some(fake.socket_path().to_path_buf());
    settings.reconcile_secs = Some(reconcile_secs);
    (dirs, settings)
}

fn open_store(dirs: &DaemonDirs) -> Store {
    Store::open(&dirs.store_path()).expect("store opens")
}

fn read_run(store: &Store, id: &str) -> Run {
    store
        .run(&RunId(id.into()))
        .expect("run read")
        .expect("run row")
}

fn read_effect(store: &Store, key: &str) -> Effect {
    store
        .effect(&EffectKey(key.into()))
        .expect("effect read")
        .expect("effect row")
}

/// The `result_json` column — the OQ-11 cause lives there for failures.
fn result_json_of(store: &Store, key: &str) -> Option<String> {
    store
        .conn()
        .query_row(
            "SELECT result_json FROM effects WHERE effect_key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}
