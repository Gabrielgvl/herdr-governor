//! S8 — §4.7 step 1's untrusted-read rows, e2e: `session.snapshot`
//! unavailable, malformed or carrying a duplicated pane locator is never
//! an absence — no settlement while the read is unusable, health
//! degraded for the caller, deadlines still firing.

use super::*;

use crate::support::mcp_client::{
    McpClient, caller_envelope, canonical, status_call, status_page, tool_code,
};

/// F3/F22 — `session.snapshot` failing for several ticks neither
/// settles nor stalls the deadline sweep: an armed `max_age` still
/// fires inside the outage, `herdr_status` refuses `DAEMON_UNAVAILABLE`
/// while the caller cannot be resolved, and the untouched run settles
/// `pane_lost` only once a good read sees the exit.
#[tokio::test]
async fn f3_unavailable_snapshots_never_settle_but_deadlines_fire() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    // w1:p1 the run's child; w1:p2 a second child whose elapsed
    // `max_age` is the deadline witness and whose occupant doubles as
    // the caller `herdr_status` resolves through.
    let (mut topology, t1) = agent_topology("gov-r-snap", Some("sess-snap"));
    let (p2, t2) = occupied_tab(&mut topology, "w1", "gov-r-dl", "sess-dl");
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    // Latch the outage before the seeds land so every read that could
    // observe them fails — including the one that fires the deadline.
    fake.snapshot_fault(true);
    let run = active_run_on("r-snap", "w1:p1", &t1, Some("sess-snap"), &inc);
    seed_run(&mut store, &run);
    let mut deadline = active_run_on("r-dl", &p2, &t2, Some("sess-dl"), &inc);
    deadline.max_age_deadline = PAST;
    seed_run(&mut store, &deadline);

    // Deadlines still fire — the sweep runs even when every read fails.
    wait_for("the deadline to fire inside the outage", || {
        read_run(&store, "r-dl").settlement
            == Some(Settlement::Unresolved {
                reason: UnresolvedReason::MaxAge,
            })
    })
    .await;

    // No settlement on faulted reads — the request log counts them.
    let base = fake.requests().len();
    let span = Instant::now()
        .checked_add(Duration::from_millis(3_500))
        .unwrap();
    while Instant::now() < span {
        let current = read_run(&store, "r-snap");
        assert!(
            current.settlement.is_none(),
            "an unavailable snapshot is never an absence: {:?}",
            current.settlement
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let failed = fake
        .requests_since(base)
        .iter()
        .filter(|(method, _)| method == "session.snapshot")
        .count();
    assert!(
        failed >= 3,
        "the hold covered several faulted reads: {failed}"
    );

    // Health degraded — the caller-facing surface refuses while the
    // request-time read cannot resolve the caller (an internal fault,
    // never an identity verdict).
    let client = McpClient::new(
        &state.join("governor.sock"),
        caller_envelope(
            &p2,
            &canonical(tmp.path()),
            "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
        ),
    );
    let refused = client.call(&status_call(1)).await;
    assert_eq!(tool_code(&refused), "DAEMON_UNAVAILABLE");

    // Recovery: the first good read settles the real absence, and the
    // status surface serves `health.herdr` off the recovery read.
    fake.agent_exit("w1:p1");
    fake.snapshot_fault(false);
    wait_for("the recovered observation to settle", || {
        read_run(&store, "r-snap").settlement == Some(Settlement::PaneLost)
    })
    .await;
    let reply = client.call(&status_call(2)).await;
    let page = status_page(&reply);
    assert_eq!(
        page["health"]["herdr"]["incarnation"].as_str(),
        Some(inc.as_str()),
        "health recovered on the same incarnation"
    );
    stop(handle, shutdown).await;
}

/// F3 — malformed `session.snapshot` replies are `invalid` reads, never
/// absence: the run survives every faulted tick and settles `pane_lost`
/// only when a well-formed read sees the exit.
#[tokio::test]
async fn f3_malformed_snapshots_never_settle() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-raw", Some("sess-raw"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-raw", "w1:p1", &terminal, Some("sess-raw"), &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-raw").child_status.is_some()
    })
    .await;

    // Six malformed replies, queued before the exit: more than the ~4
    // `session.snapshot` reads a 3.5 s window can issue (the event-driven
    // read plus the 1 s ticks), so every read inside the hold faults.
    for _ in 0..6 {
        fake.fault("session.snapshot", Fault::malformed());
    }
    fake.agent_exit("w1:p1");
    let span = Instant::now()
        .checked_add(Duration::from_millis(3_500))
        .unwrap();
    while Instant::now() < span {
        let current = read_run(&store, "r-raw");
        assert!(
            current.settlement.is_none(),
            "a malformed reply is never an absence: {:?}",
            current.settlement
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    wait_for("the first well-formed read to settle", || {
        read_run(&store, "r-raw").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}

/// F3 — a duplicated pane locator makes the snapshot `invalid`: several
/// ticks change nothing, and a stale duplicate that still reports the
/// occupant even masks the exit — the absence settles `pane_lost` only
/// once the snapshot is trustworthy again.
#[tokio::test]
async fn f3_duplicated_locator_never_settles() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-dup", Some("sess-dup"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-dup", "w1:p1", &terminal, Some("sess-dup"), &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-dup").child_status.is_some()
    })
    .await;

    // A second `w1:p1` row still reporting the occupant — `view_of` and
    // `classify` both refuse the read.
    let duplicated = fake.state().topology.panes[0].clone();
    fake.state().topology.panes.push(duplicated);
    let span = Instant::now()
        .checked_add(Duration::from_millis(2_500))
        .unwrap();
    while Instant::now() < span {
        let current = read_run(&store, "r-dup");
        assert!(
            current.settlement.is_none(),
            "an invalid snapshot never settles: {:?}",
            current.settlement
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The exit is masked while the stale duplicate still reports the
    // agent — a trustworthy `absent` read never reached the loop.
    fake.agent_exit("w1:p1");
    let mask_span = Instant::now()
        .checked_add(Duration::from_millis(1_300))
        .unwrap();
    while Instant::now() < mask_span {
        assert!(
            read_run(&store, "r-dup").settlement.is_none(),
            "the stale duplicate masks the absence"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fake.state().topology.panes.pop();
    wait_for("the trustworthy read to settle", || {
        read_run(&store, "r-dup").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}
