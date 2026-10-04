//! §4.7 step 8 — the status-event subscription end to end: a
//! `pane.agent_status_changed` event drives the observation without
//! waiting for the periodic tick.

use super::*;

/// §4.7 step 8 — a `pane.agent_status_changed` event triggers an
/// observation for the resolved Run *without* waiting for the periodic
/// tick. `reconcile_secs` is an hour: the next tick cannot explain the
/// settlement; only the subscription path can.
#[tokio::test]
async fn reconcile_status_event_triggers_observation_before_the_next_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-evt", Some("sess-evt"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    // Seeded before `daemon::run`: the pane set reaches the maintainer
    // through `serve`'s initial push and the startup pass classifies the
    // run `unique` while the agent is still present.
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-evt", "w1:p1", &terminal, Some("sess-evt"), &inc);
    seed_run(&mut store, &run);
    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, 3_600));
    wait_bound(&state).await;

    // The maintainer arms once the pane set reaches it — the
    // `events.subscribe` request is the observable gate; the run being
    // classified `unique` proves tick zero already saw the agent.
    wait_for("the status-event subscription", || {
        fake.requests()
            .iter()
            .any(|(method, _)| method == "events.subscribe")
    })
    .await;
    wait_for("tick zero's unique classification", || {
        read_run(&store, "r-evt").child_status.is_some()
    })
    .await;
    // The occupant exits: the event (not the hour-away tick) must drive
    // the absence observation → `pane_lost`.
    fake.agent_exit("w1:p1");
    wait_for("the event-driven settlement", || {
        read_run(&store, "r-evt").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}

/// F28 — S7: Herdr restarts mid-Run — the socket file is recreated (the
/// incarnation moves), the armed stream EOFs and re-arms, and the
/// session-bearing identity re-proves by its native session alone: the
/// run stays `active`, keeps classifying under the foreign incarnation,
/// and a later exit still settles `pane_lost`.
#[tokio::test]
async fn f28_restart_reproves_identity_by_session() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-s7", Some("sess-s7"));
    let mut fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-s7", "w1:p1", &terminal, Some("sess-s7"), &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-s7").child_status.is_some()
    })
    .await;
    wait_for("the armed stream", || {
        fake.requests()
            .iter()
            .any(|(method, _)| method == "events.subscribe")
    })
    .await;

    fake.restart();
    let inc_after = socket_incarnation(fake.socket_path());
    assert_ne!(inc, inc_after, "a recreated socket is a new incarnation");

    // The EOF'd stream re-arms — a second `events.subscribe` lands.
    wait_for("the subscription re-arm", || {
        fake.requests()
            .iter()
            .filter(|(method, _)| method == "events.subscribe")
            .count()
            >= 2
    })
    .await;

    // Under the foreign incarnation the native session alone re-proves
    // the identity: a post-restart status change still classifies
    // `unique`, never `absent`/`invalid`/`identity_unprovable`.
    fake.set_agent_status("w1:p1", "blocked");
    wait_for("the re-proved classification", || {
        read_run(&store, "r-s7").child_status == Some(ChildStatus::Blocked)
    })
    .await;
    let current = read_run(&store, "r-s7");
    assert!(
        current.state == State::Active && current.settlement.is_none(),
        "a re-proved identity never settles: {:?}",
        current.state
    );

    fake.agent_exit("w1:p1");
    wait_for("the post-restart settlement", || {
        read_run(&store, "r-s7").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}
