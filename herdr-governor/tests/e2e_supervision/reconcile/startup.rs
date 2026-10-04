//! §4.3 steps 5–7 — the startup-scan rows and the foreign-incarnation
//! settlement, e2e: `mark_restart`'s global dispatching scan reclassifies
//! a settled Run's stranded effect, and a sessionless identity settles
//! `identity_unprovable` only under a *valid foreign* incarnation.

use super::*;

/// F8 [r2] — a `dispatching` effect on a *settled* Run is still found by
/// §4.3 step 5's global scan and reclassified `unconfirmed` at startup.
#[tokio::test]
async fn f8_restart_marks_dispatching_close_on_settled_run() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let fake = FakeHerdr::start(Topology::single_shell());

    // Seed strictly before `daemon::run`: mark_restart is a startup step.
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let mut run = run_row_on("r-8", "l-8");
    seed_run(&mut store, &run);
    let settling = settle(&run, Settlement::Cancelled, NOW, &policy());
    store.apply(&settling, NOW).expect("settle");
    run = read_run(&store, "r-8");
    assert!(run.settlement.is_some(), "the run is settled");
    seed_dispatching(&mut store, "run:r-8:close", EffectKind::Close, "r-8");
    drop(store);

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;

    let check = open_store(&state);
    let journal = check.journal(&RunId("r-8".into())).expect("journal");
    let close = journal
        .iter()
        .find(|e| e.key.0 == "run:r-8:close")
        .expect("close effect");
    assert_eq!(
        close.state,
        EffectState::Unconfirmed,
        "the settled run's dispatching close is unconfirmed"
    );
    stop(handle, shutdown).await;
}

/// F28 — a sessionless identity survives a same-incarnation snapshot but
/// settles `identity_unprovable` once a *valid foreign* incarnation
/// reads: `fake.restart()` swaps the socket file, so the next snapshot's
/// epoch mints a different incarnation.
#[tokio::test]
async fn f28_sessionless_run_settles_identity_unprovable_only_on_valid_foreign_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    // The occupant carries no `agent_session` — the identity is provable
    // only under its own incarnation.
    let (topology, terminal) = agent_topology("gov-r-28", None);
    let mut fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-28", "w1:p1", &terminal, None, &inc);
    seed_run(&mut store, &run);

    // Same incarnation: classified `unique`/`absent` normally — never
    // `identity_unprovable`.
    let span = Instant::now()
        .checked_add(Duration::from_millis(2_500))
        .unwrap();
    while Instant::now() < span {
        let current = read_run(&store, "r-28");
        assert!(
            current.settlement.is_none(),
            "same-incarnation snapshots never settle unprovable: {:?}",
            current.settlement
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fake.restart();
    wait_for("the foreign-incarnation settlement", || {
        read_run(&store, "r-28").settlement
            == Some(Settlement::Unresolved {
                reason: UnresolvedReason::IdentityUnprovable,
            })
    })
    .await;
    stop(handle, shutdown).await;
}

/// F28 — S7b's other half: the foreign-incarnation settlement needs a
/// *valid* read. Unavailable and invalid post-restart snapshots both
/// hold the sessionless Run — `identity_unprovable` lands only once a
/// good foreign snapshot reaches the loop.
#[tokio::test]
async fn f28_sessionless_run_needs_a_valid_foreign_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-28b", None);
    let mut fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-28b", "w1:p1", &terminal, None, &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-28b").child_status.is_some()
    })
    .await;

    // Restart under a latched outage: the incarnation is foreign but
    // every read still fails — no `identity_unprovable`.
    fake.snapshot_fault(true);
    fake.restart();
    let span = Instant::now()
        .checked_add(Duration::from_millis(2_000))
        .unwrap();
    while Instant::now() < span {
        assert!(
            read_run(&store, "r-28b").settlement.is_none(),
            "unavailable foreign snapshots never settle unprovable"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Reads recover but the snapshot is invalid (duplicated locator) —
    // still no settlement under the foreign incarnation.
    let duplicated = fake.state().topology.panes[0].clone();
    fake.state().topology.panes.push(duplicated);
    fake.snapshot_fault(false);
    let invalid_span = Instant::now()
        .checked_add(Duration::from_millis(2_000))
        .unwrap();
    while Instant::now() < invalid_span {
        assert!(
            read_run(&store, "r-28b").settlement.is_none(),
            "invalid foreign snapshots never settle unprovable"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The first *valid* foreign read settles it.
    fake.state().topology.panes.pop();
    wait_for("the valid foreign settlement", || {
        read_run(&store, "r-28b").settlement
            == Some(Settlement::Unresolved {
                reason: UnresolvedReason::IdentityUnprovable,
            })
    })
    .await;
    stop(handle, shutdown).await;
}
