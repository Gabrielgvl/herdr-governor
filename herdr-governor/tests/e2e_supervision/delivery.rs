//! `delivery` — the P5.C2 e2e (§4.8, F17/F18 + S12/S25): `daemon::run`
//! against `FakeHerdr`, follow-ups admitted through the real
//! `herdr_run message` tool call, rows and mailbox events seeded through
//! a second `Store` connection on the same `governor.db` (the reconcile
//! suite's pattern — strictly post-bind where a seed must be invisible
//! to §4.3 step 5). Every wait is a bounded poll; nothing sleeps on the
//! wall clock. S3's transcript `delivery-id:` lift is C5's — the restart
//! test in `f17` stops at `unconfirmed` + `follow_up_unconfirmed`, the
//! C2 half. The cases live in `f17`/`f18` children; the fixtures below
//! are theirs.

mod f17;
mod f18;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use governor_core::config::{Capability, OperatingPointId, Qualification, args_digest};
use governor_core::delivery::{
    ExpiryReason, FollowUpWrite, MailboxEvent, MailboxEventKind, MailboxSubject, MessageBody,
    OutboxMessage, OutboxState,
};
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectId, EffectKey,
    EventId, HerdrIncarnation, IdempotencyKey, LaunchId, MessageKey, NativeSession, PaneId,
    ProjectRoot, RelayInstanceId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectResolution, EffectState, EffectWrite, OwnerChange, Run, Settlement,
    State, StateChange, Transition, UnresolvedReason,
};
use governor_core::task::{Launch, LaunchOutcome, LaunchPhase, Task};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use herdr_governor::store::Store;
use serde_json::{Value, json};

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, await_for, fixture};
use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Topology};
use crate::support::mcp_client::{McpClient, caller_envelope, canonical, tool_body, tool_code};

const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// `max_age_deadline` when the test does not want one: year 2100.
const FAR: Timestamp = Timestamp(4_102_444_800_000);
/// A deadline already passed.
const PAST: Timestamp = Timestamp(1);
/// The envelope's `relayInstanceId` — the 32-lowercase-hex shape the
/// validator requires (the value is free).
const RELAY: &str = "0123456789abcdef0123456789abcdef";
/// `herdr_op` bound for the DropResponse leg — the fake holds the
/// connection until the client deadline, so the leg costs ~3 s.
const OP_TIMEOUT: &str = "herdr_op_timeout_secs = 3\n";

// — Fixture builders —————————————————————————————————————————————————

fn caller(n: u8) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession(format!("sess-caller-{n}")),
    }
}

/// `w1:t1` holds `w1:p1` — the caller pane (kind-a, `sess-caller-1`,
/// `idle`); one fresh tab+pane per `extra` row
/// `(name, kind, session, status)` so the child/second-owner panes are
/// distinct locators.
fn topology(extra: &[(&str, &str, &str, &str)]) -> Topology {
    let mut topology = Topology::single_shell();
    topology.panes[0].agent = Some(occupant("owner-a", "kind-a", "sess-caller-1", "idle"));
    for (name, kind, session, status) in extra {
        let (_, pane_id) = topology.create_tab("w1");
        let pane = topology
            .panes
            .iter_mut()
            .find(|pane| pane.pane_id == *pane_id)
            .expect("created pane");
        pane.agent = Some(occupant(name, kind, session, status));
    }
    topology
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

fn run_row(id: &str, owner: CallerKey) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(format!("l-{id}")),
        owner,
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

/// An `active` Run seeded already `settled` (settlement + settled_at) —
/// a subject row for the hint tests that emits nothing of its own.
fn settled_run(id: &str) -> Run {
    let mut run = run_row(id, caller(1));
    run.state = State::Settled;
    run.settlement = Some(Settlement::Unresolved {
        reason: UnresolvedReason::IdentityUnprovable,
    });
    run.settled_at = Some(NOW);
    run
}

fn launch_row(id: &str, caller_key: &CallerKey, phase: LaunchPhase) -> Launch {
    Launch {
        id: LaunchId(id.into()),
        caller: caller_key.clone(),
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
        outcome: None,
    }
}

/// `w1:pN`'s `terminal_id` — the identity seed needs it verbatim.
fn terminal_of(topology: &Topology, pane: &str) -> String {
    topology
        .pane(pane)
        .expect("scripted pane")
        .terminal_id
        .clone()
}

/// The `HerdrIncarnation` a snapshot over `path` mints (§4.3's
/// `<inode>:<mtime_secs>.<mtime_nsecs>` spelling).
fn socket_incarnation(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).expect("socket stat");
    format!("{}:{}.{:09}", meta.ino(), meta.mtime(), meta.mtime_nsec())
}

// — Store seeds (the second `Store` connection pattern) ———————————————

fn open_store(dirs: &DaemonDirs) -> Store {
    Store::open(&dirs.store_path()).expect("store opens")
}

fn seed(store: &mut Store, changes: Vec<StateChange>, events: Vec<MailboxEvent>) {
    store
        .apply(
            &Transition {
                state_changes: changes,
                events,
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("seed apply");
}

/// `callers` + `relay_bindings` for `n` bound at `w1:p{n}` — the
/// `runs.owner_caller_id` FK demands the row before `ReserveRun`.
fn bind_caller(store: &mut Store, n: u8, pane: &str) {
    seed(
        store,
        vec![StateChange::BindCaller(CallerBinding {
            caller: caller(n),
            relay_instance: RelayInstanceId(format!("relay-{n}")),
            pane_at_bind: PaneId(pane.to_owned()),
        })],
        Vec::new(),
    );
}

/// `evaluating` → `routed` + `ReserveRun` (the `runs.launch_id` FK
/// demands the row first). `run.state` keeps whatever the builder set.
fn seed_run(store: &mut Store, run: &Run) {
    let launch = run.launch.0.clone();
    seed(
        store,
        vec![StateChange::RecordLaunch(launch_row(
            &launch,
            &run.owner,
            LaunchPhase::Evaluating,
        ))],
        Vec::new(),
    );
    seed(
        store,
        vec![
            StateChange::RecordLaunch(launch_row(&launch, &run.owner, LaunchPhase::Routed)),
            StateChange::ReserveRun(run.clone()),
        ],
        Vec::new(),
    );
}

fn outbox_row(run: &RunId, seq: u64, key: &str, body: &str) -> OutboxMessage {
    OutboxMessage {
        run: run.clone(),
        seq,
        message_key: MessageKey(key.to_owned()),
        sender: caller(1),
        body_digest: Digest([0x5a; 32]),
        body: MessageBody::Inline(body.to_owned()),
        state: OutboxState::Queued,
        effect: None,
        expiry_reason: None,
    }
}

/// A queued row (seq), a submitted row (seq+1, its prompt effect
/// journaled terminal `acknowledged` so restart marking never touches
/// it), and a published body file for the queued one when `file` names
/// it.
fn seed_outbox_pair(store: &mut Store, run: &RunId, seq: u64, file: Option<PathBuf>) {
    let next = seq.saturating_add(1);
    let mut queued = outbox_row(run, seq, "k-queued", "q");
    if let Some(path) = file {
        queued.body = MessageBody::File {
            path: path.to_string_lossy().into_owned(),
        };
    }
    let effect = Effect {
        id: EffectId(format!("eff:run:{}:outbox:{}", run.0, next)),
        key: EffectKey(format!("run:{}:outbox:{}", run.0, next)),
        kind: EffectKind::Prompt,
        subject_launch: None,
        subject_run: Some(run.clone()),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    };
    store
        .apply(
            &Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: vec![effect],
            },
            NOW,
        )
        .expect("plan effect");
    seed(
        store,
        vec![
            StateChange::WriteFollowUp(FollowUpWrite::Enqueue(queued)),
            StateChange::WriteFollowUp(FollowUpWrite::Enqueue(outbox_row(
                run, next, "k-done", "d",
            ))),
            StateChange::WriteFollowUp(FollowUpWrite::Dispatch {
                run: run.clone(),
                seq: next,
                effect: EffectId(format!("eff:run:{}:outbox:{}", run.0, next)),
            }),
            StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey(format!("run:{}:outbox:{}", run.0, next)),
            }),
            StateChange::WriteEffect(EffectWrite::Result {
                key: EffectKey(format!("run:{}:outbox:{}", run.0, next)),
                resolution: EffectResolution::Acknowledged { receipt: None },
            }),
            StateChange::WriteFollowUp(FollowUpWrite::Resolve {
                run: run.clone(),
                seq: next,
                state: OutboxState::Submitted,
            }),
        ],
        Vec::new(),
    );
}

/// A passed `qualifications` row for `point`'s current args — the F26
/// gate `point_caps` re-checks against.
fn qualify(store: &mut Store, point: &str, args: &[&str], capability: &str) {
    store
        .record_qualification(
            &Qualification {
                operating_point: OperatingPointId(point.to_owned()),
                args_digest: args_digest(args.iter().copied()),
                capability: Capability(capability.to_owned()),
                passed: true,
                evidence: "{}".to_owned(),
            },
            NOW,
        )
        .expect("qualify");
}

fn emit(store: &mut Store, event: MailboxEvent) {
    seed(store, Vec::new(), vec![event]);
}

fn mailbox_event(
    id: &str,
    subject: MailboxSubject,
    kind: MailboxEventKind,
    qualifier: Option<u64>,
) -> MailboxEvent {
    MailboxEvent::emitted(
        EventId(id.to_owned()),
        subject,
        kind,
        qualifier,
        "{}".into(),
    )
    .expect("dedup key derivable")
}

// — Reads and polls ——————————————————————————————————————————————————

fn outbox(dirs: &DaemonDirs, run: &str) -> Vec<OutboxMessage> {
    open_store(dirs)
        .outbox(&RunId(run.to_owned()))
        .expect("outbox read")
}

fn journal(dirs: &DaemonDirs, run: &str) -> Vec<Effect> {
    open_store(dirs)
        .journal(&RunId(run.to_owned()))
        .expect("journal read")
}

fn unacked(dirs: &DaemonDirs, n: u8) -> Vec<MailboxEvent> {
    open_store(dirs)
        .mailbox_unacked(&caller(n), None, 100)
        .expect("mailbox read")
}

/// `agent.prompt` requests the fake accepted, `(target, text)` pairs.
fn prompts(fake: &FakeHerdr) -> Vec<(String, String)> {
    fake.requests()
        .iter()
        .filter(|(method, _)| method == "agent.prompt")
        .map(|(_, params)| {
            (
                params["target"].as_str().expect("prompt target").to_owned(),
                params["text"].as_str().expect("prompt text").to_owned(),
            )
        })
        .collect()
}

fn prompts_on(fake: &FakeHerdr, pane: &str) -> Vec<(String, String)> {
    prompts(fake)
        .into_iter()
        .filter(|(target, _)| target == pane)
        .collect()
}

/// One `herdr_run message` call through a fresh socket frame.
async fn message(client: &McpClient, id: i64, run: &str, key: &str, text: &str) -> Value {
    client
        .call_tool(
            json!(id),
            "herdr_run",
            json!({"action": "message", "runId": run, "messageKey": key, "text": text}),
        )
        .await
}

fn run_client(dirs: &DaemonDirs, daemon: &TestDaemon, pane: &str) -> McpClient {
    McpClient::new(
        &daemon.socket_path(),
        caller_envelope(pane, &canonical(dirs.root()), RELAY),
    )
}

/// The catalog: `[daemon]` naming the fakes plus `pt-a` — a kind-a
/// operating point claiming every capability the suite qualifies.
fn catalog(fake: &FakeHerdr) -> Catalog {
    let mut catalog = Catalog::new(fake.socket_path(), "http://127.0.0.1:9");
    catalog.points_toml = concat!(
        "[[catalog.operating_points]]\n",
        "id = \"pt-a\"\n",
        "harness = \"kind-a\"\n",
        "args = [\"--cap\"]\n",
        "tier = \"fast\"\n",
        "capabilities = [\"start\", \"mid_turn_input\", \"hint_consumption\"]\n",
        "cost_class = 0\n",
        "provider = \"vendor-a\"\n",
    )
    .to_owned();
    catalog
}
