//! `wire` — the commit→wire→ack happy path: F8's ordering, F9/F25's
//! rendered prompt texts, F20's cancel close.

use super::*;

/// F8 — the journal commits `dispatching` before the wire op can return:
/// with `pane.close`'s reply delayed, the wire is mid-flight while the
/// row already reads `dispatching` with `dispatched_at` stamped. A crash
/// in that window finds the durable mark, never a `planned` row the
/// restart could re-dispatch onto an already-closed pane.
#[tokio::test]
async fn f8_commit_precedes_the_wire() {
    let (topology, terminal) = agent_topology("gov-r-f8", Some("sess-f8"));
    let fake = FakeHerdr::start(topology);
    fake.fault("pane.close", Fault::Delay(Duration::from_millis(600)));
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, 3600);

    // Seeded before spawn: the startup `hand_out` offers it, and a 3600s
    // reconcile cadence means no observation interposes mid-flight.
    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-f8", "w1:p1", &terminal, Some("sess-f8"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-f8:close",
                EffectKind::Close,
                "r-f8",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan close");

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    // The wire op is in flight (the fake recorded it, the reply is still
    // delayed) and the journal already shows the commit — the mutation
    // ordering §4.4 guarantees.
    await_for("pane.close on the wire", || fake.saw("pane.close")).await;
    let in_flight = read_effect(&store, "run:r-f8:close");
    assert_eq!(
        in_flight.state,
        EffectState::Dispatching,
        "the durable commit precedes the wire return — a crash here finds dispatching"
    );
    assert!(in_flight.dispatched_at.is_some());

    await_for("the ack to commit", || {
        read_effect(&store, "run:r-f8:close").state == EffectState::Acknowledged
    })
    .await;
    daemon.shutdown().await;
}

/// F20 — the cancel leg end to end: a `close` effect on the captured
/// identity dispatches `pane.close` against the re-verified locator; the
/// pane is gone from the topology and the row acknowledges.
#[tokio::test]
async fn f20_cancel_closes_the_pane() {
    let (topology, terminal) = agent_topology("gov-r-f20", Some("sess-f20"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, 3600);

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-f20", "w1:p1", &terminal, Some("sess-f20"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-f20:close",
                EffectKind::Close,
                "r-f20",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan close");

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    await_for("the close to commit", || {
        read_effect(&store, "run:r-f20:close").state == EffectState::Acknowledged
    })
    .await;
    let requests = fake.requests();
    let close = requests
        .iter()
        .find(|(m, _)| m == "pane.close")
        .expect("pane.close reached the wire");
    assert_eq!(
        close.1.get("pane_id").and_then(|v| v.as_str()),
        Some("w1:p1"),
        "the re-verified locator was closed"
    );
    assert!(
        fake.state()
            .topology
            .panes
            .iter()
            .all(|p| p.pane_id != "w1:p1"),
        "the pane is gone"
    );
    daemon.shutdown().await;
}

/// F9/F16/F25 — the prompt renders at hand-off and lands verbatim: the
/// task prompt carries the Task contract and the handoff block; the
/// episode nudge carries its reminder. Both `agent.prompt` texts are
/// asserted on the wire, and both rows acknowledge.
#[tokio::test]
async fn f9_prompts_render_and_land() {
    let (topology, terminal) = agent_topology("gov-r-p9", Some("sess-p9"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, TICK_SECS);

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-p9", "w1:p1", &terminal, Some("sess-p9"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");

    // `prompt:task` dispatches only while the Run is `prompting` (F9's
    // order — the task prompt precedes every other message to the child).
    move_state(&mut store, "r-p9", State::Prompting);
    let task_prompt = effect(
        "run:r-p9:prompt:task",
        EffectKind::Prompt,
        "r-p9",
        Some(EffectTarget::Child(target.clone())),
    );
    store
        .apply(&plan(vec![task_prompt]), NOW)
        .expect("plan task prompt");

    await_for("the task prompt to commit", || {
        read_effect(&store, "run:r-p9:prompt:task").state == EffectState::Acknowledged
    })
    .await;
    let text = fake.prompt_text("w1:p1");
    assert!(
        text.contains("OBJECTIVE-MARK") && text.contains("DONE-MARK"),
        "the Task's contract fields render onto the wire: {text}"
    );
    assert!(
        text.contains("r-p9") && text.contains("handoff"),
        "the handoff block names the run's file: {text}"
    );

    // The run moves on; the episode's nudge is the next prompt.
    move_state(&mut store, "r-p9", State::Active);
    let nudge = effect(
        "run:r-p9:nudge:1",
        EffectKind::Prompt,
        "r-p9",
        Some(EffectTarget::Child(target)),
    );
    store.apply(&plan(vec![nudge]), NOW).expect("plan nudge");

    await_for("the nudge to commit", || {
        read_effect(&store, "run:r-p9:nudge:1").state == EffectState::Acknowledged
    })
    .await;
    let nudged = fake
        .requests()
        .iter()
        .filter(|(m, _)| m == "agent.prompt")
        .count();
    assert_eq!(nudged, 2, "task prompt then nudge");
    let nudge_text = fake.prompt_text_at(1);
    assert!(
        nudge_text.contains("still working?"),
        "the nudge body renders: {nudge_text}"
    );
    assert!(
        nudge_text.contains("r-p9"),
        "the handoff path names the run: {nudge_text}"
    );
    daemon.shutdown().await;
}
