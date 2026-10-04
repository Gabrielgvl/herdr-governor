// — `start:0` ————————————————————————————————————————————————————————

use super::*;

/// S1 — `start:0@pre_dispatch`: the never-committed `agent.start` stays
/// `planned`; the restart starts the child once and the launch lands.
#[tokio::test]
async fn s1_start_pre_dispatch_relaunches_once() {
    let mut cell = kill_cell("start:0", Boundary::PreDispatch, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "start:0");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Planned
    );
    assert!(wire_calls(cell.world.fake(), "agent.start").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
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

/// S1 — `start:0@dispatch_committed`: a `dispatching` `agent.start` the
/// wire never saw is `unconfirmed` — launch `failed/unknown` with the
/// acknowledged tab in `createdTopology`, run `launch_failed`, and
/// `agent.start` never reaches the wire.
#[tokio::test]
async fn s1_start_dispatch_committed_fails_launch_unknown() {
    let mut cell = kill_cell("start:0", Boundary::DispatchCommitted, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "start:0");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert!(wire_calls(cell.world.fake(), "agent.start").is_empty());

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 2);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    assert_launch_failed(&settled_run(&cell.world, run).await);
    assert_eq!(
        wire_calls(cell.world.fake(), "tab.create").len(),
        1,
        "the acknowledged topology ran once"
    );
    assert!(wire_calls(cell.world.fake(), "agent.start").is_empty());
    assert_eq!(task_prompts(cell.world.fake()), 0);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `start:0@wire_returned`: `agent.start` answered but the identity
/// capture died — the launch fails `unknown`, the run can never settle
/// `launch_failed` (the orphaned `gov-*` agent blocks `absent`), and the
/// orphan is never adopted or re-started.
#[tokio::test]
async fn s1_start_wire_returned_orphan_never_adopted() {
    let mut cell = kill_cell("start:0", Boundary::WireReturned, "new").await;
    let run = cell.run.as_ref().expect("a starting run");
    let key = run_key(run, "start:0");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert_eq!(wire_calls(cell.world.fake(), "agent.start").len(), 1);

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_failed_unknown(&launch, &run.id.0, 2);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    let current = read_run(&cell.world.store(), &run.id.0);
    assert_eq!(
        current.state,
        State::Starting,
        "the run cannot prove absence while the orphan exists"
    );
    never(
        "the orphaned run to settle",
        Duration::from_millis(2_500),
        || {
            cell.world
                .store()
                .run(&run.id)
                .ok()
                .flatten()
                .is_some_and(|row| row.settlement.is_some())
        },
    )
    .await;
    assert_eq!(
        wire_calls(cell.world.fake(), "agent.start").len(),
        1,
        "the orphan is never adopted and never re-started"
    );
    assert_eq!(task_prompts(cell.world.fake()), 0);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `start:0@result_committed`: the identity capture and the
/// `launched` compose committed before the abort — the run is
/// `prompting`, `prompt:task` `planned`; the restart lands the prompt
/// once → `active`+acknowledged.
#[tokio::test]
async fn s1_start_result_committed_continues() {
    let mut cell = kill_cell("start:0", Boundary::ResultCommitted, "new").await;
    let run = cell.run.as_ref().expect("a prompting run");
    assert_eq!(run.state, State::Prompting);
    let key = run_key(run, "start:0");
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
