//! `reconcile` — the P5.B3 e2e: `daemon::run` in-process against
//! `FakeHerdr`, runs seeded through a second `Store` connection on the
//! same `governor.db`. Where a seed must be invisible to §4.3 step 5
//! (an in-flight `dispatching` leg `mark_restart` would reclassify) it
//! lands after the governor socket exists — strictly post-bind. Every
//! wait is a bounded poll; nothing sleeps on the wall clock. The cases
//! live in `reconcile/` children by §4.7 step; the fixtures below are
//! theirs.

mod deadlines;
mod observations;
mod startup;
mod subscription;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use governor_core::config::{Policy, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, LaunchId, NativeSession, PaneId, ProjectRoot,
    RelayInstanceId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectState, EffectWrite, Run, Settlement, State, StateChange, Transition,
    UnresolvedReason, settle,
};
use governor_core::task::{Launch, LaunchPhase, Task};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::{self, Settings};
use herdr_governor::store::Store;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Topology};

const DEADLINE: Duration = Duration::from_secs(10);
/// `reconcile_secs = 1` — the fastest cadence the catalog accepts.
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
const PAST: Timestamp = Timestamp(1);

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
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(inc.to_owned()),
        terminal_id: TerminalId(terminal.to_owned()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName(format!("gov-{id}")),
        native_session: session.map(|s| NativeSession(s.to_owned())),
        pane_id: PaneId(pane.to_owned()),
    });
    run
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
            objective: "o".into(),
            scope: "s".into(),
            done_when: vec!["d".into()],
            constraints: vec![],
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
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

fn effect(key: &str, kind: EffectKind, run: &str) -> Effect {
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind,
        subject_launch: None,
        subject_run: Some(RunId(run.into())),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
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

/// Journal `key`'s effect `planned` then `dispatching`.
fn seed_dispatching(store: &mut Store, key: &str, kind: EffectKind, run: &str) {
    store
        .apply(
            &Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: vec![effect(key, kind, run)],
            },
            NOW,
        )
        .expect("plan");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey(key.into()),
            })]),
            NOW,
        )
        .expect("dispatch");
}

fn policy() -> Policy {
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

// — Daemon bring-up ———————————————————————————————————————————————————

/// `[daemon]` fixture — `reconcile_secs`/`herdr_socket` come through
/// `Settings` overrides; the catalog values just have to parse.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let state = root.join("state");
    let config = root.join("config");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("catalog.toml"),
        "[policy]\ntiers = [\"standard\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n\
         [catalog]\noperating_points = []\n\n\
         [daemon]\nherdr_socket = \"/nonexistent/herdr.sock\"\n\
         jev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n",
    )
    .unwrap();
    let credentials = config.join("credentials");
    std::fs::write(&credentials, "test-token\n").unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    (state, config)
}

fn settings(state: &Path, config: &Path, fake: &FakeHerdr, reconcile_secs: u64) -> Settings {
    Settings {
        state_dir: state.to_path_buf(),
        config_dir: config.to_path_buf(),
        herdr_socket: Some(fake.socket_path().to_path_buf()),
        reconcile_secs: Some(reconcile_secs),
    }
}

/// Spawn `daemon::run`; returns the task handle and the shutdown sender.
fn spawn_daemon(
    settings: Settings,
) -> (
    JoinHandle<Result<ExitCode, daemon::DaemonError>>,
    oneshot::Sender<()>,
) {
    let (tx, rx) = oneshot::channel();
    (tokio::spawn(daemon::run(settings, None, Some(rx))), tx)
}

/// The `<state>/governor.sock` appearing means §4.3 steps 1–8 completed —
/// the startup pass already ran.
async fn wait_bound(state: &Path) {
    let sock = state.join("governor.sock");
    wait_for("governor socket", || sock.exists()).await;
}

fn open_store(state: &Path) -> Store {
    Store::open(&state.join("governor.db")).expect("store opens")
}

fn read_run(store: &Store, id: &str) -> Run {
    store
        .run(&RunId(id.into()))
        .expect("run read")
        .expect("run row")
}

/// The bounded poll: `tokio::time::sleep` yields to the daemon and fake
/// tasks on the same (current-thread) runtime — `park_timeout` would
/// freeze the executor.
async fn wait_for(what: &str, mut until: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        if until() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Stop the daemon and require a clean exit.
async fn stop(
    handle: JoinHandle<Result<ExitCode, daemon::DaemonError>>,
    shutdown: oneshot::Sender<()>,
) {
    let _sent = shutdown.send(());
    let code = handle.await.expect("daemon task").expect("run");
    assert_eq!(code, ExitCode::SUCCESS);
}

/// `single_shell` with an agent occupant on `w1:p1` — returns the
/// topology and the pane's `terminal_id` (the identity seed needs it).
fn agent_topology(name: &str, session: Option<&str>) -> (Topology, String) {
    let mut topology = Topology::single_shell();
    let pane = topology.panes.first_mut().expect("shell pane");
    pane.agent = Some(Occupant {
        name: name.to_owned(),
        kind: "kind-a".to_owned(),
        status: "working".to_owned(),
        session: session.map(|value| SessionRef {
            kind: SessionKind::Id,
            value: value.to_owned(),
        }),
    });
    let terminal = pane.terminal_id.clone();
    (topology, terminal)
}

/// The `HerdrIncarnation` a snapshot over `path` mints —
/// `<inode>:<mtime_secs>.<mtime_nsecs zero-padded to 9 digits>`
/// (§4.3's spelling).
fn socket_incarnation(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).expect("socket stat");
    format!("{}:{}.{:09}", meta.ino(), meta.mtime(), meta.mtime_nsec())
}
