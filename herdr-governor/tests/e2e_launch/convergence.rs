//! `convergence` — the §4.5 table's rows on the wire: an `evaluating`
//! Launch whose ask is still `planned` dispatches on the next boot; a
//! `routed` Launch with a `reserved` Run and no topology leg begins, or
//! — its caller absent — finishes `failed{absent}` settling the Run
//! `launch_not_started`; a topology failure with no start leg finishes
//! `failed` and the Run settles `launch_failed`; and a Run settled
//! before `prompting` finishes its Launch exactly once, after its
//! in-flight leg resolves. (The `dispatching`-eval row is
//! `evaluation::f12_dispatching_eval_abstains_interrupted_on_restart`.)

use std::time::Duration;

use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::delivery::MailboxEventKind;
use governor_core::identity::{AgentKind, CallerBinding, EffectKey, PaneId, RelayInstanceId};
use governor_core::lifecycle::{
    EffectCertainty, EffectState, Settlement, State, StateChange, UnresolvedReason, settle,
};
use governor_core::routing::{Candidate, Decision, Exploration};
use governor_core::task::{LaunchOutcome, LaunchPhase, admit};
use herdr_governor::store::Store;
use serde_json::json;

use crate::support::fake_herdr::Fault;
use crate::support::fake_herdr::topology::Topology;

use super::*;

/// The `Decision` a seeded `routed` row carries — one `start`-qualified
/// candidate at `op-a`.
fn seeded_decision() -> Decision {
    Decision {
        judged_tier: Tier("standard".into()),
        requested_tier: None,
        policy_cap: None,
        policy_floor: None,
        caller_uplift: None,
        recovery_minimum: None,
        exploration: Exploration {
            assigned: false,
            executed: false,
        },
        start_tier: Tier("standard".into()),
        candidates: vec![Candidate {
            operating_point: OperatingPointId("op-a".into()),
            provider: Provider("vendor-a".into()),
            tier: Tier("standard".into()),
            harness: AgentKind("kind-a".into()),
            args: vec!["--a".to_owned()],
        }],
        config_version: ConfigVersion("seeded".into()),
    }
}

fn caller_binding() -> StateChange {
    StateChange::BindCaller(CallerBinding {
        caller: caller_key(),
        relay_instance: RelayInstanceId(RELAY.into()),
        pane_at_bind: PaneId(CALLER_PANE.into()),
    })
}

/// Seed the `routed` Launch `l-routed` with its `reserved` Run and no
/// topology leg — what a kill between `decided` and `begin` leaves.
fn seed_routed(store: &mut Store) {
    let mut launch = launch_row("l-routed", LaunchPhase::Routed);
    launch.decision = Some(seeded_decision());
    launch.config_version = Some(ConfigVersion("seeded".into()));
    let mut seed = vec![caller_binding()];
    seed.extend(launch_chain(&launch));
    seed.push(StateChange::ReserveRun(run_row(
        "run-routed",
        "l-routed",
        State::Reserved,
    )));
    store.apply(&changes(seed), NOW).expect("seed routed");
}

/// §4.5 — an `evaluating` Launch with its ask still `planned` (a kill
/// between admission and dispatch) loses nothing: the next boot hands
/// the journaled ask out once and the Launch completes.
#[tokio::test]
async fn convergence_planned_eval_dispatches_on_the_next_boot() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    {
        let mut store = world.store();
        store
            .apply(&changes(vec![caller_binding()]), NOW)
            .expect("bind caller");
        store
            .apply(
                &admit(&launch_row("l-seeded", LaunchPhase::Evaluating)),
                NOW,
            )
            .expect("seed admit");
        qualify_start(&mut store, "op-a", &["--a"]);
    }
    world.start().await;

    let launch = launch_done(&world.state()).await;
    assert_eq!(launch.id.0, "l-seeded");
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(world.jev().requests().len(), 1, "the journaled ask, once");
    world.shutdown().await;
}

/// §4.5 — a `routed` Launch with a `reserved` Run and no topology leg
/// resolves the caller's pane by native session in the fresh snapshot
/// and runs `begin ‖ launch_plan` at startup.
#[tokio::test]
async fn convergence_routed_reserved_row_begins_topology() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    {
        let mut store = world.store();
        seed_routed(&mut store);
        qualify_start(&mut store, "op-a", &["--a"]);
    }
    world.start().await;

    let launch = launch_done(&world.state()).await;
    let Some(LaunchOutcome::Launched { run, .. }) = &launch.outcome else {
        panic!("the routed row converged to launched: {:?}", launch.outcome);
    };
    assert_eq!(run.0, "run-routed");
    assert!(
        saw_wire(world.fake(), "tab.create"),
        "begin planned the tab"
    );
    assert!(world.jev().requests().is_empty(), "never re-evaluated");
    world.shutdown().await;
}

/// §4.5 — the same `routed` row with a caller the fresh snapshot cannot
/// place finishes `failed{absent}` and settles the reserved Run
/// `launch_not_started` in the same write; no topology is touched.
#[tokio::test]
async fn convergence_routed_absent_caller_finishes_failed_absent() {
    let mut world = World::build(Topology::single_shell(), |catalog| {
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
    });
    seed_routed(&mut world.store());
    world.start().await;

    let launch = launch_done(&world.state()).await;
    let Some(LaunchOutcome::Failed { certainty, run, .. }) = &launch.outcome else {
        panic!("the absent caller finished failed: {:?}", launch.outcome);
    };
    assert_eq!(*certainty, EffectCertainty::Absent);
    assert_eq!(run.as_ref().map(|id| id.0.as_str()), Some("run-routed"));
    let row = run_for(&world.store(), &launch);
    assert_eq!(
        row.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchNotStarted,
        })
    );
    assert!(
        !saw_wire(world.fake(), "tab.create") && !saw_wire(world.fake(), "agent.start"),
        "an absent caller touches no topology"
    );
    world.shutdown().await;
}

/// §4.5/S14b — the topology leg fails and no start leg exists: the
/// Launch finishes `failed{unknown}` with an empty `createdTopology`
/// (one `launch_failed`, one `launch_answered`), and the Run — its
/// launch leg attempted and terminated — settles
/// `unresolved(launch_failed)` through the identity-less absence rule.
#[tokio::test]
async fn f7_topology_failure_finishes_launch_failed_and_run_unresolved() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    world
        .fake()
        .fault("tab.create", Fault::TimeoutAfter(Duration::from_millis(50)));

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "failed", "{body}");
    assert_eq!(body["effectCertainty"], "unknown");
    assert_eq!(body["createdTopology"], json!({"tab": null, "panes": []}));

    let run = wait_store(&world.state(), "the run to settle", |store| {
        let run = run_for(store, &only_launch(store));
        (run.state == State::Settled).then_some(run)
    })
    .await;
    assert_eq!(
        run.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed,
        })
    );
    let store = world.store();
    assert!(
        store
            .effect(&EffectKey(run_key(&run, "start:0")))
            .expect("read")
            .is_none(),
        "no start leg was ever planned"
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchFailed).len(),
        1
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchAnswered).len(),
        1
    );
    assert!(!saw_wire(world.fake(), "agent.start"));
    world.shutdown().await;
}

/// §4.5 — the Run settles (`cancelled`) while its tab leg is still in
/// flight: the Launch waits for that leg's result, then finishes
/// `failed` exactly once with the tab it really created; the settled
/// Run never starts an agent.
#[tokio::test]
async fn f7_run_settled_before_prompting_finishes_launch_once() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    world
        .fake()
        .fault("tab.create", Fault::Delay(Duration::from_millis(2500)));

    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    let run = wait_store(&world.state(), "the tab leg in flight", |store| {
        let launch = store
            .launches_in_phase(LaunchPhase::Launching)
            .expect("read")
            .into_iter()
            .next()?;
        let run = store.run_by_launch(&launch.id).ok().flatten()?;
        let tab = store
            .effect(&EffectKey(run_key(&run, "tab")))
            .ok()
            .flatten()?;
        (tab.state == EffectState::Dispatching).then_some(run)
    })
    .await;
    let policy = governor_core::config::Policy {
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
    };
    world
        .store()
        .apply(&settle(&run, Settlement::Cancelled, NOW, &policy), NOW)
        .expect("settle the starting run");

    let body = tool_body(&call.await.expect("call joins"));
    assert_eq!(body["outcome"], "failed", "{body}");
    assert_eq!(body["effectCertainty"], "absent", "{body}");
    assert!(
        body["createdTopology"]["tab"].is_string(),
        "the tab the in-flight leg created is reported: {body}"
    );
    let store = world.store();
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchAnswered).len(),
        1
    );
    assert_eq!(
        effect_at(&store, &run_key(&run, "tab")).state,
        EffectState::Acknowledged
    );
    assert!(
        !saw_wire(world.fake(), "agent.start"),
        "a settled Run never starts"
    );
    world.shutdown().await;
}
