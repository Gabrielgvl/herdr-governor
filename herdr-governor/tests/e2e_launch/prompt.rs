//! `prompt` — F16 on the wire: an acknowledgement naming a different
//! agent than the captured identity is `unknown`, a lost ack is
//! `unconfirmed`, and a kill after the prompt's dispatch commit leaves it
//! `unconfirmed` on restart — each time the Run goes `active` with
//! `prompt_certainty = unconfirmed`, the caller is notified, and the
//! prompt is never resubmitted.

use std::time::Duration;

use governor_core::config::{Policy, Tier};
use governor_core::delivery::MailboxEventKind;
use governor_core::identity::EffectKey;
use governor_core::lifecycle::{
    EffectCertainty, EffectState, PromptCertainty, Run, Settlement, State, settle,
};
use governor_core::task::LaunchOutcome;
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};

use crate::support::daemon::{await_for, never};
use crate::support::fake_herdr::Fault;

use super::*;

/// The Run once `prompt:task` reached `state` — polled by key.
async fn run_when_prompt(world: &World, state: EffectState) -> Run {
    wait_store(&world.state(), "the task prompt's state", |store| {
        let launch = all_launches(store).into_iter().next()?;
        let run = store.run_by_launch(&launch.id).ok().flatten()?;
        let prompt = store
            .effect(&EffectKey(run_key(&run, "prompt:task")))
            .ok()
            .flatten()?;
        (prompt.state == state).then_some(run)
    })
    .await
}

/// F16 — the Run's unconfirmed task prompt: `active`, certainty
/// `unconfirmed`, one `prompt_unconfirmed` event for the caller.
fn assert_unconfirmed(world: &World) {
    let store = world.store();
    let run = run_for(&store, &only_launch(&store));
    assert_eq!(run.prompt_certainty, Some(PromptCertainty::Unconfirmed));
    assert_eq!(
        caller_events(&store, MailboxEventKind::PromptUnconfirmed).len(),
        1,
        "the caller is notified once"
    );
}

/// F16/H#30 — the occupant is replaced between the prompt's dispatch
/// commit and its wire write: the ack names a fresh native session, not
/// the captured identity, so the write resolves `failed/unknown`
/// (`ack_identity_mismatch`) and the prompt is unconfirmed.
#[tokio::test]
async fn f16_ack_must_match_identity() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world
        .start_seamed(SeamConfig {
            suffix: "prompt:task".to_owned(),
            boundary: Boundary::DispatchCommitted,
            action: SeamAction::Pause(Duration::from_millis(600)),
        })
        .await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    let run = run_when_prompt(&world, EffectState::Dispatching).await;
    let pane = run
        .identity
        .as_ref()
        .expect("captured identity")
        .pane_id
        .0
        .clone();
    world.fake().replace_occupant(&pane);
    assert_eq!(
        tool_body(&call.await.expect("call joins"))["outcome"],
        "launched",
        "the start ack already answered"
    );

    let failed = run_when_prompt(&world, EffectState::Failed).await;
    let prompt = effect_at(&world.store(), &run_key(&failed, "prompt:task"));
    assert_eq!(prompt.certainty, Some(EffectCertainty::Unknown));
    assert_eq!(wire_calls(world.fake(), "agent.prompt").len(), 1);
    assert_unconfirmed(&world);
    world.shutdown().await;
}

/// F16/H#29 — the prompt lands but its ack is lost (the op deadline
/// fires): `unknown`, the Run goes `active` unconfirmed, the caller is
/// notified, and the prompt is never resubmitted.
#[tokio::test]
async fn f16_lost_ack_sets_unconfirmed_and_notifies() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\nherdr_op_timeout_secs = 1\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    world.fake().fault("agent.prompt", Fault::DropResponse);

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "launched", "{body}");
    let failed = run_when_prompt(&world, EffectState::Failed).await;
    assert_eq!(
        effect_at(&world.store(), &run_key(&failed, "prompt:task")).certainty,
        Some(EffectCertainty::Unknown)
    );
    let active = run_for(&world.store(), &only_launch(&world.store()));
    assert_eq!(active.state, State::Active, "supervision continues");
    assert_unconfirmed(&world);
    assert_eq!(
        wire_calls(world.fake(), "agent.prompt").len(),
        1,
        "the lost-ack prompt landed once and was never resent"
    );
    world.shutdown().await;
}

/// F16/F8 — a kill after the prompt's dispatch commit, before its wire
/// write: the restart marks it `unconfirmed` (possibly consumed), the
/// Run goes `active` unconfirmed with the caller notified, and nothing
/// ever reaches the wire — no resubmission across the restart.
#[tokio::test]
async fn f16_prompt_kill_after_dispatch_commit_is_unconfirmed() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world
        .spawn_child(SeamConfig {
            suffix: "prompt:task".to_owned(),
            boundary: Boundary::DispatchCommitted,
            action: SeamAction::Abort,
        })
        .await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    let _call = world.fire_launch(&launch_args(&task(&[]), "k1"));
    let stderr = world.wait_child().await;
    assert!(
        stderr.contains("seam hit prompt:task@dispatch_committed"),
        "{stderr}"
    );
    let launch = only_launch(&world.store());
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "the start ack finished the launch before the kill: {:?}",
        launch.outcome
    );

    world.start().await;
    let marked = run_when_prompt(&world, EffectState::Unconfirmed).await;
    let active = wait_store(&world.state(), "the run to go active", |store| {
        store
            .run(&marked.id)
            .ok()
            .flatten()
            .filter(|row| row.state == State::Active)
    })
    .await;
    assert_eq!(active.prompt_certainty, Some(PromptCertainty::Unconfirmed));
    assert_unconfirmed(&world);
    assert!(
        !saw_wire(world.fake(), "agent.prompt"),
        "never sent, never resubmitted"
    );
    world.shutdown().await;
}

/// F20/S31b — the cancel that lands while `prompt:task`'s pre-dispatch
/// `session.snapshot` is still on the wire: `DispatchCommit` re-reads
/// the Run, sees the settlement, and skips — the row stays `planned`,
/// `dispatched_at` unset, and no `agent.prompt` is ever written. The
/// supervisor-facing cancel rides `herdr_run`, which PR B declares but
/// does not serve (a `TOOL_UNKNOWN` refusal, "lands with C2"); the
/// settlement the commit gate checks is the same one `on_cancel`
/// writes, so this test
/// applies `cancelled` through the store like
/// `f7_run_settled_before_prompting` does.
#[tokio::test]
async fn f20_cancel_during_pre_dispatch_snapshot_never_sends_the_prompt() {
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
        catalog.daemon_extra = "launch_wait_secs = 15\n".to_owned();
        // An inert tick: no reconcile snapshot can consume the armed
        // delay before the prompt's own verification does.
        catalog.reconcile_secs = 3600;
    });
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    // Hold the start ack long enough to arm the snapshot delay after the
    // request-time and tab-leg snapshots have passed — the next
    // `session.snapshot` on the wire is the task prompt's verification.
    world
        .fake()
        .fault("agent.start", Fault::Delay(Duration::from_millis(2_500)));

    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    await_for("the start on the wire", || {
        saw_wire(world.fake(), "agent.start")
    })
    .await;
    world
        .fake()
        .fault("session.snapshot", Fault::Delay(Duration::from_secs(5)));
    let snapshots = wire_calls(world.fake(), "session.snapshot").len();
    await_for("the prompt's verification in flight", || {
        wire_calls(world.fake(), "session.snapshot").len() > snapshots
    })
    .await;

    let policy = Policy {
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
    let run = run_for(&world.store(), &only_launch(&world.store()));
    world
        .store()
        .apply(&settle(&run, Settlement::Cancelled, NOW, &policy), NOW)
        .expect("cancel the prompting run");

    assert_eq!(
        tool_body(&call.await.expect("call joins"))["outcome"],
        "launched",
        "the start ack already answered"
    );
    // The delayed reply lands, the commit asks, the settled gate skips:
    // five seconds of delay plus margin, and the prompt is never sent.
    never(
        "a prompt after the skipped commit",
        Duration::from_secs(4),
        || saw_wire(world.fake(), "agent.prompt"),
    )
    .await;
    let store = world.store();
    let prompt = effect_at(&store, &run_key(&run, "prompt:task"));
    assert_eq!(prompt.state, EffectState::Planned, "the commit skipped");
    assert_eq!(prompt.dispatched_at, None, "never marked dispatching");
    let settled = run_for(&store, &only_launch(&store));
    assert_eq!(settled.state, State::Settled);
    assert_eq!(settled.settlement, Some(Settlement::Cancelled));
    world.shutdown().await;
}
