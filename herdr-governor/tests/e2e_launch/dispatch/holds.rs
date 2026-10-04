//! `holds` — F10's fresh-verify holds: `blocked` and identity-`absent`
//! children are never prompted; the row stays `planned` for the next
//! hand-off — and a held or skipped prompt is re-offered at the
//! reconcile cadence, never by its own release (F4).

use governor_core::lifecycle::PromptCertainty;

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

/// The window the cadence cases count `session.snapshot` reads across.
const HOLD_WINDOW: Duration = Duration::from_secs(3);
/// The read budget for `HOLD_WINDOW` at `TICK_SECS = 1`: at most four
/// ticks, each one tick read plus the re-offered runner's fresh verify,
/// with slack for the observation feed. A hold that re-offers itself on
/// its own release spins hundreds of reads in the same window.
const HOLD_READS_MAX: usize = 12;

/// Plan `run:<run>:nudge:<n>` to `run`'s captured identity.
fn plan_nudge(store: &mut Store, run: &Run, n: u8) {
    let id = &run.id.0;
    store
        .apply(
            &plan(vec![effect(
                &format!("run:{id}:nudge:{n}"),
                EffectKind::Prompt,
                id,
                Some(EffectTarget::Child(run.identity.clone().expect("identity"))),
            )]),
            NOW,
        )
        .expect("plan nudge");
}

/// The `session.snapshot` requests `fake` served after request `base`.
fn snapshot_reads_since(fake: &FakeHerdr, base: usize) -> usize {
    fake.requests_since(base)
        .iter()
        .filter(|(method, _)| method == "session.snapshot")
        .count()
}

/// F10/F4 — a held prompt is re-offered at the reconcile cadence, never
/// by its own `ReleaseSubject`: across a blocked period the fresh-verify
/// reads stay one per tick instead of a retry loop at round-trip speed.
#[tokio::test]
async fn f10_blocked_hold_is_reoffered_at_tick_cadence() {
    let (mut topology, terminal) = agent_topology("gov-r-cad", Some("sess-cad"));
    topology.panes[0].agent.as_mut().expect("occupant").status = "blocked".to_owned();
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, TICK_SECS);

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let run = active_run_on("r-cad", "w1:p1", &terminal, Some("sess-cad"), &inc);
    seed_run(&mut store, &run);
    plan_nudge(&mut store, &run, 1);
    await_for("the blocked observation to land", || {
        read_run(&store, "r-cad").child_status.is_some()
    })
    .await;

    let base = fake.requests().len();
    never("agent.prompt on a blocked child", HOLD_WINDOW, || {
        fake.saw("agent.prompt")
    })
    .await;
    let reads = snapshot_reads_since(&fake, base);
    assert!(
        reads <= HOLD_READS_MAX,
        "a held prompt retries per tick, not per release: {reads} reads in {HOLD_WINDOW:?}"
    );
    assert_eq!(
        read_effect(&store, "run:r-cad:nudge:1").state,
        EffectState::Planned
    );
    daemon.shutdown().await;
}

/// F4 — the `Skip` twin: a nudge whose fresh verify passes but whose
/// commit gate holds it (the Task prompt's certainty is `unconfirmed` —
/// F9's ordering barrier) is re-offered at the reconcile cadence, never
/// by its own `DispatchCommit` `Skip`.
#[tokio::test]
async fn f4_skipped_prompt_is_reoffered_at_tick_cadence() {
    let (topology, terminal) = agent_topology("gov-r-skp", Some("sess-skp"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());
    let (dirs, settings) = world(&fake, TICK_SECS);

    let daemon = TestDaemon::start_in_process(&settings, None).await;

    let mut store = open_store(&dirs);
    bind_caller(&mut store);
    let mut run = active_run_on("r-skp", "w1:p1", &terminal, Some("sess-skp"), &inc);
    run.prompt_certainty = Some(PromptCertainty::Unconfirmed);
    seed_run(&mut store, &run);
    plan_nudge(&mut store, &run, 1);
    await_for("unique classification", || {
        read_run(&store, "r-skp").child_status.is_some()
    })
    .await;

    let base = fake.requests().len();
    never("agent.prompt behind the barrier", HOLD_WINDOW, || {
        fake.saw("agent.prompt")
    })
    .await;
    let reads = snapshot_reads_since(&fake, base);
    assert!(
        reads <= HOLD_READS_MAX,
        "a skipped prompt retries per tick, not per skip: {reads} reads in {HOLD_WINDOW:?}"
    );
    assert_eq!(
        read_effect(&store, "run:r-skp:nudge:1").state,
        EffectState::Planned
    );
    daemon.shutdown().await;
}
