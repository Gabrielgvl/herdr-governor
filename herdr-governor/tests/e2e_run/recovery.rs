//! `recovery` — §4.10's daemon-side sweep end to end (S21, S21b, S22):
//! a `pending` obligation admits the deterministic `recovery:<pred>`
//! successor whose Route moves it `dispatched`, whose abstention moves
//! it `blocked` (each with its mailbox event), and whose expiry fails
//! it — unless a successor is already in flight. §17's settled-mid-
//! start carry-over: the `agent.start` ack that lands after the Run
//! settled carries an orphan child identity, and the daemon plans and
//! dispatches the verified `<start>:close` for it.

use std::time::Duration;

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::RunId;
use governor_core::lifecycle::{Settlement, State, StateChange, UnresolvedReason, settle};
use governor_core::recovery::{RecoveryObligation, RecoveryStatus};
use governor_core::task::{LaunchOutcome, LaunchPhase};
use serde_json::json;

use crate::support::daemon::{await_for, never};
use crate::support::fake_herdr::Fault;
use crate::support::fake_jev::Fault as JevFault;

use super::*;

/// The caller's settle-time `Policy` — only the window fields matter.
fn seed_policy() -> governor_core::config::Policy {
    governor_core::config::Policy {
        tiers: vec![governor_core::config::Tier("standard".into())],
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

/// Seed the caller's predecessor `run` — `starting` with no captured
/// identity (a child provably absent — §4.10's dispatch gate), started
/// `standard` on `vendor-a`, settled `unresolved(launch_failed)`.
fn seed_predecessor(store: &mut herdr_governor::store::Store, run: &str, launch: &str) {
    let mut row = run_row(run, launch, State::Starting);
    row.tier_start = Some(governor_core::config::Tier("standard".into()));
    row.provider = Some(governor_core::config::Provider("vendor-a".into()));
    let mut writes = launch_chain(&done_launch(launch, failed_outcome(run)));
    writes.push(StateChange::ReserveRun(row));
    seed(store, writes, Vec::new());
    let seeded = store
        .run(&RunId(run.into()))
        .expect("read")
        .expect("seeded run");
    let settled = settle(
        &seeded,
        Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed,
        },
        NOW,
        &seed_policy(),
    );
    store.apply(&settled, NOW).expect("settle predecessor");
}

/// The `pending` `provider_limit` obligation for `pred` — `expiry`
/// sizes `expires_at` against the daemon's real clock.
fn obligation(store: &mut herdr_governor::store::Store, pred: &str, expiry: Duration) {
    seed_obligation(store, &RunId(pred.into()), expiry);
}

/// The one obligation row in `status`, or `None`.
fn obligation_in(
    store: &herdr_governor::store::Store,
    status: RecoveryStatus,
) -> Option<RecoveryObligation> {
    let mut rows = store.recoveries_by_state(status).expect("recoveries read");
    (rows.len() == 1).then(|| rows.remove(0))
}

/// A world catalogued for recovery: `standard` and `high` tiers, and
/// the one `high`-tier non-`vendor-a` point the F13 recovery minimum
/// admits.
fn recovery_world() -> World {
    World::new(|catalog| {
        catalog.tiers = vec!["standard".to_owned(), "high".to_owned()];
        catalog.points_toml = point_at("op-hi-b", "high", 0, "vendor-b", "--hi-b");
    })
}

/// F21/§4.10 (S21) — the sweep admits the deterministic successor for
/// a `dispatch_ready` obligation while it stays `pending`; the
/// successor's routing decision moves it `dispatched` in the same
/// transaction, with its `recovery_dispatched` event, and the
/// successor Run launches.
#[tokio::test]
async fn s21_pending_obligation_dispatches_through_successor_route() {
    let mut world = recovery_world();
    world.jev().push_answers(launch_eval("new"));
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(&mut store, "r-pred", "l-pred");
        // Far past the (real-clock − seed-NOW) skew — live for the run.
        obligation(&mut store, "r-pred", Duration::from_hours(24 * 365));
        qualify_start(&mut store, "op-hi-b", &["--hi-b"]);
    }
    world.start().await;

    let dispatched = wait_store(&world.state(), "the dispatched obligation", |store| {
        obligation_in(store, RecoveryStatus::Dispatched)
    })
    .await;
    assert_eq!(dispatched.predecessor.0, "r-pred");
    let successor_id = dispatched.successor_launch.clone().expect("linked");
    let successor = wait_store(
        &world.state(),
        "the successor's launched outcome",
        |store| {
            store
                .launch(&successor_id)
                .ok()
                .flatten()
                .filter(|launch| launch.outcome.is_some())
        },
    )
    .await;
    assert_eq!(successor.idempotency_key.0, "recovery:r-pred");
    assert!(
        successor
            .task
            .objective
            .contains("predecessor_run_id: r-pred"),
        "the successor preamble: {}",
        successor.task.objective
    );
    assert!(
        matches!(successor.outcome, Some(LaunchOutcome::Launched { .. })),
        "the successor launched: {:?}",
        successor.outcome
    );
    let store = world.store();
    assert_eq!(
        caller_events(&store, MailboxEventKind::RecoveryDispatched).len(),
        1,
        "recovery_dispatched once"
    );
    let run = run_for(&store, &successor);
    assert_eq!(run.owner, caller_key(), "the predecessor's owner owns it");
    assert_eq!(world.evals().len(), 1, "the successor asked exactly once");
    world.shutdown().await;
}

/// F21/§4.10 (S21b) — the admitted successor that abstains moves the
/// still-`pending` obligation `blocked` with the abstention reason and
/// its `recovery_blocked` event.
#[tokio::test]
async fn s21b_successor_abstention_blocks_the_pending_obligation() {
    let mut world = recovery_world();
    world.jev().push_fault(JevFault::Status {
        status: 503,
        error_type: None,
        retry_after_ms: None,
    });
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(&mut store, "r-pred", "l-pred");
        obligation(&mut store, "r-pred", Duration::from_hours(24 * 365));
    }
    world.start().await;

    let blocked = wait_store(&world.state(), "the blocked obligation", |store| {
        obligation_in(store, RecoveryStatus::Blocked)
    })
    .await;
    assert_eq!(blocked.predecessor.0, "r-pred");
    assert_eq!(blocked.reason.as_deref(), Some("evaluation_failed"));
    let store = world.store();
    assert!(
        obligation_in(&store, RecoveryStatus::Pending).is_none(),
        "nothing left pending"
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::RecoveryBlocked).len(),
        1,
        "recovery_blocked once"
    );
    world.shutdown().await;
}

/// F21/§4.10 (S22) — a `pending` obligation past `expires_at` fails
/// `expired` on the next sweep; no successor is admitted and Jev is
/// never asked.
#[tokio::test]
async fn s22_elapsed_obligation_fails_expired_without_a_successor() {
    let mut world = recovery_world();
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(&mut store, "r-pred", "l-pred");
        // `expires_at = NOW` — elapsed under the daemon's real clock.
        obligation(&mut store, "r-pred", Duration::ZERO);
    }
    world.start().await;

    let failed = wait_store(&world.state(), "the failed obligation", |store| {
        obligation_in(store, RecoveryStatus::Failed)
    })
    .await;
    assert_eq!(failed.predecessor.0, "r-pred");
    assert_eq!(failed.reason.as_deref(), Some("expired"));
    let store = world.store();
    assert!(
        all_launches(&store)
            .iter()
            .all(|launch| !launch.idempotency_key.0.starts_with("recovery:")),
        "no successor was admitted"
    );
    assert!(world.evals().is_empty(), "Jev was never asked");
    world.shutdown().await;
}

/// F21/§4.10 — the expiry sweep skips an obligation whose deterministic
/// successor is already in flight: `expires_at` elapsed yet the row
/// stays `pending` (never `failed`), no second successor is minted, and
/// the in-flight one's own bounds still apply.
#[tokio::test]
async fn f21_expiry_sweep_skips_obligation_with_in_flight_successor() {
    let mut world = recovery_world();
    // The successor's evaluation never answers — it holds `evaluating`
    // for the whole assertion window.
    world.jev().push_fault(JevFault::Silent);
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_predecessor(&mut store, "r-pred", "l-pred");
        // Already elapsed — without the successor this fails on the
        // first sweep.
        obligation(&mut store, "r-pred", Duration::ZERO);
        // The in-flight successor the sweep must see: `evaluating`,
        // keyed `recovery:r-pred`, `recovery_of` bound.
        let mut successor = launch_row("l-succ", LaunchPhase::Evaluating);
        successor.idempotency_key =
            governor_core::identity::IdempotencyKey("recovery:r-pred".into());
        successor.task.recovery_of = Some(RunId("r-pred".into()));
        seed(
            &mut store,
            vec![StateChange::RecordLaunch(successor)],
            Vec::new(),
        );
    }
    world.start().await;

    // Three-plus ticks at the one-second cadence: the obligation stays
    // `pending` — the elapsed expiry is skipped, never `failed`.
    never(
        "the obligation to leave pending while its successor is in flight",
        Duration::from_secs(3),
        || {
            let store = world.store();
            store
                .recoveries_by_state(RecoveryStatus::Pending)
                .expect("recoveries read")
                .is_empty()
        },
    )
    .await;
    let store = world.store();
    assert!(
        obligation_in(&store, RecoveryStatus::Failed).is_none(),
        "the elapsed obligation was skipped, not failed"
    );
    let successors: Vec<_> = all_launches(&store)
        .into_iter()
        .filter(|launch| launch.idempotency_key.0.starts_with("recovery:"))
        .collect();
    assert_eq!(successors.len(), 1, "no second successor minted");
    world.shutdown().await;
}

/// §17 — a Run settled while its `agent.start` is in flight: the ack
/// carries a live child identity nothing else owns (the Run's row never
/// captured it), so the daemon plans the verified `<start>:close` for
/// the orphan and `pane.close` reaches the wire. The cancel rides the
/// literal `herdr_run{cancel}` — F20 and the orphan carry-over compose.
#[tokio::test]
async fn s17_settled_mid_start_orphan_gets_a_verified_close() {
    let mut world = World::new(|catalog| {
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
        catalog.daemon_extra = "launch_wait_secs = 5\n".to_owned();
    });
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    // Hold the start ack — the cancel must settle the Run first.
    world
        .fake()
        .fault("agent.start", Fault::Delay(Duration::from_millis(2_500)));
    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    await_for("the start on the wire", || {
        saw_wire(world.fake(), "agent.start")
    })
    .await;

    let run = run_for(&world.store(), &only_launch(&world.store()));
    let cancel = world
        .run_call(&json!({"action": "cancel", "runId": run.id.0, "closePane": false}))
        .await;
    assert_eq!(
        tool_body(&cancel)["settlement"],
        "cancelled",
        "settled mid-start: {cancel}"
    );

    // The delayed ack lands: the AgentStarted receipt's orphan identity
    // is closed — one `pane.close`, acknowledged, journaled
    // `run:<id>:start:0:close`.
    let close = wait_store(&world.state(), "the orphan close to commit", |store| {
        store
            .effect(&governor_core::identity::EffectKey(run_key(
                &run,
                "start:0:close",
            )))
            .ok()
            .flatten()
            .filter(|row| row.state == governor_core::lifecycle::EffectState::Acknowledged)
    })
    .await;
    assert_eq!(close.kind, governor_core::lifecycle::EffectKind::Close);
    assert!(
        saw_wire(world.fake(), "pane.close"),
        "the verified close reached the wire"
    );
    assert_eq!(
        wire_calls(world.fake(), "pane.close").len(),
        1,
        "exactly one close"
    );
    // The cancelled-mid-start Launch converges `failed` once the
    // delayed `agent.start` leg goes terminal — the acknowledged start
    // keeps the certainty `unknown` (a child provably ran — the orphan
    // the close above is for).
    assert_eq!(
        tool_body(&call.await.expect("call joins"))["outcome"],
        "failed"
    );
    world.shutdown().await;
}
