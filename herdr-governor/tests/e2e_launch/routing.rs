//! `routing` — F13's durable ordering (the routing decision and the
//! reserved Run persist before any topology effect, proven by a kill at
//! the first topology checkpoint), a cooling provider excluded at
//! routing, and F14 placement: a related tab under four panes takes a
//! right split without focus, a full or `new` choice a fresh tab whose
//! initial pane hosts the child.

use governor_core::config::Provider;
use governor_core::lifecycle::{EffectReceipt, EffectState, State, StateChange};
use governor_core::recovery::Cooldown;
use governor_core::task::{LaunchOutcome, LaunchPhase};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use serde_json::{Value, json};

use super::*;

/// F13 — the decision is persisted before topology: a kill at the
/// `tab` effect's `pre_dispatch` checkpoint leaves the `launching` row
/// with its immutable decision and config version, the `starting` Run
/// and the `planned` tab leg — and nothing on the Herdr wire. The
/// restart dispatches that leg once and the Launch completes.
#[tokio::test]
async fn f13_decision_persisted_before_topology() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world
        .spawn_child(SeamConfig {
            suffix: "tab".to_owned(),
            boundary: Boundary::PreDispatch,
            action: SeamAction::Abort,
        })
        .await;
    let _call = world.fire_launch(&launch_args(&task(&[]), "k1"));
    let stderr = world.wait_child().await;
    assert!(stderr.contains("seam hit tab@pre_dispatch"), "{stderr}");

    let store = world.store();
    let launch = only_launch(&store);
    assert_eq!(launch.phase, LaunchPhase::Launching);
    let decision = launch.decision.as_ref().expect("the persisted decision");
    assert_eq!(decision.candidates.len(), 1);
    assert_eq!(decision.candidates[0].operating_point.0, "op-a");
    assert_eq!(decision.candidates[0].args, ["--a"]);
    assert!(launch.config_version.is_some(), "the config version rides");
    let run = run_for(&store, &launch);
    assert_eq!(run.state, State::Starting);
    assert_eq!(
        effect_at(&store, &run_key(&run, "tab")).state,
        EffectState::Planned
    );
    assert!(!saw_wire(world.fake(), "tab.create"), "no topology ran");
    drop(store);

    qualify_start(&mut world.store(), "op-a", &["--a"]);
    world.start().await;
    let done = launch_done(&world.state()).await;
    assert!(
        matches!(done.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        done.outcome
    );
    assert_eq!(wire_calls(world.fake(), "tab.create").len(), 1);
    world.shutdown().await;
}

/// F13 step 6 — a provider cooling down contributes no candidate: the
/// decision lists only the other provider's point, the launch lands
/// there with no `requestedOperatingPointId`, and the cooled point never
/// reaches the wire.
#[tokio::test]
async fn f13_cooling_provider_excluded() {
    let points = format!(
        "{}{}",
        point("op-a", 0, "vendor-a", "--a"),
        point("op-b", 1, "vendor-b", "--b")
    );
    let mut world = World::new(&points, "launch_wait_secs = 15\n");
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    {
        let mut store = world.store();
        qualify_start(&mut store, "op-a", &["--a"]);
        qualify_start(&mut store, "op-b", &["--b"]);
        store
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
    }

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");
    assert_eq!(body["operatingPointId"], "op-b");
    assert!(body.get("requestedOperatingPointId").is_none(), "{body}");
    let launch = only_launch(&world.store());
    let candidates: Vec<&str> = launch
        .decision
        .as_ref()
        .expect("decision")
        .candidates
        .iter()
        .map(|candidate| candidate.operating_point.0.as_str())
        .collect();
    assert_eq!(candidates, ["op-b"], "the cooled provider is excluded");
    let starts = wire_calls(world.fake(), "agent.start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["args"], json!(["--b"]));
    world.shutdown().await;
}

/// F14 — a related tab holding fewer than four panes takes the pane: a
/// right split anchored in that tab, without focus, and the child
/// starts in the split's pane. Three launches fill `w1:t1` to four
/// panes; the fourth, answered the same tab, opens a new one instead.
#[tokio::test]
async fn f14_existing_tab_under_four_panes_splits_right_no_focus() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("w1:t1"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    for index in 1..=3 {
        let key = format!("k{index}");
        let body = tool_body(&world.launch(&launch_args(&task(&[]), &key)).await);
        assert_eq!(body["outcome"], "launched", "{key}: {body}");
        let splits = wire_calls(world.fake(), "pane.split");
        assert_eq!(splits.len(), index, "{key} split w1:t1");
        let split = splits.last().expect("this launch's split");
        assert_eq!(split["direction"], "right", "{split}");
        assert_eq!(split["focus"], false, "{split}");
        let anchor = split["target_pane_id"].as_str().expect("anchor pane");
        assert_eq!(
            world
                .fake()
                .state()
                .topology
                .pane(anchor)
                .map(|p| p.tab_id.clone()),
            Some("w1:t1".to_owned()),
            "the split is anchored in the chosen tab"
        );
        let run = run_for(&world.store(), &launch_at(&world.store(), &key));
        let receipt = effect_at(&world.store(), &run_key(&run, "split")).receipt;
        let Some(EffectReceipt::PaneCreated { pane }) = receipt else {
            panic!("the split receipt: {receipt:?}");
        };
        let start = wire_calls(world.fake(), "agent.start");
        assert_eq!(start.last().expect("start")["pane_id"], pane.0.as_str());
    }
    assert!(
        !saw_wire(world.fake(), "tab.create"),
        "no tab under four panes"
    );

    let full = tool_body(&world.launch(&launch_args(&task(&[]), "k4")).await);
    assert_eq!(full["outcome"], "launched", "{full}");
    assert_eq!(wire_calls(world.fake(), "pane.split").len(), 3);
    assert_eq!(
        wire_calls(world.fake(), "tab.create").len(),
        1,
        "a full tab yields a new tab"
    );
    world.shutdown().await;
}

/// F14/H#102 — a `new` placement creates one tab without focus and
/// starts the child in that tab's initial pane: no split, no orphan.
#[tokio::test]
async fn f14_new_tab_initial_pane_is_used() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");
    let tabs = wire_calls(world.fake(), "tab.create");
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["focus"], false, "{}", tabs[0]);
    assert!(
        !saw_wire(world.fake(), "pane.split"),
        "the initial pane is used"
    );

    let store = world.store();
    let run = run_for(&store, &only_launch(&store));
    let receipt = effect_at(&store, &run_key(&run, "tab")).receipt;
    let Some(EffectReceipt::TabCreated { tab, pane }) = receipt else {
        panic!("the tab receipt: {receipt:?}");
    };
    let starts = wire_calls(world.fake(), "agent.start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["pane_id"], Value::from(pane.0.as_str()));
    let in_tab = world
        .fake()
        .state()
        .topology
        .panes
        .iter()
        .filter(|row| row.tab_id == tab.0)
        .count();
    assert_eq!(in_tab, 1, "one pane in the new tab — no orphan");
    world.shutdown().await;
}
