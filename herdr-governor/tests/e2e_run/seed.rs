//! `seed` — the durable-row builders and store reads the `e2e_run`
//! cases share: `changes` wraps bare `StateChange`s into a `Transition`,
//! `run_row`/`launch_row`/`launch_chain` build the seeded shapes,
//! `seed_outbox`/`seed_mailbox`/`seed_obligation` fill F6/F18/F21's
//! rows, and the bounded polls read the daemon's `governor.db` through
//! a side connection. Re-exported into the `e2e_run` root.

use std::path::Path;
use std::time::{Duration, Instant};

use governor_core::delivery::{
    FollowUpWrite, MailboxEvent, MailboxEventKind, MailboxSubject, MessageBody, OutboxMessage,
    OutboxState,
};
use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, Digest, EffectKey, EventId,
    HerdrIncarnation, IdempotencyKey, LaunchId, MessageKey, NativeSession, PaneId, ProjectRoot,
    RelayInstanceId, RunId, TerminalId,
};
use governor_core::lifecycle::{Effect, Run, State, StateChange, Transition};
use governor_core::recovery::{RecoveryObligation, RecoveryOrigin};
use governor_core::task::{Launch, LaunchOutcome, LaunchPhase, Task};
use herdr_governor::store::Store;

use crate::support::daemon::socket_incarnation;
use crate::support::fake_herdr::FakeHerdr;

use super::{CALLER_PANE, CHILD_PANE, DEADLINE, FAR, NOW, RELAY, caller_key};

pub(crate) fn changes(state_changes: Vec<StateChange>) -> Transition {
    Transition {
        state_changes,
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// One `apply` of row writes plus mailbox events.
pub(crate) fn seed(store: &mut Store, state_changes: Vec<StateChange>, events: Vec<MailboxEvent>) {
    store
        .apply(
            &Transition {
                state_changes,
                events,
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("seed apply");
}

// — Rows ——————————————————————————————————————————————————————————————

/// The `Task` a `launch_row` carries.
pub(crate) fn seeded_task(objective: &str) -> Task {
    Task {
        objective: objective.into(),
        scope: "src/".into(),
        done_when: vec!["cargo test passes".into()],
        constraints: Vec::new(),
        tier: None,
        recovery_of: None,
        label: None,
        cwd: None,
        retention: None,
    }
}

/// A `launches` row for seeding — `phase`, caller, root `/p`, key `k-<id>`.
pub(crate) fn launch_row(id: &str, phase: LaunchPhase) -> Launch {
    let task = seeded_task("land the green refactor");
    Launch {
        id: LaunchId(id.into()),
        caller: caller_key(),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(format!("k-{id}")),
        digest_version: Task::DIGEST_VERSION,
        task_digest: task.digest(),
        task,
        phase,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

/// The `RecordLaunch` chain a non-`evaluating` phase seed needs — every
/// later phase is a guarded UPDATE on a legal predecessor, so the seed
/// walks the path rather than jumping.
pub(crate) fn launch_chain(launch: &Launch) -> Vec<StateChange> {
    if launch.phase == LaunchPhase::Evaluating {
        return vec![StateChange::RecordLaunch(launch.clone())];
    }
    let mut first = launch.clone();
    first.phase = LaunchPhase::Evaluating;
    first.decision = None;
    first.config_version = None;
    first.outcome = None;
    vec![
        StateChange::RecordLaunch(first),
        StateChange::RecordLaunch(launch.clone()),
    ]
}

/// The `failed` outcome most predecessor seeds end on — `unknown`
/// certainty (the crash's reach is unprovable), `run` bound.
pub(crate) fn failed_outcome(run: &str) -> LaunchOutcome {
    LaunchOutcome::Failed {
        certainty: governor_core::lifecycle::EffectCertainty::Unknown,
        run: Some(RunId(run.into())),
        created_topology: governor_core::lifecycle::CreatedTopology {
            tab: None,
            panes: Vec::new(),
        },
    }
}

/// A `done` Launch — the `runs.launch_id` FK's predecessor row the
/// schema CHECK wants an outcome on.
pub(crate) fn done_launch(id: &str, outcome: LaunchOutcome) -> Launch {
    let mut row = launch_row(id, LaunchPhase::Done);
    row.outcome = Some(outcome);
    row
}

/// A `runs` row for seeding — `state`, owner the caller, no identity,
/// `/p` cwd and a `max_age` the suite never reaches. Mutate the fields a
/// case needs (provider/tier/identity) before `ReserveRun`.
pub(crate) fn run_row(id: &str, launch: &str, state: State) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(launch.into()),
        owner: caller_key(),
        owner_generation: 0,
        version: 0,
        state,
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

/// The `ChildIdentity` the supervised `gov-r1` on `w1:p2` reports —
/// `terminal` is the pane row's `terminal_id`, read off the scripted
/// topology before `FakeHerdr::start`; the incarnation is the fake's
/// socket stat, exactly the daemon's own derivation (F2/OQ-8).
pub(crate) fn child_identity(fake: &FakeHerdr, terminal: &str, name: &str) -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal.to_owned()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName(name.to_owned()),
        native_session: Some(NativeSession("sess-r1".into())),
        pane_id: PaneId(CHILD_PANE.to_owned()),
    }
}

/// One `BindCaller` for `key` — the `callers`/`relay_bindings` FK every
/// owned row and every ownership write resolves against. `relay` is the
/// binding's `relay_instance_id`: `relay_bindings` is UNIQUE on it, so a
/// seeded-only caller takes its own id (it never dials in).
pub(crate) fn bind(key: &CallerKey, relay: &str, pane: &str) -> StateChange {
    StateChange::BindCaller(CallerBinding {
        caller: key.clone(),
        relay_instance: RelayInstanceId(relay.into()),
        pane_at_bind: PaneId(pane.to_owned()),
    })
}

/// The caller's own binding — the seeds' FK target and the real client's
/// relay (the envelope `client`/`client_for` present).
pub(crate) fn bind_caller(store: &mut Store) {
    seed(
        store,
        vec![bind(&caller_key(), RELAY, CALLER_PANE)],
        Vec::new(),
    );
}

/// Seed `run` plus its launch row (chain-walked through `evaluating`).
pub(crate) fn seed_run(store: &mut Store, run: &Run, launch: &Launch) {
    let mut writes = launch_chain(launch);
    writes.push(StateChange::ReserveRun(run.clone()));
    seed(store, writes, Vec::new());
}

/// A `queued` outbox entry for `run` — `seq` and `message_key` are the
/// page's ordering and key columns.
pub(crate) fn seed_outbox(store: &mut Store, run: &RunId, seq: u64, key: &str, body: &str) {
    let text = body.to_owned();
    seed(
        store,
        vec![StateChange::WriteFollowUp(FollowUpWrite::Enqueue(
            OutboxMessage {
                run: run.clone(),
                seq,
                message_key: MessageKey(key.into()),
                sender: caller_key(),
                body_digest: Digest([seq.to_le_bytes()[0]; 32]),
                body: MessageBody::Inline(text),
                state: OutboxState::Queued,
                effect: None,
                expiry_reason: None,
            },
        ))],
        Vec::new(),
    );
}

/// One `queued` mailbox event bound to `run` (`MailboxEvent::emitted`
/// derives the dedup key) — F19's "unread events" and F6's `ack`
/// target. `qualifier` is the recurrent kinds' episode suffix.
pub(crate) fn seed_mailbox(
    store: &mut Store,
    run: &RunId,
    kind: MailboxEventKind,
    qualifier: Option<u64>,
    id: &str,
) {
    let event = MailboxEvent::emitted(
        EventId(id.into()),
        MailboxSubject::Run(run.clone()),
        kind,
        qualifier,
        "{}".into(),
    )
    .expect("a legal run-subject event");
    seed(store, Vec::new(), vec![event]);
}

/// A `pending` recovery obligation for `predecessor` — `expiry` from
/// `NOW` sets whether the daemon's real clock sees it elapsed (the
/// sweep cases seed a past expiry deliberately).
pub(crate) fn seed_obligation(store: &mut Store, predecessor: &RunId, expiry: Duration) {
    let obligation = RecoveryObligation::pending(
        predecessor.clone(),
        RecoveryOrigin::ProviderLimit,
        NOW,
        expiry,
    );
    seed(
        store,
        vec![StateChange::RecordRecovery(obligation)],
        Vec::new(),
    );
}

// — Store reads ———————————————————————————————————————————————————————

pub(crate) fn open_store(state: &Path) -> Store {
    Store::open(&state.join("governor.db")).expect("store opens")
}

/// Every launch row, any phase.
pub(crate) fn all_launches(store: &Store) -> Vec<Launch> {
    [
        LaunchPhase::Evaluating,
        LaunchPhase::Routed,
        LaunchPhase::Launching,
        LaunchPhase::Done,
    ]
    .iter()
    .flat_map(|phase| store.launches_in_phase(*phase).expect("launches read"))
    .collect()
}

/// The one launch whose idempotency key starts with `prefix`.
pub(crate) fn launch_at(store: &Store, prefix: &str) -> Launch {
    let found: Vec<Launch> = all_launches(store)
        .into_iter()
        .filter(|launch| launch.idempotency_key.0.starts_with(prefix))
        .collect();
    assert_eq!(found.len(), 1, "exactly one launch keyed {prefix}*");
    found.into_iter().next().expect("one match")
}

/// The one launch the test's store holds.
pub(crate) fn only_launch(store: &Store) -> Launch {
    launch_at(store, "")
}

pub(crate) fn run_for(store: &Store, launch: &Launch) -> Run {
    store
        .run_by_launch(&launch.id)
        .expect("run read")
        .expect("the launch reserved a run")
}

pub(crate) fn effect_at(store: &Store, key: &str) -> Effect {
    store
        .effect(&EffectKey(key.into()))
        .expect("effect read")
        .unwrap_or_else(|| panic!("effect row {key}"))
}

/// `run:<id>:<suffix>` — the journal-key spelling `launch_plan` and
/// `plan_agent_start` use.
pub(crate) fn run_key(run: &Run, suffix: &str) -> String {
    format!("run:{}:{suffix}", run.id.0)
}

/// The caller's unacked mailbox events of `kind`.
pub(crate) fn caller_events(store: &Store, kind: MailboxEventKind) -> Vec<MailboxEvent> {
    store
        .mailbox_unacked(&caller_key(), None, 500)
        .expect("mailbox read")
        .into_iter()
        .filter(|event| event.kind == kind)
        .collect()
}

// — Bounded polls —————————————————————————————————————————————————————

/// Poll a fresh side-store view until `until` yields or `DEADLINE`
/// passes — `tokio::time::sleep` yields to the daemon and fakes on the
/// same runtime.
pub(crate) async fn wait_store<T>(
    state: &Path,
    what: &str,
    mut until: impl FnMut(&Store) -> Option<T>,
) -> T {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        let store = open_store(state);
        let value = until(&store);
        drop(store);
        if let Some(found) = value {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}
