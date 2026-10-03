//! §4.7 step 2 — the deadline sweep end to end: every armed deadline is
//! attempted in one pass, and an elapsed `max_age` settles within one
//! tick of being seen (N2).

use super::*;

/// F22 — every armed deadline is attempted in one sweep: a run with all
/// four armed in the past settles on the first tick that sees it.
#[tokio::test]
async fn f22_every_armed_deadline_fires_within_one_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-dl", Some("sess-dl"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let mut run = active_run_on("r-dl", "w1:p1", &terminal, Some("sess-dl"), &inc);
    run.idle_deadline = Some(PAST);
    run.repair_deadline = Some(PAST);
    run.judgment_deadline = Some(PAST);
    run.max_age_deadline = PAST;
    seed_run(&mut store, &run);

    wait_for("the elapsed deadlines to fire", || {
        read_run(&store, "r-dl").settlement.is_some()
    })
    .await;
    stop(handle, shutdown).await;
}

/// N2 — an elapsed `max_age` settles within one tick of being seen.
#[tokio::test]
async fn n2_settles_within_max_age_plus_one_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-max", Some("sess-max"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let mut run = active_run_on("r-max", "w1:p1", &terminal, Some("sess-max"), &inc);
    run.max_age_deadline = PAST;
    seed_run(&mut store, &run);

    wait_for("the max-age settlement", || {
        read_run(&store, "r-max").settlement.is_some()
    })
    .await;
    stop(handle, shutdown).await;
}
