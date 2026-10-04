//! `recovery` — the F21 composition hooks B2 owns: a `recoveryOf`
//! launch records under `recovery:<predecessor>` with a `pending`
//! caller-origin obligation, which moves to `dispatched` inside the
//! successor's routing transaction or to `blocked` with its abstention —
//! each move with its mailbox event; the admission refusals; and F13
//! step 4's recovery minimum (one tier above the predecessor's start,
//! never its provider).

use std::time::Duration;

use governor_core::config::{OperatingPointId, Policy, Provider, Tier};
use governor_core::delivery::MailboxEventKind;
use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, NativeSession, PaneId, RelayInstanceId, RunId,
};
use governor_core::lifecycle::{
    CreatedTopology, EffectCertainty, Settlement, State, StateChange, UnresolvedReason, settle,
};
use governor_core::recovery::RecoveryStatus;
use governor_core::task::{Launch, LaunchOutcome, LaunchPhase};
use herdr_governor::store::Store;
use serde_json::{Value, json};

use crate::support::fake_jev::{Answer, Fault};
use crate::support::mcp_client::{McpClient, caller_envelope};

use super::*;

/// A `herdr_launch` carrying `recoveryOf`.
fn recovery_args(predecessor: &str, key: &str) -> Value {
    launch_args(&task(&[("recoveryOf", json!(predecessor))]), key)
}

/// The policy `settle` reads (expiry/cooldown windows only).
fn seed_policy() -> Policy {
    Policy {
        tiers: vec![Tier("standard".into())],
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.6,
        exploration_rate: 0.0,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

/// The `unresolved(launch_failed)` settlement most predecessors end on.
fn launch_failed() -> Settlement {
    Settlement::Unresolved {
        reason: UnresolvedReason::LaunchFailed,
    }
}

/// A `done` predecessor Launch (`failed`) — the CHECK wants an outcome.
fn done_launch(launch: &str, run: &str) -> Launch {
    let mut row = launch_row(launch, LaunchPhase::Done);
    row.outcome = Some(LaunchOutcome::Failed {
        certainty: EffectCertainty::Unknown,
        run: Some(RunId(run.into())),
        created_topology: CreatedTopology {
            tab: None,
            panes: Vec::new(),
        },
    });
    row
}

/// Seed `owner`'s predecessor `run` (launch `launch`), `starting` with no
/// identity (the start never captured one — provably absent), started at
/// `tier` on `provider`; settled `settlement` when one is given.
fn seed_predecessor(
    store: &mut Store,
    owner: &CallerKey,
    run: &str,
    launch: &str,
    settlement: Option<Settlement>,
) {
    let mut row = run_row(run, launch, State::Starting);
    row.owner = owner.clone();
    row.tier_start = Some(Tier("standard".into()));
    row.provider = Some(Provider("vendor-a".into()));
    let mut done = done_launch(launch, run);
    done.caller = owner.clone();
    let mut seed = Vec::new();
    if *owner != caller_key() {
        seed.push(StateChange::BindCaller(CallerBinding {
            caller: owner.clone(),
            relay_instance: RelayInstanceId("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd".into()),
            pane_at_bind: PaneId("w2:p1".into()),
        }));
    }
    seed.extend(launch_chain(&done));
    seed.push(StateChange::ReserveRun(row));
    store.apply(&changes(seed), NOW).expect("seed predecessor");
    let Some(end) = settlement else {
        return;
    };
    let seeded = store
        .run(&RunId(run.into()))
        .expect("read")
        .expect("seeded run");
    let settled = settle(&seeded, end, NOW, &seed_policy());
    store.apply(&settled, NOW).expect("settle predecessor");
}

/// The bound caller's own binding — the seeds' FK target.
fn bind_caller(store: &mut Store) {
    store
        .apply(
            &changes(vec![StateChange::BindCaller(CallerBinding {
                caller: caller_key(),
                relay_instance: RelayInstanceId(RELAY.into()),
                pane_at_bind: PaneId(CALLER_PANE.into()),
            })]),
            NOW,
        )
        .expect("bind caller");
}

/// A world with the caller's settled predecessor `run-pred` seeded (a
/// `standard` start on `vendor-a`) and the one point its recovery may
/// take: `op-hi-b`, a tier up on another provider (F13 step 4).
async fn recovery_world() -> World {
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.tiers = vec!["standard".to_owned(), "high".to_owned()];
        catalog.points_toml = point_at("op-hi-b", "high", 0, "vendor-b", "--hi-b");
        catalog.daemon_extra = "launch_wait_secs = 15\n".to_owned();
    });
    world.start().await;
    let mut store = world.store();
    bind_caller(&mut store);
    seed_predecessor(
        &mut store,
        &caller_key(),
        "run-pred",
        "l-pred",
        Some(launch_failed()),
    );
    qualify_start(&mut store, "op-hi-b", &["--hi-b"]);
    world
}

/// F21/§4.10 — the successor's routing decision and the obligation's
/// `pending → dispatched` move (linked to the successor Launch, with its
/// `recovery_dispatched` event) persist together; a second `recoveryOf`
/// under another caller key replays to the same successor.
#[tokio::test]
async fn f21_successor_decided_dispatches_obligation_in_route_transaction() {
    let world = recovery_world().await;
    world.jev().push_answers(launch_eval("new"));

    let body = tool_body(&world.launch(&recovery_args("run-pred", "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");

    let store = world.store();
    let successor = launch_at(&store, "recovery:");
    assert_eq!(successor.idempotency_key.0, "recovery:run-pred");
    assert!(
        successor
            .task
            .objective
            .contains("predecessor_run_id: run-pred"),
        "the successor preamble: {}",
        successor.task.objective
    );
    assert!(successor.decision.is_some(), "routed");
    let dispatched = store
        .recoveries_by_state(RecoveryStatus::Dispatched)
        .expect("recoveries read");
    let [obligation] = dispatched.as_slice() else {
        panic!("one dispatched obligation: {dispatched:?}");
    };
    assert_eq!(obligation.predecessor.0, "run-pred");
    assert_eq!(obligation.successor_launch.as_ref(), Some(&successor.id));
    let events = caller_events(&store, MailboxEventKind::RecoveryDispatched);
    assert_eq!(events.len(), 1, "recovery_dispatched once");
    assert!(
        events[0].body.contains(&successor.id.0),
        "{}",
        events[0].body
    );

    let replay = tool_body(&world.launch(&recovery_args("run-pred", "k2")).await);
    assert_eq!(
        replay, body,
        "the successor key is the idempotency identity"
    );
    assert_eq!(world.evals().len(), 1, "one evaluation total");
    world.shutdown().await;
}

/// F21/§4.10 — an admitted successor that abstains moves the still
/// `pending` obligation to `blocked` with the abstention reason, emits
/// `recovery_blocked`, and the caller is answered `abstained`.
#[tokio::test]
async fn f21_successor_abstention_blocks_pending_obligation_with_event() {
    let world = recovery_world().await;
    world.jev().push_fault(Fault::Status {
        status: 503,
        error_type: None,
        retry_after_ms: None,
    });

    let body = tool_body(&world.launch(&recovery_args("run-pred", "k1")).await);
    assert_eq!(
        body,
        json!({"outcome": "abstained", "reason": "evaluation_failed"})
    );
    let store = world.store();
    let blocked = store
        .recoveries_by_state(RecoveryStatus::Blocked)
        .expect("recoveries read");
    let [obligation] = blocked.as_slice() else {
        panic!("one blocked obligation: {blocked:?}");
    };
    assert_eq!(obligation.predecessor.0, "run-pred");
    assert_eq!(obligation.reason.as_deref(), Some("evaluation_failed"));
    assert!(
        store
            .recoveries_by_state(RecoveryStatus::Pending)
            .expect("read")
            .is_empty(),
        "nothing left pending"
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::RecoveryBlocked).len(),
        1
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchAnswered).len(),
        1
    );
    world.shutdown().await;
}

/// F21 — `recoveryOf` admission refuses before recording anything: an
/// unknown or still-running predecessor is
/// `RECOVERY_PREDECESSOR_UNSETTLED`; another caller's is `NOT_OWNER`.
#[tokio::test]
async fn f21_recovery_of_refusals() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    let foreign = CallerKey {
        agent_kind: AgentKind("kind-b".into()),
        native_session: NativeSession("sess-other".into()),
    };
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(&mut store, &caller_key(), "run-live", "l-live", None);
        seed_predecessor(
            &mut store,
            &foreign,
            "run-foreign",
            "l-foreign",
            Some(launch_failed()),
        );
    }

    let unknown = world.launch(&recovery_args("run-missing", "k1")).await;
    assert_eq!(tool_code(&unknown), "RECOVERY_PREDECESSOR_UNSETTLED");
    let live = world.launch(&recovery_args("run-live", "k2")).await;
    assert_eq!(tool_code(&live), "RECOVERY_PREDECESSOR_UNSETTLED");
    let not_mine = world.launch(&recovery_args("run-foreign", "k3")).await;
    assert_eq!(tool_code(&not_mine), "NOT_OWNER");

    let store = world.store();
    assert_eq!(
        all_launches(&store).len(),
        2,
        "only the two seeded launches"
    );
    assert!(
        store
            .recoveries_by_state(RecoveryStatus::Pending)
            .expect("read")
            .is_empty(),
        "no obligation recorded"
    );
    assert!(world.jev().requests().is_empty(), "refused before Jev");
    world.shutdown().await;
}

/// F13 step 4 — a recovery starts at least one tier above the
/// predecessor's start and never on its provider: `standard` judged,
/// `high` floored, `vendor-a`'s points excluded at both tiers.
#[tokio::test]
async fn f13_recovery_minimum_and_exclusion() {
    let points = format!(
        "{}{}{}",
        point("op-std-a", 0, "vendor-a", "--std-a"),
        point_at("op-hi-a", "high", 0, "vendor-a", "--hi-a"),
        point_at("op-hi-b", "high", 1, "vendor-b", "--hi-b")
    );
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.tiers = vec!["standard".to_owned(), "high".to_owned()];
        catalog.points_toml = points;
        catalog.daemon_extra = "launch_wait_secs = 15\n".to_owned();
    });
    world.jev().push_answers(with_answer(
        launch_eval("new"),
        "weakest_sufficient_tier",
        &Answer::choice("standard", &[("standard", 0.8), ("high", 0.2)]),
    ));
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(
            &mut store,
            &caller_key(),
            "run-pred",
            "l-pred",
            Some(launch_failed()),
        );
        for (point_id, args) in [
            ("op-std-a", "--std-a"),
            ("op-hi-a", "--hi-a"),
            ("op-hi-b", "--hi-b"),
        ] {
            qualify_start(&mut store, point_id, &[args]);
        }
    }

    let body = tool_body(&world.launch(&recovery_args("run-pred", "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");
    assert_eq!(body["operatingPointId"], "op-hi-b");
    assert_eq!(body["tierEvidence"]["judgedTier"], "standard");
    assert_eq!(body["tierEvidence"]["startTier"], "high");
    let successor = launch_at(&world.store(), "recovery:");
    let decision = successor.decision.expect("decision");
    assert_eq!(decision.recovery_minimum, Some(Tier("high".into())));
    let candidates: Vec<OperatingPointId> = decision
        .candidates
        .into_iter()
        .map(|candidate| candidate.operating_point)
        .collect();
    assert_eq!(candidates, [OperatingPointId("op-hi-b".into())]);
    world.shutdown().await;
}

/// F21 — the claimable window across idempotency scopes: a `provider_limit`
/// obligation stays `pending` — claimable — while the successor that
/// claimed it is still `evaluating`, and F11's `(caller, project_root,
/// key)` scope cannot see a `recovery:<predecessor>` Launch admitted
/// under another root. A second `recoveryOf` from the same owner under a
/// different root is `RECOVERY_EXISTS`: the successor Launch, not the
/// scope, is the recovery's identity.
#[tokio::test]
async fn f21_second_recovery_in_another_scope_is_recovery_exists() {
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.tiers = vec!["standard".to_owned(), "high".to_owned()];
        catalog.points_toml = point_at("op-hi-b", "high", 0, "vendor-b", "--hi-b");
        catalog.daemon_extra = "launch_wait_secs = 1\n".to_owned();
    });
    // The successor's evaluation never answers — the Launch holds
    // `evaluating` and the claimed obligation `pending` for the window.
    world.jev().push_fault(Fault::Silent);
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(
            &mut store,
            &caller_key(),
            "run-pred",
            "l-pred",
            Some(Settlement::ProviderLimited),
        );
        qualify_start(&mut store, "op-hi-b", &["--hi-b"]);
    }

    let first = world.spawn_launch(&recovery_args("run-pred", "k1"));
    wait_store(&world.state(), "the successor launch", |store| {
        all_launches(store)
            .iter()
            .any(|launch| launch.idempotency_key.0 == "recovery:run-pred")
            .then_some(())
    })
    .await;

    // The same owner under a different canonical `projectRoot` — a
    // second idempotency scope the `recovery:` key never sees.
    let other = world.outside();
    let second_scope = McpClient::new(
        &world.dirs().socket_path(),
        caller_envelope(CALLER_PANE, other.to_str().expect("utf8"), RELAY),
    );
    let reply = second_scope
        .call_tool(json!(1), "herdr_launch", recovery_args("run-pred", "k2"))
        .await;
    assert_eq!(tool_code(&reply), "RECOVERY_EXISTS", "{reply}");

    let store = world.store();
    let successors: Vec<Launch> = all_launches(&store)
        .into_iter()
        .filter(|launch| launch.idempotency_key.0.starts_with("recovery:"))
        .collect();
    assert_eq!(successors.len(), 1, "one successor: {successors:?}");
    assert_eq!(
        store
            .recoveries_by_state(RecoveryStatus::Pending)
            .expect("recoveries read")
            .len(),
        1,
        "the claimed obligation was still pending — the window the refusal closes"
    );

    let first_body = tool_body(&first.await.expect("first call joins"));
    assert_eq!(first_body["outcome"], "pending", "{first_body}");
    world.shutdown().await;
}
