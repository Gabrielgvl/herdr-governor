//! `holds` — F10's fresh-verify holds: `blocked` and identity-`absent`
//! children are never prompted; the row stays `planned` for the next
//! hand-off.

use super::*;

/// F10/H#17 — never prompt a `blocked` child: the fresh verify holds the
/// nudge `planned` and nothing reaches `agent.prompt`. Unblocking
/// releases the same row — a hold, not a failure.
#[tokio::test]
async fn f10_blocked_child_is_never_prompted() {
    let (mut topology, terminal) = agent_topology("gov-r-blk", Some("sess-blk"));
    topology.panes[0].agent.as_mut().expect("occupant").status = "blocked".to_owned();
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, TICK_SECS);

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-blk", "w1:p1", &terminal, Some("sess-blk"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-blk:nudge:1",
                EffectKind::Prompt,
                "r-blk",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan nudge");

    // Across several ticks the nudge must never dispatch: `blocked` at the
    // fresh verify (and, once observed, at `prompt_dispatchable` too).
    await_for("the blocked observation to land", || {
        read_run(&store, "r-blk").child_status.is_some()
    })
    .await;
    never(
        "agent.prompt on a blocked child",
        Duration::from_secs(3),
        || fake.saw("agent.prompt"),
    )
    .await;
    assert_eq!(
        read_effect(&store, "run:r-blk:nudge:1").state,
        EffectState::Planned
    );

    // Unblock: the same row dispatches and acknowledges.
    fake.set_agent_status("w1:p1", "working");
    await_for("the released nudge to commit", || {
        read_effect(&store, "run:r-blk:nudge:1").state == EffectState::Acknowledged
    })
    .await;
    assert!(fake.saw("agent.prompt"));
    daemon.shutdown().await;
}

/// F10 — the captured identity no longer proves (the occupant's session
/// re-minted): the prompt is held at the fresh verify and `agent.prompt`
/// never reaches the wire — the row's `dispatched_at` stays empty
/// whatever terminal state settle eventually writes.
#[tokio::test]
async fn f10_absent_identity_never_dispatches() {
    // The scripted occupant's session is NOT the captured one.
    let (topology, terminal) = agent_topology("gov-r-gone", Some("sess-other"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, TICK_SECS);

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-gone", "w1:p1", &terminal, Some("sess-captured"), &inc);
    seed_run(&mut store, &run);
    let target = run.identity.clone().expect("identity");
    store
        .apply(
            &plan(vec![effect(
                "run:r-gone:nudge:1",
                EffectKind::Prompt,
                "r-gone",
                Some(EffectTarget::Child(target)),
            )]),
            NOW,
        )
        .expect("plan nudge");

    never(
        "agent.prompt on an unproven identity",
        Duration::from_secs(3),
        || fake.saw("agent.prompt"),
    )
    .await;
    assert!(
        read_effect(&store, "run:r-gone:nudge:1")
            .dispatched_at
            .is_none(),
        "the wire never saw it"
    );
    daemon.shutdown().await;
}
