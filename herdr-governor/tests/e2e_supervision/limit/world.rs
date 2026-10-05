//! `world` — the supervised-Run world the `limit` cases share: one
//! `active` Run seeded through a second `Store` connection, the caller
//! at `w1:p1`, the child `gov-r1` at `w1:p2` (a `devin` or `claude`
//! occupant), the fakes, and the reads the cases assert on. The task
//! prompt's journal row is dispatched at `NOW` — the probe's
//! `cycle_start` anchor — and `devin_log_dir` pins the process-log scan
//! to the world's dir.

use std::path::{Path, PathBuf};
use std::time::Duration;

use governor_core::config::Provider;
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, LaunchId, NativeSession, PaneId, ProjectRoot,
    RelayInstanceId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectResolution, EffectState, EffectWrite, Run, State, StateChange,
    Transition,
};
use governor_core::task::{Launch, LaunchPhase, Task};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::store::Store;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, fixture};
use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Topology};
use crate::support::fake_jev::{Answer, FakeJev};

/// The seeded rows' apply time — `2026-10-01T00:00:00Z`. The task prompt's
/// `dispatched_at` is the probe's `cycle_start`: the committed fixture's
/// stall line (2026-10-02T01:33:04Z) counts, a 2026-09-30 record does not.
const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// `max_age_deadline` the suite never reaches: year 2100.
const FAR: Timestamp = Timestamp(4_102_444_800_000);
/// Several review intervals (1 s each) — the "never" windows.
pub(super) const QUIET: Duration = Duration::from_secs(4);
/// The bound session the committed fixture names.
pub(super) const SESSION: &str = "tidal-vase";
/// The Claude child's session — UUID-shaped, as the record requires.
const CLAUDE: &str = "11111111-2222-3333-4444-555555555555";

/// `tests/fixtures/contract/devin-provider-limit.log` — the redacted copy
/// of the real stall (`tidal-vase`, zeroed trace id).
pub(super) fn fixture_bytes() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/contract/devin-provider-limit.log"),
    )
    .expect("the committed fixture reads")
}

/// Which harness the child runs — the probe's source differs per kind.
#[derive(Debug, Clone, Copy)]
pub(super) enum Child {
    /// `devin` — the `devin_log_dir` process-log scan.
    Devin,
    /// `claude` — the `<slug>/<uuid>.jsonl` session record.
    Claude,
}

/// The knobs a test sets on its world.
pub(super) struct Opts {
    pub child: Child,
    /// The child's Herdr status.
    pub status: &'static str,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            child: Child::Devin,
            status: "idle",
        }
    }
}

/// One supervised Run's world.
pub(super) struct World {
    pub fake: FakeHerdr,
    pub jev: FakeJev,
    pub dirs: DaemonDirs,
    /// `[daemon] devin_log_dir` — the process-log scan dir.
    pub logs: PathBuf,
    /// The session transcript (`data/<id>.json` for Devin, the slugged
    /// `projects/<uuid>.jsonl` for Claude).
    pub transcript: PathBuf,
    /// The Run's worktree — the Claude record's `cwd`.
    pub cwd: PathBuf,
    /// The scratch root — held for its `Drop` (the world's cleanup).
    #[expect(
        dead_code,
        reason = "the TempDir's Drop removes every staged file; nothing reads it"
    )]
    pub tmp: TempDir,
}

/// Build the world: the caller at `w1:p1`, the child `gov-r1` at `w1:p2`,
/// an `active` Run `r1` bound to it with `provider` set (the cooldown
/// needs one), `run:r1:prompt:task` dispatched at `NOW` (the probe's
/// `cycle_start`), and the catalog's 1 s review interval.
pub(super) fn world(opts: &Opts) -> World {
    let tmp = tempfile::tempdir().expect("tmp");
    let logs = tmp.path().join("logs");
    let projects = tmp.path().join("projects");
    let data = tmp.path().join("data");
    let cwd = tmp.path().join("work");
    for dir in [&logs, &projects, &data, &cwd] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let (kind, session, transcript) = match opts.child {
        Child::Devin => {
            let path = data.join(format!("{SESSION}.json"));
            std::fs::write(
                &path,
                "{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"tidal-vase\",\"steps\":[{\"step_id\":1,\"source\":\"agent\",\"message\":\"reading the repo\"}]}",
            )
            .expect("atif doc");
            ("devin", SESSION.to_owned(), path)
        }
        Child::Claude => {
            let slug = cwd.to_string_lossy().replace('/', "-");
            let path = projects.join(slug).join(format!("{CLAUDE}.jsonl"));
            std::fs::create_dir_all(path.parent().expect("slug dir")).expect("projects dir");
            std::fs::write(&path, "").expect("empty session log");
            ("claude", CLAUDE.to_owned(), path)
        }
    };

    let mut topology = Topology::single_shell();
    topology.panes[0].agent = Some(occupant("owner-a", "kind-a", "sess-caller-1", "idle"));
    let (_, child_pane) = topology.create_tab("w1");
    let row = topology
        .panes
        .iter_mut()
        .find(|pane| pane.pane_id == child_pane)
        .expect("child pane");
    row.agent = Some(occupant("gov-r1", kind, &session, opts.status));
    let terminal = row.terminal_id.clone();
    let fake = FakeHerdr::start(topology);
    let jev = FakeJev::start();

    let mut catalog = Catalog::new(fake.socket_path(), jev.base_url());
    catalog.daemon_extra = format!(
        "review_interval_secs = 1\ntranscript_data_dirs = [\"{}\"]\ntranscript_project_dirs = [\"{}\"]\ndevin_log_dir = \"{}\"\n",
        data.display(),
        projects.display(),
        logs.display()
    );
    let dirs = fixture(&catalog);

    let mut run = run_row(&cwd);
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal),
        agent_kind: AgentKind(kind.into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession(session)),
        pane_id: PaneId(child_pane),
    });
    let mut store = open_store(&dirs);
    seed(&mut store, vec![StateChange::BindCaller(binding())]);
    seed(
        &mut store,
        vec![StateChange::RecordLaunch(launch(LaunchPhase::Evaluating))],
    );
    seed(
        &mut store,
        vec![
            StateChange::RecordLaunch(launch(LaunchPhase::Routed)),
            StateChange::ReserveRun(run),
        ],
    );
    // The task prompt row's `dispatched_at` is the probe's cycle_start.
    seed_effects(&mut store, vec![prompt_task()]);
    seed(
        &mut store,
        vec![
            StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey("run:r1:prompt:task".into()),
            }),
            StateChange::WriteEffect(EffectWrite::Result {
                key: EffectKey("run:r1:prompt:task".into()),
                resolution: EffectResolution::Acknowledged { receipt: None },
            }),
        ],
    );
    World {
        fake,
        jev,
        dirs,
        logs,
        transcript,
        cwd,
        tmp,
    }
}

impl World {
    /// Start the daemon in-process against this world.
    pub(super) async fn start(&self) -> TestDaemon {
        TestDaemon::start_in_process(&self.dirs.settings(), None).await
    }

    /// The Run's current row.
    pub(super) fn run(&self) -> Run {
        open_store(&self.dirs)
            .run(&RunId("r1".into()))
            .expect("run read")
            .expect("run row")
    }

    /// The `run:r1:limit:*` journal rows, in key order.
    pub(super) fn limit_rows(&self) -> Vec<Effect> {
        open_store(&self.dirs)
            .journal(&RunId("r1".into()))
            .expect("journal read")
            .into_iter()
            .filter(|effect| effect.key.0.starts_with("run:r1:limit:"))
            .collect()
    }

    /// The Jev requests carrying a typed `limitRecord` (`state.blocked`),
    /// in arrival order.
    pub(super) fn limit_asks(&self) -> Vec<Value> {
        self.jev
            .requests()
            .iter()
            .filter_map(|request| request.state().cloned())
            .filter(|state| {
                state
                    .get("blocked")
                    .is_some_and(|blocked| blocked.get("limitRecord").is_some())
            })
            .collect()
    }

    /// Write `bytes` as `name` under `devin_log_dir`.
    pub(super) fn stage_log(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.logs.join(name);
        std::fs::write(&path, bytes).expect("log writes");
        path
    }

    /// Append a 429 record to the Claude session log. `timestamp` is the
    /// record's own `Z` time; the extra fields land verbatim.
    pub(super) fn claude_record(&self, timestamp: &str, extra: &Value) {
        use std::io::Write as _;
        let mut body = serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "error": "rate_limit",
            "apiErrorStatus": 429,
            "requestId": "req-1",
            "sessionId": CLAUDE,
            "cwd": self.cwd.to_string_lossy(),
            "timestamp": timestamp,
        });
        body.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .expect("session log opens");
        writeln!(file, "{body}").expect("record appends");
    }
}

/// Every supervision question answered; `provider_limited` takes `p`.
pub(super) fn answers(provider_limited: f64) -> Vec<(&'static str, Answer)> {
    [
        ("blocked_on_input", Answer::noul(0.1)),
        ("no_recent_progress", Answer::noul(0.1)),
        ("outside_scope", Answer::noul(0.1)),
        ("provider_limited", Answer::noul(provider_limited)),
        ("handoff_meets_item_0", Answer::noul(0.1)),
    ]
    .into()
}

/// The seeded caller (`sess-caller-1` at `w1:p1`).
pub(super) fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-caller-1".into()),
    }
}

fn binding() -> CallerBinding {
    CallerBinding {
        caller: caller(),
        relay_instance: RelayInstanceId("relay-1".into()),
        pane_at_bind: PaneId("w1:p1".into()),
    }
}

fn occupant(name: &str, kind: &str, session: &str, status: &str) -> Occupant {
    Occupant {
        name: name.to_owned(),
        kind: kind.to_owned(),
        status: status.to_owned(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: session.to_owned(),
        }),
    }
}

fn run_row(cwd: &Path) -> Run {
    Run {
        id: RunId("r1".into()),
        launch: LaunchId("l-r1".into()),
        owner: caller(),
        owner_generation: 0,
        version: 0,
        state: State::Active,
        prompt_certainty: None,
        child_name: "gov-r1".into(),
        identity: None,
        operating_point: None,
        provider: Some(Provider("vendor-b".into())),
        tier_start: None,
        cwd: cwd.to_string_lossy().into_owned(),
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

/// The task prompt's journal row — `cycle_start` is its `dispatched_at`.
fn prompt_task() -> Effect {
    Effect {
        id: EffectId("eff:run:r1:prompt:task".into()),
        key: EffectKey("run:r1:prompt:task".into()),
        kind: EffectKind::Prompt,
        subject_launch: None,
        subject_run: Some(RunId("r1".into())),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

fn launch(phase: LaunchPhase) -> Launch {
    Launch {
        id: LaunchId("l-r1".into()),
        caller: caller(),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey("l-r1".into()),
        digest_version: 1,
        task_digest: Digest([0xab; 32]),
        task: Task {
            objective: "fix the failing test".into(),
            scope: "the parser module".into(),
            done_when: vec!["the tests pass".into()],
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

fn socket_incarnation(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).expect("socket stat");
    format!("{}:{}.{:09}", meta.ino(), meta.mtime(), meta.mtime_nsec())
}

/// A second `Store` connection on the daemon's `governor.db`.
pub(super) fn open_store(dirs: &DaemonDirs) -> Store {
    Store::open(&dirs.store_path()).expect("store opens")
}

fn seed(store: &mut Store, changes: Vec<StateChange>) {
    store
        .apply(
            &Transition {
                state_changes: changes,
                events: Vec::new(),
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("seed apply");
}

fn seed_effects(store: &mut Store, effects: Vec<Effect>) {
    store
        .apply(
            &Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects,
            },
            NOW,
        )
        .expect("seed apply");
}

/// The `run:r1:nudge:*` prompts the child pane has received (F25).
pub(super) fn nudges(fake: &FakeHerdr) -> usize {
    fake.requests()
        .into_iter()
        .filter(|(method, params)| {
            method == "agent.prompt"
                && params["target"] == "w1:p2"
                && params["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("still working?"))
        })
        .count()
}
