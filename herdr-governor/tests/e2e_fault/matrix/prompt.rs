// — `prompt:task` ————————————————————————————————————————————————————

use super::*;

/// S1 — `prompt:task@pre_dispatch`: the never-committed task prompt
/// stays `planned`; the restart sends it once → `active`+acknowledged.
#[tokio::test]
async fn s1_prompt_pre_dispatch_relaunches_once() {
    let mut cell = kill_cell("prompt:task", Boundary::PreDispatch, "new").await;
    let run = cell.run.as_ref().expect("a prompting run");
    let key = run_key(run, "prompt:task");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Planned
    );
    assert_eq!(task_prompts(cell.world.fake()), 0);

    cell.world.start().await;
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    assert_eq!(task_prompts(cell.world.fake()), 1);
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `prompt:task@dispatch_committed`: the committed-but-never-wired
/// task prompt is `unconfirmed` at restart — the run goes `active` with
/// `prompt_certainty = unconfirmed`, the caller is notified once, and
/// the prompt never reaches the wire.
#[tokio::test]
async fn s1_prompt_dispatch_committed_is_unconfirmed() {
    let mut cell = kill_cell("prompt:task", Boundary::DispatchCommitted, "new").await;
    let run = cell.run.as_ref().expect("a prompting run");
    let key = run_key(run, "prompt:task");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert_eq!(task_prompts(cell.world.fake()), 0);

    cell.world.start().await;
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(current.prompt_certainty, Some(PromptCertainty::Unconfirmed));
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    let store = cell.world.store();
    assert_eq!(
        caller_events(&store, MailboxEventKind::PromptUnconfirmed).len(),
        1,
        "one caller notification for the unconfirmed prompt"
    );
    drop(store);
    assert_eq!(
        task_prompts(cell.world.fake()),
        0,
        "never sent, never resubmitted"
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `prompt:task@wire_returned`: the prompt landed but the result
/// died — `unconfirmed` at restart, `active`+unconfirmed, the caller
/// notified once, and the wire saw exactly one task prompt (the killed
/// send), never a retry.
#[tokio::test]
async fn s1_prompt_wire_returned_is_unconfirmed() {
    let mut cell = kill_cell("prompt:task", Boundary::WireReturned, "new").await;
    let run = cell.run.as_ref().expect("a prompting run");
    let key = run_key(run, "prompt:task");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Dispatching
    );
    assert_eq!(task_prompts(cell.world.fake()), 1);

    cell.world.start().await;
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(current.prompt_certainty, Some(PromptCertainty::Unconfirmed));
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Unconfirmed
    );
    let store = cell.world.store();
    assert_eq!(
        caller_events(&store, MailboxEventKind::PromptUnconfirmed).len(),
        1
    );
    drop(store);
    assert_eq!(
        task_prompts(cell.world.fake()),
        1,
        "the applied send is journaled, never re-sent"
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}

/// S1 — `prompt:task@result_committed`: the prompt's result committed
/// before the abort — the run is `active`+acknowledged and stays so;
/// the wire saw the one prompt.
#[tokio::test]
async fn s1_prompt_result_committed_stays_active() {
    let mut cell = kill_cell("prompt:task", Boundary::ResultCommitted, "new").await;
    let run = cell.run.as_ref().expect("an active run");
    assert_eq!(run.state, State::Active);
    assert_eq!(run.prompt_certainty, Some(PromptCertainty::Acknowledged));
    let key = run_key(run, "prompt:task");
    assert_eq!(
        effect_at(&cell.world.store(), &key).state,
        EffectState::Acknowledged
    );

    cell.world.start().await;
    let current = active_run(&cell.world, &cell.launch).await;
    assert_eq!(
        current.prompt_certainty,
        Some(PromptCertainty::Acknowledged)
    );
    assert_eq!(
        task_prompts(cell.world.fake()),
        1,
        "the committed prompt never re-sends"
    );
    assert_deadlines_unchanged(cell.run.as_ref(), &cell.world.store());
    cell.world.shutdown().await;
}
