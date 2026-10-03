//! `terminal` — the failed-terminal honesty cases: F16's ack mismatch
//! commits `unknown`; F29's pre-wire shutdown gate commits `absent`.

use super::*;

/// F16 — the `agent.prompt` ack must name the captured identity it was
/// sent to. The seam pauses the committed dispatch; the occupant re-mints
/// under it; the ack's agent no longer matches — the write may have landed
/// on the wrong agent, so `unknown` is the only honest certainty.
#[tokio::test]
async fn f16_ack_identity_mismatch_is_unknown() {
    let (topology, terminal) = agent_topology("gov-r-ack", Some("sess-ack"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, 3600);

    // Pre-spawn seed on a `prompting` run; 3600s reconcile keeps any
    // observation of the swapped occupant out of the window.
    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let mut run = active_run_on("r-ack", "w1:p1", &terminal, Some("sess-ack"), &inc);
    run.state = State::Prompting;
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-ack:prompt:task",
                EffectKind::Prompt,
                "r-ack",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan task prompt");

    let daemon =
        TestDaemon::start_in_process(&settings, Some(pause_at_dispatch("prompt:task"))).await;

    // `dispatching` is visible exactly when the seam pause begins — swap
    // the occupant inside the window.
    await_for("the dispatch commit", || {
        read_effect(&store, "run:r-ack:prompt:task").state == EffectState::Dispatching
    })
    .await;
    let reminted = fake.replace_occupant("w1:p1");
    assert_ne!(reminted, "sess-ack", "the occupant re-minted");

    await_for("the refused ack to commit", || {
        read_effect(&store, "run:r-ack:prompt:task").state == EffectState::Failed
    })
    .await;
    let row = read_effect(&store, "run:r-ack:prompt:task");
    assert_eq!(
        row.certainty,
        Some(EffectCertainty::Unknown),
        "the write may have landed on the wrong agent"
    );
    let json = result_json_of(&store, "run:r-ack:prompt:task").expect("a cause is journaled");
    assert!(
        json.contains("ack_identity_mismatch"),
        "the F16 cause is on the row: {json}"
    );
    assert!(
        fake.saw("agent.prompt"),
        "the mutation itself ran — the ack was the mismatch"
    );
    daemon.shutdown().await;
}

/// F29/§4.14 — the pre-wire gate: shutdown raised between the durable
/// commit and the wire op answers `Failed{Absent,"shutdown_before_wire"}`
/// — the mutation provably never ran — and the drain still commits the
/// result before the daemon exits.
#[tokio::test]
async fn f29_shutdown_before_wire_fails_absent() {
    let (topology, terminal) = agent_topology("gov-r-f29", Some("sess-f29"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, 3600);

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-f29", "w1:p1", &terminal, Some("sess-f29"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-f29:close",
                EffectKind::Close,
                "r-f29",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan close");

    let daemon = TestDaemon::start_in_process(&settings, Some(pause_at_dispatch("close"))).await;

    // The row is `dispatching` — the seam pause holds the runner between
    // the commit and the wire; the shutdown flag lands inside it.
    await_for("the dispatch commit", || {
        read_effect(&store, "run:r-f29:close").state == EffectState::Dispatching
    })
    .await;
    daemon.shutdown().await;

    let row = read_effect(&store, "run:r-f29:close");
    assert_eq!(
        (row.state, row.certainty),
        (EffectState::Failed, Some(EffectCertainty::Absent)),
        "shutdown before the wire provably never ran"
    );
    let json = result_json_of(&store, "run:r-f29:close").expect("the gate's cause");
    assert!(
        json.contains("shutdown_before_wire"),
        "the §4.14 cause is on the row: {json}"
    );
    assert!(!fake.saw("pane.close"), "the wire never saw the close");
}
