//! `world` — the `herdr_run` e2e fixture: the topology every envelope
//! resolves to (the caller on `w1:p1`, the supervised child `gov-r1`
//! on `w1:p2`, a verified handover successor on `w1:p3`, a foreign
//! caller on `w1:p4`), the catalog builders, `FakeHerdr` + `FakeJev` +
//! `daemon::run` in-process, and the `herdr_run`/`herdr_launch` wire
//! helpers. Re-exported into the `e2e_run` root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use governor_core::config::{Capability, OperatingPointId, Qualification, args_digest};
use governor_core::identity::{AgentKind, CallerEnvelope, CallerKey, NativeSession};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::store::Store;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::task::JoinHandle;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, fixture};
use crate::support::fake_herdr::FakeHerdr;
use crate::support::fake_herdr::topology::{Occupant, SessionRef, Topology};
use crate::support::fake_jev::{Answer, CapturedRequest, FakeJev};
use crate::support::mcp_client::{McpClient, caller_envelope};
pub(crate) use crate::support::mcp_client::{tool_body, tool_code};

use super::{
    CALLER_PANE, CHILD_PANE, CHILD_RELAY, FOREIGN_PANE, FOREIGN_RELAY, NOW, RELAY, SUCC_PANE,
    SUCC_RELAY, TICK_SECS, open_store,
};

// — Scripted world ————————————————————————————————————————————————————

/// One occupant shorthand.
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

/// `single_shell` plus: the `kind-a`/`caller-session` caller on `w1:p1`
/// (every owner-bound action resolves through it), the child `gov-r1`
/// on `w1:p2`, the verified successor `kind-b`/`sess-succ` on `w1:p3`
/// and the foreign caller `kind-b`/`sess-foreign` on `w1:p4` — F4's
/// not-the-owner lane and F19's previous-owner lane.
pub(crate) fn run_topology() -> Topology {
    let mut topology = Topology::single_shell();
    topology.panes[0].agent = Some(occupant("caller", "kind-a", "caller-session", "working"));
    for (name, kind, session, status) in [
        ("gov-r1", "kind-a", "sess-r1", "working"),
        ("succ", "kind-b", "sess-succ", "idle"),
        ("foreign", "kind-b", "sess-foreign", "idle"),
    ] {
        let (_tab, pane_id) = topology.create_tab("w1");
        let pane = topology
            .panes
            .iter_mut()
            .find(|row| row.pane_id == pane_id)
            .expect("scripted pane");
        pane.agent = Some(occupant(name, kind, session, status));
    }
    topology
}

/// The `CallerKey` the `w1:p1` occupant resolves to — every seeded Run's
/// owner unless a test says otherwise.
pub(crate) fn caller_key() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("caller-session".into()),
    }
}

/// The `CallerKey` the `w1:p4` occupant resolves to — F19's previous
/// owner and F4's foreign lane.
pub(crate) fn foreign_key() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-b".into()),
        native_session: NativeSession("sess-foreign".into()),
    }
}

/// The `CallerKey` the `w1:p3` successor resolves to.
pub(crate) fn succ_key() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-b".into()),
        native_session: NativeSession("sess-succ".into()),
    }
}

/// The caller envelope for `pane` at `project_root` (canonical — H#3)
/// over `relay` — one relay per caller (`relay_bindings` UNIQUE).
pub(crate) fn envelope(pane: &str, relay: &str, project_root: &Path) -> CallerEnvelope {
    caller_envelope(pane, project_root.to_str().expect("utf8 root"), relay)
}

/// `path` canonicalized — the spelling `projectRoot`/`task.cwd` carry.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

/// The relay each scripted pane's client dials — RELAY is the caller's
/// alone; a second caller on it is an identity mismatch, never a bind.
pub(crate) fn pane_relay(pane: &str) -> &'static str {
    match pane {
        CALLER_PANE => RELAY,
        CHILD_PANE => CHILD_RELAY,
        SUCC_PANE => SUCC_RELAY,
        FOREIGN_PANE => FOREIGN_RELAY,
        other => panic!("no scripted relay for {other}"),
    }
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
/// the F15 dispatch-commit gate's precondition.
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
/// `related_tab` (`"new"` is always a legal label) over the offered
/// label set — the successor recovery's ask is the same shape.
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

// — The world —————————————————————————————————————————————————————————

/// The shared fixture: topology + `FakeHerdr` + `FakeJev` + catalog +
/// an `McpClient` per caller envelope, rooted at the canonical
/// `project`. `start*` bring the daemon up.
pub(crate) struct World {
    /// The project root's tempdir guard — `Drop` removes it; nothing
    /// reads the path twice.
    #[expect(
        dead_code,
        reason = "the TempDir guard — its Drop removes the project root"
    )]
    scratch: TempDir,
    fake: FakeHerdr,
    jev: FakeJev,
    dirs: DaemonDirs,
    daemon: Option<TestDaemon>,
    /// The canonical project root the envelopes name.
    project: PathBuf,
    /// The `w1:p1` caller's client.
    client: Arc<McpClient>,
    /// `w1:p2`'s `terminal_id` — the F2 locator the seeded child's
    /// identity captures (the same one the daemon reads at the wire).
    child_terminal: String,
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

    /// The canonical project root the envelopes name.
    pub(crate) fn project(&self) -> &Path {
        &self.project
    }

    /// `run_topology` plus a catalog `edit` over the defaults
    /// (`standard` tier, one-second tick, no points).
    pub(crate) fn new(edit: impl FnOnce(&mut Catalog)) -> Self {
        Self::build(run_topology(), edit)
    }

    /// `topology` plus a catalog `edit` applied over the defaults.
    pub(crate) fn build(topology: Topology, edit: impl FnOnce(&mut Catalog)) -> Self {
        let child_terminal = topology
            .pane(CHILD_PANE)
            .expect("the scripted child pane")
            .terminal_id
            .clone();
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
        let client = Arc::new(McpClient::new(
            &dirs.socket_path(),
            envelope(CALLER_PANE, RELAY, &project),
        ));
        Self {
            scratch,
            fake,
            jev,
            dirs,
            daemon: None,
            project,
            client,
            child_terminal,
        }
    }

    /// `w1:p2`'s `terminal_id` — feeds `child_identity`/`child`.
    pub(crate) fn child_terminal(&self) -> &str {
        &self.child_terminal
    }

    /// The Jev asks captured so far whose `state` is a launch Task —
    /// the evaluation calls a launch or its recovery successor makes.
    pub(crate) fn evals(&self) -> Vec<CapturedRequest> {
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

    /// `daemon::run` in-process — the socket binds before this returns.
    pub(crate) async fn start(&mut self) {
        self.daemon = Some(TestDaemon::start_in_process(&self.dirs.settings(), None).await);
    }

    /// The side-store handle over the daemon's `governor.db`.
    pub(crate) fn store(&self) -> Store {
        open_store(self.dirs.state_dir())
    }

    /// `<state>` — the path `wait_store` polls.
    pub(crate) fn state(&self) -> PathBuf {
        self.dirs.state_dir().to_path_buf()
    }

    /// A client for a second pane — the foreign caller's `w1:p4`
    /// envelope or another `projectRoot` scope. `relay` is that
    /// caller's own: a relay already bound to another caller resolves
    /// `CALLER_IDENTITY_MISMATCH`, never the pane's occupant.
    pub(crate) fn client_for(&self, pane: &str, relay: &str, project_root: &Path) -> McpClient {
        McpClient::new(
            &self.dirs.socket_path(),
            envelope(pane, relay, project_root),
        )
    }

    /// `herdr_launch` on its own task — the cases that hold the call
    /// while a fault window opens (`S31b`, the orphan start).
    pub(crate) fn spawn_launch(&self, args: &Value) -> JoinHandle<Value> {
        let client = Arc::clone(&self.client);
        let owned = args.clone();
        tokio::spawn(async move { client.call_tool(json!(1), "herdr_launch", owned).await })
    }

    /// `tools/call herdr_run` — resolves when the reply does (a
    /// `closePane` reply may ride the close's wait bound).
    pub(crate) async fn run_call(&self, args: &Value) -> Value {
        self.client
            .call_tool(json!(1), "herdr_run", args.clone())
            .await
    }

    /// Graceful stop — asserts the clean exit like every `TestDaemon`.
    pub(crate) async fn shutdown(self) {
        if let Some(daemon) = self.daemon {
            daemon.shutdown().await;
        }
    }
}
