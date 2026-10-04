//! `world` — the supervised-Run world the `evidence` suite and its
//! `acceptance`/`idle` children share: one `active` Run seeded through a
//! second `Store` connection, the caller at `w1:p1`, the child `gov-r1`
//! at `w1:p2`, the fakes, and the reads the cases assert on.

use std::path::{Path, PathBuf};
use std::time::Duration;

use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, HerdrIncarnation,
    IdempotencyKey, LaunchId, NativeSession, PaneId, ProjectRoot, RelayInstanceId, RunId,
    TerminalId, Timestamp,
};
use governor_core::lifecycle::{Effect, Run, State, StateChange, Transition};
use governor_core::task::{Launch, LaunchPhase, Task};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::store::Store;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, fixture};
use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Topology};
use crate::support::fake_jev::{Answer, FakeJev};

const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// `max_age_deadline` the suite never reaches: year 2100.
const FAR: Timestamp = Timestamp(4_102_444_800_000);
/// Several review intervals (1 s each) — the "never" windows.
pub(super) const QUIET: Duration = Duration::from_secs(4);

// — The world ————————————————————————————————————————————————————————

/// How the child's session is reported and where its transcript lives.
#[derive(Debug, Clone)]
pub(super) enum Session {
    /// A path-keyed (`pi`) session: the transcript is a JSONL file the
    /// world creates with its header.
    PiPath,
    /// An id-keyed (`devin`) session under `<tmp>/data`; the document is
    /// the test's to write (absent during the first turn).
    DevinId(&'static str),
}

/// The knobs a test sets on its world.
#[derive(Debug, Clone)]
pub(super) struct Opts {
    pub session: Session,
    /// The child's Herdr status.
    pub status: &'static str,
    /// The Task's doneWhen items.
    pub done_when: Vec<String>,
    /// The Run's pinned base (F6) and worktree.
    pub base_commit: Option<String>,
    /// `None` → a fresh empty dir.
    pub cwd: Option<PathBuf>,
    /// Whether the owner's session holds a pane (F23's pause).
    pub owner_present: bool,
    /// Extra `[policy]` TOML lines.
    pub policy_extra: String,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            session: Session::PiPath,
            status: "working",
            done_when: vec!["the tests pass".into()],
            base_commit: None,
            cwd: None,
            owner_present: true,
            policy_extra: String::new(),
        }
    }
}

/// One supervised Run's world: the fakes, the daemon dirs, the child's
/// transcript, and the seeded Run.
pub(super) struct World {
    pub fake: FakeHerdr,
    pub jev: FakeJev,
    pub dirs: DaemonDirs,
    pub transcript: PathBuf,
    pub tmp: TempDir,
}

/// Build the world: the caller at `w1:p1`, the child `gov-r1` at
/// `w1:p2`, an `active` Run `r1` bound to it, the catalog naming both
/// fakes with a 1 s review interval.
pub(super) fn world(opts: &Opts) -> World {
    let tmp = tempfile::tempdir().expect("tmp");
    let (kind, session_kind, session, transcript) = match opts.session {
        Session::PiPath => {
            let path = tmp.path().join("2026_s1.jsonl");
            std::fs::write(
                &path,
                "{\"type\":\"session\",\"version\":3,\"id\":\"s1\",\"cwd\":\"/x\"}\n",
            )
            .expect("transcript header");
            let value = path.to_string_lossy().into_owned();
            ("pi", SessionKind::Path, value, path)
        }
        Session::DevinId(id) => {
            let path = tmp.path().join("data").join(format!("{id}.json"));
            std::fs::create_dir_all(tmp.path().join("data")).expect("data root");
            ("devin", SessionKind::Id, id.to_owned(), path)
        }
    };
    let mut topology = Topology::single_shell();
    let owner_session = if opts.owner_present {
        "sess-caller-1"
    } else {
        "sess-elsewhere"
    };
    topology.panes[0].agent = Some(occupant(
        "owner-a",
        "kind-a",
        SessionKind::Id,
        owner_session,
        "idle",
    ));
    let (_, child_pane) = topology.create_tab("w1");
    let row = topology
        .panes
        .iter_mut()
        .find(|pane| pane.pane_id == child_pane)
        .expect("child pane");
    row.agent = Some(occupant(
        "gov-r1",
        kind,
        session_kind,
        &session,
        opts.status,
    ));
    let terminal = row.terminal_id.clone();
    let fake = FakeHerdr::start(topology);
    let jev = FakeJev::start();

    let mut catalog = Catalog::new(fake.socket_path(), jev.base_url());
    catalog.policy_extra = opts.policy_extra.clone();
    catalog.daemon_extra = format!(
        "review_interval_secs = 1\ntranscript_data_dirs = [\"{}\"]\ntranscript_project_dirs = []\n",
        tmp.path().join("data").display()
    );
    let dirs = fixture(&catalog);

    let cwd = opts.cwd.clone().unwrap_or_else(|| tmp.path().join("work"));
    std::fs::create_dir_all(&cwd).expect("cwd");
    let mut run = run_row(&cwd, opts.base_commit.clone());
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal),
        agent_kind: AgentKind(kind.into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession(session)),
        pane_id: PaneId(child_pane),
    });
    let mut store = open_store(&dirs);
    seed(
        &mut store,
        vec![StateChange::BindCaller(CallerBinding {
            caller: caller(),
            relay_instance: RelayInstanceId("relay-1".into()),
            pane_at_bind: PaneId("w1:p1".into()),
        })],
    );
    seed(
        &mut store,
        vec![StateChange::RecordLaunch(launch_row(
            &opts.done_when,
            LaunchPhase::Evaluating,
        ))],
    );
    seed(
        &mut store,
        vec![
            StateChange::RecordLaunch(launch_row(&opts.done_when, LaunchPhase::Routed)),
            StateChange::ReserveRun(run),
        ],
    );
    World {
        fake,
        jev,
        dirs,
        transcript,
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

    /// The Run's journal.
    pub(super) fn journal(&self) -> Vec<Effect> {
        open_store(&self.dirs)
            .journal(&RunId("r1".into()))
            .expect("journal read")
    }

    /// Captured Jev request states of `family` (`review`, `blocked`,
    /// `acceptance`), in arrival order.
    pub(super) fn asks(&self, family: &str) -> Vec<Value> {
        self.jev
            .requests()
            .iter()
            .filter_map(|request| request.state()?.get(family).cloned())
            .collect()
    }

    /// Append one assistant message record to the Pi transcript.
    pub(super) fn say(&self, text: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .expect("transcript opens");
        let record = serde_json::json!({"type": "message", "message": {"role": "assistant",
            "content": [{"type": "text", "text": text}]}});
        writeln!(file, "{record}").expect("record appends");
    }

    /// The world's scratch root (outside the state dir).
    pub(super) fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// `<state>/handoffs/r1/handoff.md`.
    pub(super) fn handoff_path(&self) -> PathBuf {
        self.dirs.state_dir().join("handoffs/r1/handoff.md")
    }

    /// Write a valid marked handoff carrying `report`.
    pub(super) fn write_handoff(&self, report: &str) -> String {
        let text = format!("{report}\n<!-- herdr-governor handoff run=r1 -->\n");
        let path = self.handoff_path();
        std::fs::create_dir_all(path.parent().expect("dir")).expect("handoff dir");
        std::fs::write(&path, &text).expect("handoff writes");
        text
    }
}

/// Every supervision/acceptance question answered `p` — the sticky set.
pub(super) fn answers(p: f64) -> Vec<(&'static str, Answer)> {
    [
        "blocked_on_input",
        "no_recent_progress",
        "outside_scope",
        "provider_limited",
        "handoff_meets_item_0",
        "handoff_meets_item_1",
    ]
    .into_iter()
    .map(|name| (name, Answer::noul(p)))
    .collect()
}

/// The transcript texts a captured ask carries.
pub(super) fn texts(ask: &Value) -> Vec<String> {
    ask["transcript"]
        .as_array()
        .expect("transcript list")
        .iter()
        .filter_map(|line| line["text"].as_str().map(str::to_owned))
        .collect()
}

fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-caller-1".into()),
    }
}

fn occupant(
    name: &str,
    kind: &str,
    session_kind: SessionKind,
    session: &str,
    status: &str,
) -> Occupant {
    Occupant {
        name: name.to_owned(),
        kind: kind.to_owned(),
        status: status.to_owned(),
        session: Some(SessionRef {
            kind: session_kind,
            value: session.to_owned(),
        }),
    }
}

fn run_row(cwd: &Path, base_commit: Option<String>) -> Run {
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
        provider: None,
        tier_start: None,
        cwd: cwd.to_string_lossy().into_owned(),
        base_commit,
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

fn launch_row(done_when: &[String], phase: LaunchPhase) -> Launch {
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
            done_when: done_when.to_vec(),
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
