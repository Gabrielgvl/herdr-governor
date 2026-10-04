//! `world` — the launch e2e fixture: the caller topology every envelope
//! resolves to, the catalog builder, `FakeHerdr` + `FakeJev` +
//! `daemon::run` (in-process or a seamed child), the `herdr_launch`
//! wire helpers and the Jev answer scripts. Re-exported into the
//! `e2e_launch` root, so the case modules use the names unprefixed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use governor_core::config::{Capability, OperatingPointId, Qualification, args_digest};
use governor_core::identity::{AgentKind, CallerEnvelope, CallerKey, NativeSession};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::SeamConfig;
use herdr_governor::mcp::framing::encode_request;
use herdr_governor::store::Store;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, fixture};
use crate::support::fake_herdr::FakeHerdr;
use crate::support::fake_herdr::topology::{Occupant, SessionRef, Topology};
use crate::support::fake_jev::{Answer, FakeJev};
use crate::support::mcp_client::{McpClient, caller_envelope};

use super::{CALLER_PANE, NOW, RELAY, TICK_SECS, open_store};

// — Scripted world ————————————————————————————————————————————————————

/// `single_shell` with an agent occupant on `w1:p1` — the caller every
/// envelope resolves to (`kind-a` + `caller-session`, one open tab
/// `w1:t1`).
pub(crate) fn caller_topology() -> Topology {
    let mut topology = Topology::single_shell();
    let pane = topology.panes.first_mut().expect("shell pane");
    pane.agent = Some(Occupant {
        name: "caller".to_owned(),
        kind: "kind-a".to_owned(),
        status: "working".to_owned(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: "caller-session".to_owned(),
        }),
    });
    topology
}

/// The `CallerKey` the scripted occupant resolves to.
pub(crate) fn caller_key() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("caller-session".into()),
    }
}

/// The caller envelope for `w1:p1` at `project_root` (canonical — H#3).
fn caller(project_root: &Path) -> CallerEnvelope {
    caller_envelope(
        CALLER_PANE,
        project_root.to_str().expect("utf8 root"),
        RELAY,
    )
}

/// `path` canonicalized — the spelling `projectRoot`/`task.cwd` carry.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

// — Catalog ———————————————————————————————————————————————————————————

/// One `standard`-tier `[[catalog.operating_points]]` member.
pub(crate) fn point(id: &str, cost: u32, provider: &str, args: &str) -> String {
    point_at(id, "standard", cost, provider, args)
}

/// One `[[catalog.operating_points]]` member at `tier`, verbatim TOML.
pub(crate) fn point_at(id: &str, tier: &str, cost: u32, provider: &str, args: &str) -> String {
    format!(
        "[[catalog.operating_points]]\nid = \"{id}\"\nharness = \"kind-a\"\n\
         args = [\"{args}\"]\ntier = \"{tier}\"\ncapabilities = [\"start\"]\n\
         cost_class = {cost}\nprovider = \"{provider}\"\n"
    )
}

/// Seed a `passed` `start` qualification for one point's exact args —
/// the F15 dispatch-commit gate's precondition (WAL tolerates the
/// daemon's own handle).
pub(crate) fn qualify_start(store: &mut Store, point_id: &str, args: &[&str]) {
    store
        .record_qualification(
            &Qualification {
                operating_point: OperatingPointId(point_id.into()),
                args_digest: args_digest(args.iter().copied()),
                capability: Capability(Capability::START.into()),
                passed: true,
                evidence: "{}".into(),
            },
            NOW,
        )
        .expect("record qualification");
}

// — Wire calls ————————————————————————————————————————————————————————

/// The `herdr_launch` arguments (§6.2's strict DTO).
pub(crate) fn launch_args(task: &Value, key: &str) -> Value {
    json!({"task": task, "idempotencyKey": key})
}

/// A minimal valid Task — the F5 required fields plus `extras`.
pub(crate) fn task(extras: &[(&str, Value)]) -> Value {
    let mut task = json!({
        "objective": "land the green refactor",
        "scope": "src/",
        "doneWhen": ["cargo test passes"],
    });
    let object = task.as_object_mut().expect("task object");
    for (name, value) in extras {
        object.insert((*name).to_owned(), value.clone());
    }
    task
}

/// The `content[0].text` body decoded — `{"outcome":…}` on success,
/// `{"code":…,"message":…}` on `isError`.
pub(crate) fn tool_body(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tool text body: {reply}"));
    serde_json::from_str(text).expect("tool body is json")
}

/// The typed refusal code an `isError` result carries.
pub(crate) fn tool_code(reply: &Value) -> String {
    assert_eq!(reply["result"]["isError"], true, "a refusal: {reply}");
    tool_body(reply)["code"]
        .as_str()
        .expect("refusal code")
        .to_owned()
}

/// Every wire request of `method` the fake saw, params only.
pub(crate) fn wire_calls(fake: &FakeHerdr, method: &str) -> Vec<Value> {
    fake.requests()
        .into_iter()
        .filter(|(m, _)| m == method)
        .map(|(_, params)| params)
        .collect()
}

/// Whether the fake saw any `method` request.
pub(crate) fn saw_wire(fake: &FakeHerdr, method: &str) -> bool {
    !wire_calls(fake, method).is_empty()
}

// — Jev scripts ———————————————————————————————————————————————————————

/// The launch evaluation: the six always-asked questions plus
/// `related_tab` (the caller's `w1:t1` is always open here) over the
/// exact offered label set (CT-JEV-RESP-2) — `done_when_verifiable` yes,
/// `standard` sufficient, `few` files, no boundary/external/long-running.
pub(crate) fn launch_eval(related_tab: &str) -> Vec<(String, Answer)> {
    let joined = if related_tab == "new" { 0.2 } else { 0.8 };
    vec![
        ("done_when_verifiable".to_owned(), Answer::noul(0.9)),
        (
            "weakest_sufficient_tier".to_owned(),
            Answer::choice("standard", &[("standard", 0.8)]),
        ),
        (
            "changes_files".to_owned(),
            Answer::choice("few", &[("none", 0.1), ("few", 0.8), ("broad", 0.1)]),
        ),
        ("security_boundary".to_owned(), Answer::noul(0.1)),
        ("needs_external".to_owned(), Answer::noul(0.1)),
        ("long_running".to_owned(), Answer::noul(0.1)),
        (
            "related_tab".to_owned(),
            Answer::choice(related_tab, &[("w1:t1", joined), ("new", 1.0 - joined)]),
        ),
    ]
}

/// `answers` with `question`'s scripted answer replaced.
pub(crate) fn with_answer(
    mut answers: Vec<(String, Answer)>,
    question: &str,
    answer: &Answer,
) -> Vec<(String, Answer)> {
    for (name, slot) in &mut answers {
        if name == question {
            *slot = answer.clone();
        }
    }
    answers
}

// — The world —————————————————————————————————————————————————————————

/// The shared fixture: topology + `FakeHerdr` + `FakeJev` + catalog +
/// an `McpClient` rooted at the canonical `project`. `start*` bring the
/// daemon up.
pub(crate) struct World {
    scratch: TempDir,
    fake: FakeHerdr,
    jev: FakeJev,
    dirs: DaemonDirs,
    daemon: Option<TestDaemon>,
    /// The canonical project root the envelope names.
    project: PathBuf,
    client: Arc<McpClient>,
}

impl World {
    /// The fake Herdr — knobs, faults and the request log.
    pub(crate) fn fake(&self) -> &FakeHerdr {
        &self.fake
    }

    /// The fake Jev — scripted answers, faults and captured asks.
    pub(crate) fn jev(&self) -> &FakeJev {
        &self.jev
    }

    /// The captured Jev asks that are launch evaluations — `state.task`
    /// marks them; C3's supervision families nest under `review`,
    /// `blocked` and `acceptance`. Supervision is live on the
    /// one-second tick, so evaluation counts filter it out.
    pub(crate) fn evals(&self) -> Vec<crate::support::fake_jev::CapturedRequest> {
        self.jev
            .requests()
            .into_iter()
            .filter(|request| {
                request
                    .state()
                    .is_some_and(|state| state.get("task").is_some())
            })
            .collect()
    }

    /// The daemon's state/config dirs.
    pub(crate) fn dirs(&self) -> &DaemonDirs {
        &self.dirs
    }

    /// The canonical project root the envelope names.
    pub(crate) fn project(&self) -> &Path {
        &self.project
    }

    /// The caller topology, a `standard`-tier catalog of `points_toml`
    /// and the `[daemon]` extras.
    pub(crate) fn new(points_toml: &str, daemon_extra: &str) -> Self {
        Self::build(caller_topology(), |catalog| {
            catalog.points_toml = points_toml.to_owned();
            catalog.daemon_extra = daemon_extra.to_owned();
        })
    }

    /// `topology` plus a catalog `edit` applied over the defaults
    /// (`standard` tier, one-second tick, no points).
    pub(crate) fn build(topology: Topology, edit: impl FnOnce(&mut Catalog)) -> Self {
        let scratch = tempdir().expect("tmp");
        let fake = FakeHerdr::start(topology);
        let jev = FakeJev::start();
        let mut catalog = Catalog::new(fake.socket_path(), jev.base_url());
        catalog.reconcile_secs = TICK_SECS;
        catalog.tiers = vec!["standard".to_owned()];
        edit(&mut catalog);
        let dirs = fixture(&catalog);
        let project_dir = scratch.path().join("proj");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let project = canonical(&project_dir);
        let client = Arc::new(McpClient::new(&dirs.socket_path(), caller(&project)));
        Self {
            scratch,
            fake,
            jev,
            dirs,
            daemon: None,
            project,
            client,
        }
    }

    /// A scratch directory outside the project root.
    pub(crate) fn outside(&self) -> PathBuf {
        let dir = self.scratch.path().join("outside");
        std::fs::create_dir_all(&dir).expect("outside dir");
        canonical(&dir)
    }

    /// `daemon::run` in-process — the socket binds before this returns.
    pub(crate) async fn start(&mut self) {
        self.daemon = Some(TestDaemon::start_in_process(&self.dirs.settings(), None).await);
    }

    /// `daemon::run` in-process with an armed seam (`pause` cases).
    pub(crate) async fn start_seamed(&mut self, seam: SeamConfig) {
        self.daemon = Some(TestDaemon::start_in_process(&self.dirs.settings(), Some(seam)).await);
    }

    /// The real binary as a child with an armed seam (`abort` cases).
    pub(crate) async fn spawn_child(&mut self, seam: SeamConfig) {
        self.daemon = Some(TestDaemon::spawn_child(&self.dirs.settings(), Some(seam)).await);
    }

    /// Wait for the child daemon to exit on its own (the seam's abort);
    /// returns its stderr — the `seam hit` marker's home.
    pub(crate) async fn wait_child(&mut self) -> String {
        let daemon = self.daemon.take().expect("a child daemon");
        let (_status, stderr) = daemon.wait().await;
        stderr
    }

    /// The side-store handle over the daemon's `governor.db`.
    pub(crate) fn store(&self) -> Store {
        open_store(self.dirs.state_dir())
    }

    /// `<state>` — the path `wait_store` polls.
    pub(crate) fn state(&self) -> PathBuf {
        self.dirs.state_dir().to_path_buf()
    }

    /// `tools/call herdr_launch` — resolves when the reply does.
    pub(crate) async fn launch(&self, args: &Value) -> Value {
        self.client
            .call_tool(json!(1), "herdr_launch", args.clone())
            .await
    }

    /// `herdr_launch` on its own task — concurrent and seam-held calls.
    pub(crate) fn spawn_launch(&self, args: &Value) -> JoinHandle<Value> {
        let client = Arc::clone(&self.client);
        let owned = args.clone();
        tokio::spawn(async move { client.call_tool(json!(1), "herdr_launch", owned).await })
    }

    /// `herdr_launch` against a daemon about to die: the frame is
    /// written and the connection drained to EOF without asserting a
    /// reply — the crash, not the caller, is what those cases assert.
    pub(crate) fn fire_launch(&self, args: &Value) -> JoinHandle<()> {
        let sock = self.dirs.socket_path();
        let frame = encode_request(
            &caller(&self.project),
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "herdr_launch", "arguments": args}}),
        );
        tokio::spawn(async move {
            let Ok(mut stream) = UnixStream::connect(&sock).await else {
                return;
            };
            if stream.write_all(&frame).await.is_ok() {
                let mut sink = Vec::new();
                let _eof = stream.read_to_end(&mut sink).await;
            }
        })
    }

    /// Graceful stop — asserts the clean exit like every `TestDaemon`.
    pub(crate) async fn shutdown(self) {
        if let Some(daemon) = self.daemon {
            daemon.shutdown().await;
        }
    }
}
