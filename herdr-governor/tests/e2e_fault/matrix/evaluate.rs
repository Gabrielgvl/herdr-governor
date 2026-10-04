// — `evaluate` ———————————————————————————————————————————————————————

use super::*;

/// S1 — `evaluate@pre_dispatch`: the never-committed ask stays `planned`;
/// the restart hands it out once and the whole launch lands `launched`.
#[tokio::test]
async fn s1_evaluate_pre_dispatch_relaunches_once() {
    let mut cell = kill_cell("evaluate", Boundary::PreDispatch, "new").await;
    assert_eq!(cell.run, None, "no run exists before decide");
    assert!(cell.world.jev().requests().is_empty(), "never asked");
    let key = format!("launch:{}:evaluate", cell.launch.id.0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Planned,
        "the aborted dispatch never committed"
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(
        cell.world.evals().len(),
        1,
        "the journaled ask dispatches exactly once"
    );
    assert_eq!(wire_calls(cell.world.fake(), "tab.create").len(), 1);
    assert_eq!(wire_calls(cell.world.fake(), "agent.start").len(), 1);
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(task_prompts(cell.world.fake()), 1);
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    cell.world.shutdown().await;
}

/// S1 — `evaluate@dispatch_committed`: a `dispatching` ask the wire
/// never saw is `unconfirmed` at restart — the F8 abstain row
/// (`interrupted_before_decision`), no run, no re-ask.
#[tokio::test]
async fn s1_evaluate_dispatch_committed_abstains_interrupted() {
    let mut cell = kill_cell("evaluate", Boundary::DispatchCommitted, "new").await;
    let key = format!("launch:{}:evaluate", cell.launch.id.0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching,
        "the commit landed, the wire never ran"
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_eq!(
        launch.outcome,
        Some(LaunchOutcome::Abstained {
            reason: AbstainReason::InterruptedBeforeDecision
        }),
        "{:?}",
        launch.outcome
    );
    let eval = effect_at(&cell.world.store(), &key);
    assert_eq!(eval.state, EffectState::Failed);
    assert_eq!(eval.certainty, Some(EffectCertainty::Unknown));
    assert!(
        cell.world.jev().requests().is_empty(),
        "the committed ask never reached the wire and is never re-asked"
    );
    let store = cell.world.store();
    assert_eq!(
        store.run_by_launch(&launch.id).ok().flatten(),
        None,
        "an interrupted evaluation reserves no run"
    );
    drop(store);
    cell.world.shutdown().await;
}

/// S1 — `evaluate@wire_returned`: the answer came back but its commit
/// died — the restart marks the ask `unconfirmed` and abstains; the
/// wire saw exactly one ask, never a second.
#[tokio::test]
async fn s1_evaluate_wire_returned_abstains_interrupted() {
    let mut cell = kill_cell("evaluate", Boundary::WireReturned, "new").await;
    assert_eq!(
        cell.world.jev().requests().len(),
        1,
        "the ask reached the wire pre-kill"
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert_eq!(
        launch.outcome,
        Some(LaunchOutcome::Abstained {
            reason: AbstainReason::InterruptedBeforeDecision
        }),
        "{:?}",
        launch.outcome
    );
    let key = format!("launch:{}:evaluate", cell.launch.id.0);
    let eval = effect_at(&cell.world.store(), &key);
    assert_eq!(eval.state, EffectState::Failed);
    assert_eq!(eval.certainty, Some(EffectCertainty::Unknown));
    assert_eq!(
        cell.world.jev().requests().len(),
        1,
        "the answer is journaled unknown, never re-asked"
    );
    cell.world.shutdown().await;
}

/// S1 — `evaluate@result_committed`: decide + `begin` + `launch_plan`
/// committed before the abort — the run is `starting`, the topology leg
/// `planned`; the restart continues the pipeline to `launched`/`active`
/// with no second ask.
#[tokio::test]
async fn s1_evaluate_result_committed_continues() {
    let mut cell = kill_cell("evaluate", Boundary::ResultCommitted, "new").await;
    let run = cell.run.as_ref().expect("the run is reserved by then");
    assert_eq!(run.state, State::Starting);
    let key = format!("launch:{}:evaluate", cell.launch.id.0);
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Acknowledged,
        "the evaluation's result committed pre-kill"
    );

    cell.world.start().await;
    let launch = finished_launch(&cell.world).await;
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "{:?}",
        launch.outcome
    );
    assert_eq!(
        cell.world.evals().len(),
        1,
        "the committed evaluation is never re-asked"
    );
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    assert_eq!(wire_calls(cell.world.fake(), "tab.create").len(), 1);
    assert_eq!(wire_calls(cell.world.fake(), "agent.start").len(), 1);
    assert_eq!(task_prompts(cell.world.fake()), 1);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}
