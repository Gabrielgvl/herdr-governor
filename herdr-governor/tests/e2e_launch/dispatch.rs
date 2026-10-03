//! `dispatch` — §4.4's pipeline against `FakeHerdr`: the commit precedes
//! the wire (F8), the fresh re-verify keeps a prompt off a blocked or
//! non-unique child (F10), the task prompt and nudge render onto the wire
//! (F9/F25), the ack must name the captured identity (F16), the cancel
//! `close` leg lands (F20), and the §4.14 pre-wire gate answers shutdown
//! `Failed{Absent}` (F29). The cases live in `wire`/`holds`/`terminal`;
//! the fixtures stay here.
//!
//! `wire` — commit→wire→ack (F8, F9/F25, F20). `holds` — the F10
//! verify-holds. `terminal` — F16's `unknown` and F29's `absent`.

mod holds;
mod terminal;
mod wire;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::{self, Boundary, SeamAction, SeamConfig, Settings};
use herdr_governor::store::{ApplyError, Store};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Fault, Topology};

const DEADLINE: Duration = Duration::from_secs(10);
/// `reconcile_secs = 1` — the fastest cadence the catalog accepts; drives
/// `hand_out` once a second for the interactive cases.
const TICK_SECS: u64 = 1;
/// The seam pause that holds a runner at `dispatch_committed` — long
/// enough for the test to interpose, far inside `shutdown_grace`.
const SEAM_PAUSE_MS: u64 = 400;

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

// — Daemon bring-up ———————————————————————————————————————————————————

/// `[daemon]` fixture — like `e2e_supervision`'s, plus
/// `shutdown_grace_secs = 1` so the F29 drain ends promptly after the
/// paused runner's result lands (the 400ms seam pause is well inside it).
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
         jev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n\
         shutdown_grace_secs = 1\n",
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
    seam: Option<SeamConfig>,
) -> (
    JoinHandle<Result<ExitCode, daemon::DaemonError>>,
    oneshot::Sender<()>,
) {
    let (tx, rx) = oneshot::channel();
    (tokio::spawn(daemon::run(settings, seam, Some(rx))), tx)
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

/// Whether `what` becomes true within `within` — the negative wait: a
/// thing that must NOT happen is asserted by exhausting the window.
async fn never(what: &str, within: Duration, mut happened: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(within).expect("deadline");
    while Instant::now() < deadline {
        assert!(!happened(), "{what} must not happen");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
/// `<inode>:<mtime_secs>.<mtime_nsecs zero-padded to 9 digits>`.
fn socket_incarnation(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).expect("socket stat");
    format!("{}:{}.{:09}", meta.ino(), meta.mtime(), meta.mtime_nsec())
}

fn saw_wire(fake: &FakeHerdr, method: &str) -> bool {
    fake.requests().iter().any(|(m, _)| m == method)
}

/// A pause seam on `suffix` at `dispatch_committed` — the deterministic
/// window between the durable commit and the wire op.
fn pause_at_dispatch(suffix: &str) -> SeamConfig {
    SeamConfig {
        suffix: suffix.to_owned(),
        boundary: Boundary::DispatchCommitted,
        action: SeamAction::Pause(Duration::from_millis(SEAM_PAUSE_MS)),
    }
}

// — The six §4.4 contracts ————————————————————————————————————————————

// — Wire-side assertions ——————————————————————————————————————————————

/// The `text` param of the most recent `agent.prompt` to `pane`.
fn wire_prompt_text(fake: &FakeHerdr, pane: &str) -> String {
    fake.requests()
        .iter()
        .rev()
        .find(|(m, p)| {
            m == "agent.prompt" && p.get("target").and_then(|v| v.as_str()) == Some(pane)
        })
        .and_then(|(_, p)| p.get("text").and_then(|v| v.as_str()).map(str::to_owned))
        .expect("an agent.prompt reached the wire")
}

/// The `text` param of the `n`th `agent.prompt` (0-based).
fn wire_prompt_text_at(fake: &FakeHerdr, n: usize) -> String {
    fake.requests()
        .iter()
        .filter(|(m, _)| m == "agent.prompt")
        .nth(n)
        .and_then(|(_, p)| p.get("text").and_then(|v| v.as_str()).map(str::to_owned))
        .expect("the nth agent.prompt reached the wire")
}
