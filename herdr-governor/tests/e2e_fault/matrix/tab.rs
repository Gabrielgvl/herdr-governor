// — `tab`/`split` (the topology leg) —————————————————————————————————

use super::*;

/// S1 — `tab@pre_dispatch`: the never-committed `tab.create` stays
/// `planned`; the restart creates the tab once and the launch lands.
#[tokio::test]
async fn s1_tab_pre_dispatch_relaunches_once() {
    let mut cell = kill_cell("tab", Boundary::PreDispatch, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "tab");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Planned
    );
    assert!(wire_calls(cell.world.fake(), "tab.create").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(wire_calls(cell.world.fake(), "tab.create").len(), 1);
    assert_eq!(wire_calls(cell.world.fake(), "agent.start").len(), 1);
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(task_prompts(cell.world.fake()), 1);
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `tab@dispatch_committed`: a `dispatching` `tab.create` the wire
/// never saw is `unconfirmed` at restart — the launch fails `unknown`
/// with no createdTopology, the run settles `launch_failed`, and
/// `tab.create`/`agent.start`/`agent.prompt` never reach the wire.
#[tokio::test]
async fn s1_tab_dispatch_committed_fails_launch_unknown() {
    let mut cell = kill_cell("tab", Boundary::DispatchCommitted, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    assert_eq!(run.state, State::Starting);
    let key = run_key(run, "tab");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert!(wire_calls(cell.world.fake(), "tab.create").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    assert_launch_failed(&settled_run(&cell.world, run).await);
    assert!(
        wire_calls(cell.world.fake(), "tab.create").is_empty()
            && wire_calls(cell.world.fake(), "agent.start").is_empty()
            && task_prompts(cell.world.fake()) == 0,
        "the committed-but-never-wired leg never runs"
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `tab@wire_returned`: `tab.create` answered but the result died —
/// `unconfirmed` at restart, `unknown` certainty, and the created tab is
/// honestly absent from `createdTopology` (no receipt committed). The
/// wire ran once — never again.
#[tokio::test]
async fn s1_tab_wire_returned_fails_launch_unknown() {
    let mut cell = kill_cell("tab", Boundary::WireReturned, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "tab");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert_eq!(wire_calls(cell.world.fake(), "tab.create").len(), 1);

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    assert_launch_failed(&settled_run(&cell.world, run).await);
    assert_eq!(
        wire_calls(cell.world.fake(), "tab.create").len(),
        1,
        "the wire ran once — never again"
    );
    assert!(wire_calls(cell.world.fake(), "agent.start").is_empty());
    assert_eq!(task_prompts(cell.world.fake()), 0);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `tab@result_committed`: the tab's `TabCreated` receipt journaled
/// and `start:0` planned before the abort — the restart continues to
/// `launched`/`active` with every wire op exactly once.
#[tokio::test]
async fn s1_tab_result_committed_continues() {
    let mut cell = kill_cell("tab", Boundary::ResultCommitted, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "tab");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Acknowledged,
        "the receipt committed pre-kill"
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(wire_calls(cell.world.fake(), "tab.create").len(), 1);
    assert_eq!(wire_calls(cell.world.fake(), "agent.start").len(), 1);
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(task_prompts(cell.world.fake()), 1);
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}
