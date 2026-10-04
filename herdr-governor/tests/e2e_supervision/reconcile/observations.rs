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

/// F3 — S4: `replace_pane` takes the pane's whole locator away — new
/// `pane_id`, fresh `terminal_id`, no agent — so the captured identity
/// matches nothing: `active` × `absent` with no frozen handoff settles
/// `pane_lost`.
#[tokio::test]
async fn f3_pane_replacement_is_pane_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-s4", Some("sess-s4"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-s4", "w1:p1", &terminal, Some("sess-s4"), &inc);
    seed_run(&mut store, &run);
    wait_for("unique classification", || {
        read_run(&store, "r-s4").child_status.is_some()
    })
    .await;

    let replacement = fake.replace_pane("w1:p1");
    let (new_terminal, occupied) = {
        let guard = fake.state();
        let pair = guard
            .topology
            .pane(&replacement)
            .map(|pane| (pane.terminal_id.clone(), pane.agent.is_some()))
            .expect("replacement row");
        drop(guard);
        pair
    };
    assert_ne!(replacement, "w1:p1", "the pane id moves");
    assert_ne!(new_terminal, terminal, "the terminal is fresh");
    assert!(!occupied, "the replacement is a shell — the child is gone");

    wait_for("pane_lost settlement", || {
        read_run(&store, "r-s4").settlement == Some(Settlement::PaneLost)
    })
    .await;
    stop(handle, shutdown).await;
}

/// F25 — S4b's judged-first kernel: a persisted `handoffs` row at the
/// run's current work generation turns `active` × `absent` into
/// `judging`, never `pane_lost`. PR C owns the freeze write that mints
/// the row in the live flow and the acceptance leg it resumes — the row
/// itself is seeded; what this asserts is the reconcile outcome:
/// `judging`, an armed `judgment_deadline`, no settlement.
#[tokio::test]
async fn f25_frozen_unjudged_handoff_judges_first_when_child_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let (topology, terminal) = agent_topology("gov-r-frz", Some("sess-frz"));
    let fake = FakeHerdr::start(topology);
    let inc = socket_incarnation(fake.socket_path());

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    let run = active_run_on("r-frz", "w1:p1", &terminal, Some("sess-frz"), &inc);
    seed_run(&mut store, &run);
    store
        .apply(
            &changes(vec![StateChange::FreezeHandoff(FrozenHandoff {
                run: RunId("r-frz".into()),
                work_generation: 0,
                digest: Digest([0x42; 32]),
                frozen_path: "/frozen/handoff.md".into(),
                frozen_at: NOW,
                assessed: false,
            })]),
            NOW,
        )
        .expect("freeze handoff");
    wait_for("unique classification", || {
        read_run(&store, "r-frz").child_status.is_some()
    })
    .await;

    fake.agent_exit("w1:p1");
    wait_for("the judged-first transition", || {
        read_run(&store, "r-frz").state == State::Judging
    })
    .await;
    let current = read_run(&store, "r-frz");
    assert!(
        current.settlement.is_none() && current.judgment_deadline.is_some(),
        "frozen-unjudged absence judges first, never pane_lost: {:?}",
        current.state
    );
    stop(handle, shutdown).await;
}

/// The `Decision` the seeded `routed` launch carries — one
/// `start`-qualified candidate at `op-a` (the F15 commit gate's shape).
fn decision() -> Decision {
    Decision {
        judged_tier: Tier("standard".into()),
        requested_tier: None,
        policy_cap: None,
        policy_floor: None,
        caller_uplift: None,
        recovery_minimum: None,
        exploration: Exploration {
            assigned: false,
            executed: false,
        },
        start_tier: Tier("standard".into()),
        candidates: vec![Candidate {
            operating_point: OperatingPointId("op-a".into()),
            provider: Provider("vendor-a".into()),
            tier: Tier("standard".into()),
            harness: AgentKind("kind-a".into()),
            args: vec!["--a".to_owned()],
        }],
        config_version: ConfigVersion("seeded".into()),
    }
}

/// F3 — S14c: a `tab.create` whose reply lags 8 s holds the run
/// `starting` across three reconcile ticks — an in-flight topology leg
/// is never name-only absence — then the pipeline proceeds: the tab ack
/// plans `start:0`, `agent.start` captures the identity and the prompt
/// leg completes `active`.
#[tokio::test]
async fn f3_delayed_tab_create_holds_then_start_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    // The caller occupies w1:p1 — `tab.create`'s caller-context verify
    // re-resolves its workspace by native session at the wire.
    let (topology, _terminal) = agent_topology("gov-caller", Some("sess-caller-1"));
    let fake = FakeHerdr::start(topology);
    fake.fault("tab.create", Fault::Delay(Duration::from_secs(8)));

    let (handle, shutdown) = spawn_daemon(settings(&state, &config, &fake, TICK_SECS));
    wait_bound(&state).await;
    let mut store = open_store(&state);
    bind_caller(&mut store);
    // A `starting` run on a `routed` launch that carries its decision;
    // `run:r-slow:tab` is journaled `planned` exactly as `begin` left it —
    // the ack itself plans `start:0` (F15).
    let mut run = run_row_on("r-slow", "l-slow");
    run.state = State::Starting;
    let mut routed = launch_row("l-slow", LaunchPhase::Routed);
    routed.decision = Some(decision());
    store
        .apply(
            &changes(vec![
                StateChange::RecordLaunch(launch_row("l-slow", LaunchPhase::Evaluating)),
                StateChange::RecordLaunch(routed),
                StateChange::ReserveRun(run),
            ]),
            NOW,
        )
        .expect("launch + reserve");
    store
        .record_qualification(
            &Qualification {
                operating_point: OperatingPointId("op-a".into()),
                args_digest: args_digest(["--a"].iter().copied()),
                capability: Capability(Capability::START.into()),
                passed: true,
                evidence: "{}".into(),
            },
            NOW,
        )
        .expect("qualify");
    let target = EffectTarget::CallerContext(PaneId("w1:p1".into()));
    let mut tab = effect("run:r-slow:tab", EffectKind::TabCreate, "r-slow");
    tab.target = Some(target.clone());
    tab.payload_digest = Some(op_digest(EffectKind::TabCreate, Some(&target), &[]));
    store
        .apply(
            &Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: vec![tab],
            },
            NOW,
        )
        .expect("plan tab");

    // The dispatch reaches Herdr — the reply is what stalls.
    wait_for("the tab.create request", || fake.saw("tab.create")).await;
    // Three 1 s ticks inside the 8 s reply delay: the in-flight leg is
    // not a terminated one, so the name-only absence rule cannot fire.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let current = read_run(&store, "r-slow");
        assert!(
            current.state == State::Starting
                && current.settlement.is_none()
                && current.identity.is_none(),
            "an in-flight topology leg never settles: {:?}",
            current.state
        );
        assert!(
            !fake.saw("agent.start"),
            "the start leg cannot precede its tab ack"
        );
    }

    wait_for("the start to proceed", || {
        read_run(&store, "r-slow").state == State::Active
    })
    .await;
    let current = read_run(&store, "r-slow");
    assert!(
        current.identity.is_some(),
        "the start ack captured identity"
    );
    assert!(
        fake.saw("agent.start") && fake.saw("agent.prompt"),
        "the whole start leg ran"
    );
    stop(handle, shutdown).await;
}
