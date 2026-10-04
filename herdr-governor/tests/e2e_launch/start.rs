//! `start` — F15 on the wire: a typed pre-interactive refusal falls back
//! to the next persisted candidate in the same pane and reports the
//! requested point; an ambiguous (timed-out) start stops falling back
//! and records `failed{unknown}`; a cooldown landing between the
//! decision and the start skips that candidate inside the dispatch-time
//! recheck (S14d), before any wire write.

use std::time::Duration;

use governor_core::config::{OperatingPointId, Provider};
use governor_core::identity::EffectKey;
use governor_core::lifecycle::{
    EffectCertainty, EffectReceipt, EffectState, Settlement, State, StateChange, UnresolvedReason,
};
use governor_core::recovery::Cooldown;
use governor_core::task::{LaunchOutcome, LaunchPhase};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use serde_json::json;

use crate::support::fake_herdr::Fault;

use super::*;

/// `op-a` (cost 0, `vendor-a`) then `op-b` (cost 1, `vendor-b`), both
/// `start`-qualified once the daemon is up.
fn two_points() -> String {
    format!(
        "{}{}",
        point("op-a", 0, "vendor-a", "--a"),
        point("op-b", 1, "vendor-b", "--b")
    )
}

fn qualify_both(world: &World) {
    let mut store = world.store();
    qualify_start(&mut store, "op-a", &["--a"]);
    qualify_start(&mut store, "op-b", &["--b"]);
}

/// F15 — `agent_pane_busy` on `op-a`'s start is a typed pre-interactive
/// failure: `start:1` plans `op-b` against the same placement, the one
/// start that reaches the wire runs in the tab's initial pane (no second
/// topology), and the Run captures `op-b`.
#[tokio::test]
async fn f15_busy_falls_back_to_next_candidate_same_pane() {
    let mut world = World::new(&two_points(), "launch_wait_secs = 15\n");
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_both(&world);
    world.fake().fault("agent.start", Fault::Busy);

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");
    assert_eq!(body["operatingPointId"], "op-b");

    let store = world.store();
    let run = run_for(&store, &only_launch(&store));
    let first = effect_at(&store, &run_key(&run, "start:0"));
    let second = effect_at(&store, &run_key(&run, "start:1"));
    assert_eq!(first.state, EffectState::Failed, "the busy start failed");
    assert_eq!(second.state, EffectState::Acknowledged);
    assert_eq!(first.target, second.target, "the same placement target");
    let Some(EffectReceipt::TabCreated { pane, .. }) =
        effect_at(&store, &run_key(&run, "tab")).receipt
    else {
        panic!("the tab receipt");
    };
    let starts = wire_calls(world.fake(), "agent.start");
    assert_eq!(starts.len(), 1, "the refused start applied nothing");
    assert_eq!(starts[0]["pane_id"], pane.0.as_str(), "the same pane");
    assert_eq!(starts[0]["args"], json!(["--b"]));
    assert_eq!(wire_calls(world.fake(), "tab.create").len(), 1);
    assert!(!saw_wire(world.fake(), "pane.split"));
    assert_eq!(run.operating_point, Some(OperatingPointId("op-b".into())));
    world.shutdown().await;
}

/// F15/H#45 — a start that times out is ambiguous (`unknown`): fallback
/// stops (`start:1` is never planned), the Launch records `failed
/// {effectCertainty: unknown}` with the created tab, and the Run is left
/// to the transition rules — it settles `unresolved(launch_failed)`.
#[tokio::test]
async fn f15_runtime_timeout_stops_fallback_records_failed() {
    let mut world = World::new(&two_points(), "launch_wait_secs = 15\n");
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_both(&world);
    world.fake().fault(
        "agent.start",
        Fault::TimeoutAfter(Duration::from_millis(50)),
    );

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "failed", "{body}");
    assert_eq!(body["effectCertainty"], "unknown");
    assert!(body["createdTopology"]["tab"].is_string(), "{body}");

    let state = world.state();
    let run = wait_store(&state, "the run to settle", |store| {
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
    let start = effect_at(&store, &run_key(&run, "start:0"));
    assert_eq!(start.state, EffectState::Failed);
    assert_eq!(start.certainty, Some(EffectCertainty::Unknown));
    assert!(
        store
            .effect(&EffectKey(run_key(&run, "start:1")))
            .expect("read")
            .is_none(),
        "no fallback after an ambiguous start"
    );
    world.shutdown().await;
}

/// F5/F15 — when fallback moved the point, the outcome names both: the
/// body and the stored outcome carry `requestedOperatingPointId` = the
/// decision's first candidate.
#[tokio::test]
async fn f15_requested_point_reported_after_fallback() {
    let mut world = World::new(&two_points(), "launch_wait_secs = 15\n");
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_both(&world);
    world.fake().fault("agent.start", Fault::Busy);

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["operatingPointId"], "op-b", "{body}");
    assert_eq!(body["requestedOperatingPointId"], "op-a", "{body}");
    let launch = only_launch(&world.store());
    let Some(LaunchOutcome::Launched {
        operating_point,
        requested_operating_point,
        ..
    }) = launch.outcome
    else {
        panic!("launched: {:?}", launch.outcome);
    };
    assert_eq!(operating_point.0, "op-b");
    assert_eq!(
        requested_operating_point.map(|op| op.0),
        Some("op-a".into())
    );
    world.shutdown().await;
}

/// S14d/H#46 — a cooldown that lands after the routing decision but
/// before `op-a`'s start commits is caught by the dispatch-time recheck:
/// `start:0` journals the governor-refused pre-interactive result
/// (`dispatching → failed`, never on the wire) and the same transaction
/// plans `start:1` on `op-b`.
#[tokio::test]
async fn f15_cooldown_between_decision_and_start_skips_candidate_pre_interactively() {
    let mut world = World::new(&two_points(), "launch_wait_secs = 15\n");
    world.jev().push_answers(launch_eval("new"));
    // The pause holds the tab leg after its commit — the window between
    // the persisted decision and the first start's commit.
    world
        .start_seamed(SeamConfig {
            suffix: "tab".to_owned(),
            boundary: Boundary::DispatchCommitted,
            action: SeamAction::Pause(Duration::from_millis(400)),
        })
        .await;
    qualify_both(&world);

    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    let run = wait_store(&world.state(), "the tab dispatch commit", |store| {
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
    world
        .store()
        .apply(
            &changes(vec![StateChange::SetCooldown(Cooldown {
                provider: Provider("vendor-a".into()),
                until: FAR,
                reason: "provider_limited".into(),
                source_run: None,
            })]),
            NOW,
        )
        .expect("cooldown applies");

    let body = tool_body(&call.await.expect("call joins"));
    assert_eq!(body["outcome"], "launched", "{body}");
    assert_eq!(body["operatingPointId"], "op-b", "the cooled point skipped");
    assert_eq!(body["requestedOperatingPointId"], "op-a");
    let store = world.store();
    let first = effect_at(&store, &run_key(&run, "start:0"));
    assert_eq!(first.state, EffectState::Failed, "refused inside the gate");
    assert_eq!(first.certainty, Some(EffectCertainty::Absent));
    assert_eq!(
        effect_at(&store, &run_key(&run, "start:1")).state,
        EffectState::Acknowledged
    );
    let starts = wire_calls(world.fake(), "agent.start");
    assert_eq!(starts.len(), 1, "exactly one start reached the wire");
    assert_eq!(starts[0]["args"], json!(["--b"]), "never the cooled point");
    world.shutdown().await;
}
