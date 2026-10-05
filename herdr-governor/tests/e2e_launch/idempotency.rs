//! `idempotency` — F11's `(caller, project_root, idempotency_key)`
//! contract on the wire: a mid-flight replay parks behind the same
//! Launch, a terminal replay returns the stored outcome, a digest
//! mismatch is the typed conflict, the Launch is recorded before any
//! effect (a kill before the eval's dispatch loses nothing), and a git
//! outage never masks a stored outcome.

use governor_core::delivery::MailboxEventKind;
use governor_core::lifecycle::EffectState;
use governor_core::task::{LaunchOutcome, LaunchPhase};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use serde_json::json;

use crate::support::fake_jev::Fault;

use super::*;

/// F11 — same key, same digest: a replay while the first call is still
/// `evaluating` parks behind the same Launch and both callers get the
/// one terminal body; a replay after `done` returns the stored outcome
/// verbatim — no second Launch, evaluation, Run or topology.
#[tokio::test]
async fn f11_same_key_same_digest_returns_stored_or_pending() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\njev_timeout_secs = 2\n",
    );
    world.jev().push_fault(Fault::Silent);
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let parked = launch_args(&task(&[]), "k-parked");
    let first_call = world.spawn_launch(&parked);
    wait_store(&world.state(), "the first launch to record", |store| {
        (!store
            .launches_in_phase(LaunchPhase::Evaluating)
            .expect("read")
            .is_empty())
        .then_some(())
    })
    .await;
    let second = tool_body(&world.launch(&parked).await);
    let first = tool_body(&first_call.await.expect("call joins"));
    assert_eq!(first["outcome"], "abstained", "the silent ask: {first}");
    assert_eq!(second, first, "the parked replay shares the terminal body");

    world.jev().push_answers(launch_eval("new"));
    let stored = launch_args(&task(&[]), "k-stored");
    let launched = tool_body(&world.launch(&stored).await);
    assert_eq!(launched["outcome"], "launched", "{launched}");
    let replay = tool_body(&world.launch(&stored).await);
    assert_eq!(replay, launched, "the stored outcome replays verbatim");

    assert_eq!(all_launches(&world.store()).len(), 2, "one row per key");
    assert_eq!(world.evals().len(), 2, "one evaluation per key");
    assert_eq!(wire_calls(world.fake(), "agent.start").len(), 1);
    world.shutdown().await;
}

/// F11 — a replay whose own wait elapses while the Launch is still
/// `evaluating` answers `pending` naming the *same* Launch: no second
/// row, no second ask. Once the Launch is terminal the same key returns
/// the stored outcome instead.
#[tokio::test]
async fn f11_inflight_replay_pending_names_the_same_launch() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 1\njev_timeout_secs = 6\n",
    );
    world.jev().push_fault(Fault::Silent);
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let args = launch_args(&task(&[]), "k-pending");
    let first = tool_body(&world.launch(&args).await);
    assert_eq!(first["outcome"], "pending", "the wait elapses: {first}");
    let replay = tool_body(&world.launch(&args).await);
    assert_eq!(
        replay["outcome"], "pending",
        "the in-flight replay parks, then pends: {replay}"
    );
    assert_eq!(
        replay["launchId"], first["launchId"],
        "the replay names the same in-flight Launch"
    );
    assert_eq!(all_launches(&world.store()).len(), 1, "one row only");
    assert_eq!(world.jev().requests().len(), 1, "one evaluation only");

    let done = launch_done(&world.state()).await;
    assert_eq!(done.id.0, first["launchId"].as_str().expect("launchId"));
    let stored = tool_body(&world.launch(&args).await);
    assert_eq!(
        stored,
        json!({"outcome": "abstained", "reason": "evaluation_failed"}),
        "the terminal replay returns the stored outcome: {stored}"
    );
    assert_eq!(all_launches(&world.store()).len(), 1);
    assert_eq!(world.jev().requests().len(), 1);
    assert_eq!(
        caller_events(&world.store(), MailboxEventKind::LaunchAnswered).len(),
        1,
        "the one Launch is answered exactly once"
    );
    world.shutdown().await;
}

/// F11 — the same key under a different Task digest is the typed
/// `IDEMPOTENCY_KEY_CONFLICT`, not a second launch.
#[tokio::test]
async fn f11_different_digest_conflicts() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let first = world.launch(&launch_args(&task(&[]), "k1")).await;
    assert_eq!(tool_body(&first)["outcome"], "launched");
    let other = task(&[("objective", json!("a different task entirely"))]);
    let conflicted = world.launch(&launch_args(&other, "k1")).await;
    assert_eq!(tool_code(&conflicted), "IDEMPOTENCY_KEY_CONFLICT");
    assert_eq!(all_launches(&world.store()).len(), 1);
    assert_eq!(world.evals().len(), 1);
    world.shutdown().await;
}

/// F11 — the Launch is recorded before anything else: a kill at the
/// eval's `pre_dispatch` checkpoint leaves the `evaluating` row with its
/// `planned` ask and nothing on any wire. The restart dispatches that
/// journaled ask exactly once and the Launch completes — re-taking the
/// F6 base the kill took with it (HEAD, not a silent `None`).
#[tokio::test]
async fn f11_launch_recorded_before_any_effect() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    let head = git_repo(world.project());
    world
        .spawn_child(SeamConfig {
            suffix: "evaluate".to_owned(),
            boundary: Boundary::PreDispatch,
            action: SeamAction::Abort,
        })
        .await;
    let _call = world.fire_launch(&launch_args(&task(&[]), "k1"));
    let stderr = world.wait_child().await;
    assert!(
        stderr.contains("seam hit evaluate@pre_dispatch"),
        "the kill landed at the checkpoint: {stderr}"
    );

    let store = world.store();
    let launch = only_launch(&store);
    assert_eq!(launch.phase, LaunchPhase::Evaluating, "recorded first");
    assert_eq!(
        effect_at(&store, &format!("launch:{}:evaluate", launch.id.0)).state,
        EffectState::Planned,
        "the ask is journaled, never dispatched"
    );
    assert!(world.jev().requests().is_empty(), "nothing reached Jev");
    assert!(world.fake().requests().iter().all(|(method, _)| {
        !matches!(method.as_str(), "tab.create" | "pane.split" | "agent.start")
    }));
    drop(store);

    qualify_start(&mut world.store(), "op-a", &["--a"]);
    world.start().await;
    let done = launch_done(&world.state()).await;
    assert_eq!(done.id, launch.id, "the same Launch resumed");
    assert!(
        matches!(done.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        done.outcome
    );
    let run = run_for(&world.store(), &done);
    assert_eq!(
        run.base_commit.as_deref(),
        Some(head.as_str()),
        "the restart re-took the base"
    );
    assert_eq!(world.evals().len(), 1, "evaluated exactly once");
    world.shutdown().await;
}

/// F11/F6 — the git probe gates new recordings only: a broken `HEAD`
/// after a launch finished must not mask the stored outcome.
#[tokio::test]
async fn f11_replay_during_git_outage_returns_stored_outcome() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    git_repo(world.project());
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let args = launch_args(&task(&[]), "k1");
    let first = tool_body(&world.launch(&args).await);
    assert_eq!(first["outcome"], "launched");
    std::fs::write(world.project().join(".git/config"), "not config\n").expect("corrupt config");
    let replay = tool_body(&world.launch(&args).await);
    assert_eq!(replay, first, "the stored outcome survives the outage");
    world.shutdown().await;
}
