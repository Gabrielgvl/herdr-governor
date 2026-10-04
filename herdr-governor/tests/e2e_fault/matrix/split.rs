// — `split` ——————————————————————————————————————————————————————————

use super::*;

/// S1 — `split@pre_dispatch`: the never-committed `pane.split` stays
/// `planned`; the restart splits the existing tab once and launches.
#[tokio::test]
async fn s1_split_pre_dispatch_relaunches_once() {
    let mut cell = kill_cell("split", Boundary::PreDispatch, "w1:t1").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "split");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Planned
    );
    assert!(wire_calls(cell.world.fake(), "pane.split").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(wire_calls(cell.world.fake(), "pane.split").len(), 1);
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

/// S1 — `split@dispatch_committed`: a `dispatching` `pane.split` the
/// wire never saw is `unconfirmed` — launch `failed/unknown`, run
/// `launch_failed`, and `pane.split` never reaches the wire.
#[tokio::test]
async fn s1_split_dispatch_committed_fails_launch_unknown() {
    let mut cell = kill_cell("split", Boundary::DispatchCommitted, "w1:t1").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "split");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert!(wire_calls(cell.world.fake(), "pane.split").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    assert_launch_failed(&settled_run(&cell.world, run).await);
    assert!(
        wire_calls(cell.world.fake(), "pane.split").is_empty()
            && wire_calls(cell.world.fake(), "agent.start").is_empty()
            && task_prompts(cell.world.fake()) == 0,
        "the committed-but-never-wired leg never runs"
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `split@wire_returned`: `pane.split` answered but the result
/// died — `unconfirmed`, `unknown`, and the created pane is honestly
/// absent from `createdTopology`.
#[tokio::test]
async fn s1_split_wire_returned_fails_launch_unknown() {
    let mut cell = kill_cell("split", Boundary::WireReturned, "w1:t1").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "split");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert_eq!(wire_calls(cell.world.fake(), "pane.split").len(), 1);

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    assert_launch_failed(&settled_run(&cell.world, run).await);
    assert_eq!(wire_calls(cell.world.fake(), "pane.split").len(), 1);
    assert!(wire_calls(cell.world.fake(), "agent.start").is_empty());
    assert_eq!(task_prompts(cell.world.fake()), 0);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `split@result_committed`: the `PaneCreated` receipt journaled
/// and `start:0` planned before the abort — the restart continues to
/// `launched`/`active`.
#[tokio::test]
async fn s1_split_result_committed_continues() {
    let mut cell = kill_cell("split", Boundary::ResultCommitted, "w1:t1").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "split");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Acknowledged
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(wire_calls(cell.world.fake(), "pane.split").len(), 1);
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
