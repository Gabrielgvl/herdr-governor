//! F3 — the observation classes end to end: a move is followed, a new
//! native session reads `absent`, and a slow-but-healthy launch leg
//! never settles on name-only absence.

use super::*;

/// F3 — a workspace move renumbers the pane but keeps terminal, name and
/// session: the observation stays `unique`, the identity's locator is
/// followed to the new pane, and the run never goes absent.
#[tokio::test]
async fn f3_move_is_followed_not_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (mut topology, terminal) = agent_topology("gov-r-move", Some("sess-move"));
    // The move target needs a tab — a workspace without one serializes a
    // null `active_tab_id` the pinned schema decodes as absent-and-invalid,
    // making every snapshot unreadable.
    let _second = topology.create_workspace("second");
    let (_tab, _shell) = topology.create_tab("w2");
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-move", "w1:p1", &terminal, Some("sess-move"), &inc);
    seed_run(&mut store, &run);

    // Let one tick classify it `unique` at w1:p1, then move the pane.
    wait_for("unique classification", || {
        read_run(&store, "r-move").child_status.is_some()
    })
    .await;
    let moved = fake.move_to_workspace("w1:p1", "w2");
    assert_eq!(moved, "w2:p2", "the knob renumbers into w2");

    wait_for("the move to be followed", || {
        let current = read_run(&store, "r-move");
        current
            .identity
            .as_ref()
            .is_some_and(|identity| identity.pane_id.0 == moved)
    })
    .await;
    let current = read_run(&store, "r-move");
    assert!(
        current.settlement.is_none() && current.state == State::Active,
        "a followed move never settles: {:?}",
        current.state
    );
    stop(handle, shutdown).await;
}

/// F1/F3 — a same-pane replacement occupant carries a *different* native
/// session: the stable identity no longer proves, the run reads `absent`
/// and settles `pane_lost`.
#[tokio::test]
async fn f3_new_session_in_pane_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-rep", Some("sess-old"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-rep", "w1:p1", &terminal, Some("sess-old"), &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-rep").child_status.is_some()
    })
    .await;

    let new_session = fake.replace_occupant("w1:p1");
    assert_ne!(new_session, "sess-old", "the occupant re-minted");

    wait_for("pane_lost settlement", || {
        read_run(&store, "r-rep").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}

/// F3 slow-topology — a `starting` Run whose only start leg is still
/// `dispatching` is *not* absent-by-name: the attempted-and-terminated
/// rule holds the observation back while the leg is in flight.
#[tokio::test]
async fn f3_slow_topology_never_settles_a_healthy_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    // No agent named `gov-r-slow` anywhere in the scripted world.
    let fake = FakeHerdr::start(Topology::single_shell());
    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;

    // Seeded after the bind so mark_restart can't reclassify the leg.
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let mut run = run_row_on("r-slow", "l-slow");
    run.state = State::Starting;
    seed_run(&mut store, &run);
    seed_dispatching(
        &mut store,
        "run:r-slow:start",
        EffectKind::AgentStart,
        "r-slow",
    );

    // Several whole ticks with no settle — the leg in flight holds the
    // absence back the whole time.
    let span = Instant::now()
        .checked_add(Duration::from_millis(3_500))
        .unwrap();
    while Instant::now() < span {
        let current = read_run(&store, "r-slow");
        assert!(
            current.settlement.is_none() && current.state == State::Starting,
            "a dispatching start leg must hold the run: {:?}",
            current.state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop(handle, shutdown).await;
}
