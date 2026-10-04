//! `restart` — the fault suite's restart scenarios, every one against a
//! real `herdr-governor daemon` child: S27's elapsed-deadline sweep
//! inside the startup pass, S27b's `close` killed at its dispatch
//! commit on a settled Run, S27d's `SIGKILL` stale-socket recovery with
//! the lock still held, and F28's global `dispatching` scan — every
//! assertion lands at bind, the moment `governor.sock` exists, so the
//! startup pass is what produced them.

use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;

use governor_core::identity::{PaneId, TabId};
use governor_core::lifecycle::{
    EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectTarget, Settlement, State,
    StateChange, Transition, UnresolvedReason, settle,
};
use governor_core::routing::PlacementPlan;
use governor_core::task::{AbstainReason, LaunchOutcome, LaunchPhase};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};

use crate::support::daemon::{
    Catalog, DaemonDirs, TestDaemon, await_for, fixture as write_dirs, probe, socket_incarnation,
};
use crate::support::fake_herdr::topology::{Occupant, SessionRef, Topology, agent_topology};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::store::Store;

use super::*;

/// `w1` with a second tab — `w1:p1` and `w1:p2` both occupied; returns
/// the topology plus `(w1:p1, w1:p2)` terminal ids, the identity fields
/// a captured `ChildIdentity` must match.
fn two_agent_topology() -> (Topology, String, String) {
    let mut topology = Topology::single_shell();
    let (_tab, _pane) = topology.create_tab("w1");
    let terminal1 = topology
        .panes
        .iter()
        .find(|pane| pane.pane_id == "w1:p1")
        .map(|pane| pane.terminal_id.clone())
        .expect("w1:p1");
    let terminal2 = topology
        .panes
        .iter()
        .find(|pane| pane.pane_id == "w1:p2")
        .map(|pane| pane.terminal_id.clone())
        .expect("w1:p2");
    let occupant_on = |name: &str, status: &str, session: &str| Occupant {
        name: name.to_owned(),
        kind: "kind-a".to_owned(),
        status: status.to_owned(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: session.to_owned(),
        }),
    };
    for pane in &mut topology.panes {
        pane.agent = Some(match pane.pane_id.as_str() {
            "w1:p1" => occupant_on("gov-r-idle", "idle", "sess-idle"),
            _ => occupant_on("gov-r-max", "working", "sess-max"),
        });
    }
    (topology, terminal1, terminal2)
}

/// The fixture daemon pointing at a nowhere Herdr — the bring-up's
/// snapshot fails and is logged, which is what A1 does with Herdr
/// liveness. The S27d lock/socket legs need no topology.
fn dirs() -> DaemonDirs {
    write_dirs(&Catalog::inert(
        Path::new("/nonexistent/herdr.sock"),
        "http://127.0.0.1:9",
    ))
}

// — S27 ——————————————————————————————————————————————————————————————

/// S27 — `SIGKILL` then restart with elapsed deadlines: the startup
/// pass's deadline sweep fires them before the socket binds — the
/// `idle` deadline settles `no_handoff`, `max_age` settles
/// `unresolved(max_age)` — and the successor's held lock still refuses
/// a third daemon `exit 3` while it answers the probe.
#[tokio::test]
async fn s27_restart_sweeps_elapsed_deadlines_before_bind() {
    let (topology, terminal_idle, terminal_max) = two_agent_topology();
    let world = World::build(topology, |catalog| {
        // An inert tick: only the startup pass can fire the deadlines —
        // no running tick could beat the assert to them.
        catalog.reconcile_secs = 3600;
    });
    let first = TestDaemon::spawn_child(&world.dirs().settings(), None).await;
    first.signal("-9");
    let (status, _stderr) = first.wait().await;
    assert!(!status.success(), "SIGKILL is not a clean exit");

    let inc = socket_incarnation(world.fake().socket_path());
    {
        let mut store = world.store();
        bind_caller(&mut store);
        // An elapsed `idle` deadline: `idle_since` armed means the
        // episode's deadline is the episode-start bound the observation
        // leaves alone — the sweep fires it on bring-up.
        let mut run_idle = run_on(
            "r-idle",
            "l-idle",
            State::Active,
            "w1:p1",
            &terminal_idle,
            Some("sess-idle"),
            &inc,
        );
        run_idle.idle_since = Some(NOW);
        run_idle.idle_deadline = Some(PAST);
        seed_run(&mut store, &run_idle, LaunchPhase::Routed);
        let mut run_max = run_on(
            "r-max",
            "l-max",
            State::Active,
            "w1:p2",
            &terminal_max,
            Some("sess-max"),
            &inc,
        );
        run_max.max_age_deadline = PAST;
        seed_run(&mut store, &run_max, LaunchPhase::Routed);
    }

    let second = TestDaemon::spawn_child(&world.dirs().settings(), None).await;
    // Bound — the probe is a real handshake, so it only answers once the
    // successor is serving; §4.3 steps 5–7 ran before its bind.
    await_for("the successor answers", || {
        probe(&world.dirs().socket_path())
    })
    .await;
    let check = world.store();
    assert_eq!(
        read_run(&check, "r-idle").settlement,
        Some(Settlement::NoHandoff),
        "the elapsed idle deadline fired during startup"
    );
    assert_eq!(
        read_run(&check, "r-max").settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge
        }),
        "the elapsed max_age deadline fired during startup"
    );
    drop(check);
    assert!(probe(&second.socket_path()), "the successor answers");

    let third = TestDaemon::spawn_raw(&world.dirs().settings(), None);
    let (refused, text) = third.wait().await;
    assert_eq!(refused.code(), Some(3), "the held lock refuses: {text}");
    assert!(
        text.contains("socket answers: true"),
        "the refusal reports the live probe: {text}"
    );
    assert!(probe(&second.socket_path()), "the successor keeps serving");
    second.shutdown().await;
}

// — S27b —————————————————————————————————————————————————————————————

/// S27b — a `close` killed at its dispatch commit on a *settled* Run:
/// the restart's global scan reclassifies it `unconfirmed` before the
/// socket binds, `pane.close` never reaches the wire, and the settled
/// Run's row is untouched.
#[tokio::test]
async fn s27b_close_dispatch_commit_kill_is_unconfirmed_before_bind() {
    let (topology, terminal) = agent_topology("gov-r-27b", Some("sess-27b"));
    let world = World::build(topology, |catalog| {
        catalog.reconcile_secs = 3600;
    });
    let inc = socket_incarnation(world.fake().socket_path());
    {
        let mut store = world.store();
        bind_caller(&mut store);
        let run = run_on(
            "r-27b",
            "l-27b",
            State::Active,
            "w1:p1",
            &terminal,
            Some("sess-27b"),
            &inc,
        );
        seed_run(&mut store, &run, LaunchPhase::Routed);
        let stored = read_run(&store, "r-27b");
        store
            .apply(&settle(&stored, Settlement::Cancelled, NOW, &policy()), NOW)
            .expect("settle");
        // The planned close the retired-child path journals — a `Child`
        // target and the honest `op_digest`, so the dispatch gate's F10
        // re-verify passes on the live occupant.
        let close = run_effect(
            "run:r-27b:close",
            EffectKind::Close,
            "r-27b",
            Some(&EffectTarget::Child(
                stored.identity.clone().expect("identity"),
            )),
        );
        store
            .apply(
                &Transition {
                    state_changes: Vec::new(),
                    events: Vec::new(),
                    effects: vec![close],
                },
                NOW,
            )
            .expect("plan close");
    }

    let settings = world.dirs().settings();
    let child = TestDaemon::spawn_child(
        &settings,
        Some(SeamConfig {
            suffix: "close".to_owned(),
            boundary: Boundary::DispatchCommitted,
            action: SeamAction::Abort,
        }),
    )
    .await;
    let (status, stderr) = child.wait().await;
    assert_eq!(
        status.signal(),
        Some(6),
        "the seam's abort is a SIGABRT: {stderr}"
    );
    assert!(
        stderr.contains("seam hit close@dispatch_committed"),
        "{stderr}"
    );

    let second = TestDaemon::spawn_child(&world.dirs().settings(), None).await;
    // Bound — the probe only answers once the successor is serving, and
    // the restart mark runs before its bind.
    await_for("the successor answers", || {
        probe(&world.dirs().socket_path())
    })
    .await;
    let check = world.store();
    assert_eq!(
        effect_at(&check, "run:r-27b:close").state,
        EffectState::Unconfirmed,
        "the killed close is unconfirmed before bind"
    );
    assert_eq!(
        read_run(&check, "r-27b").settlement,
        Some(Settlement::Cancelled),
        "the settled run is untouched"
    );
    drop(check);
    assert!(
        !saw_wire(world.fake(), "pane.close"),
        "the committed-but-never-wired close never runs"
    );
    second.shutdown().await;
}

// — S27d —————————————————————————————————————————————————————————————

/// S27d — `SIGKILL` leaves `governor.sock` behind: the successor takes
/// the lock, unlinks the stale socket and binds, a third daemon exits 3
/// against the held lock while the successor keeps serving.
#[tokio::test]
async fn s27d_successor_acquires_lock_removes_stale_socket() {
    let dirs = dirs();
    let sock = dirs.socket_path();

    let first = TestDaemon::spawn_child(&dirs.settings(), None).await;
    await_for("the first answers", || probe(&sock)).await;
    first.signal("-9");
    let (status, _stderr) = first.wait().await;
    assert!(!status.success(), "SIGKILL is not a clean exit");
    assert!(sock.exists(), "the killed daemon left a stale socket");

    let mut second = TestDaemon::spawn_child(&dirs.settings(), None).await;
    // `spawn_child`'s `exists()` wait can fire on the stale file — the
    // probe is the honest bind (the stale file has no listener).
    await_for("the successor answers", || probe(&sock)).await;
    assert!(
        probe(&sock),
        "the successor unlinked the stale socket and answers"
    );

    let third = TestDaemon::spawn_raw(&dirs.settings(), None);
    let (refused, text) = third.wait().await;
    assert_eq!(
        refused.code(),
        Some(3),
        "a second daemon against the held lock exits 3: {text}"
    );
    assert!(
        text.contains("socket answers: true"),
        "the refusal reports the live probe: {text}"
    );
    assert!(second.alive(), "the successor is still running");
    assert!(probe(&sock), "the successor still answers");
    second.shutdown().await;
}

// — F28's pre-bind global scan ———————————————————————————————————————

/// f28's seed: one stranded `dispatching` row per subject shape — a
/// `nudge` on an `active` run, a `close` on a `settled` one, the
/// `evaluate` ask on an `evaluating` launch, and `start:0` on a
/// `starting` run with its topology already journaled.
fn seed_stranded(store: &mut Store, terminal: &str, inc: &str) {
    bind_caller(store);

    // An `active` run mid-`nudge` — the run-bound prompt row.
    let run_act = run_on(
        "r-act",
        "l-act",
        State::Active,
        "w1:p1",
        terminal,
        Some("sess-act"),
        inc,
    );
    seed_run(store, &run_act, LaunchPhase::Routed);
    seed_dispatching(
        store,
        &run_effect(
            "run:r-act:nudge:1",
            EffectKind::Prompt,
            "r-act",
            Some(&EffectTarget::Child(
                run_act.identity.clone().expect("identity"),
            )),
        ),
    );

    // A `settled` run's stranded `close` — the f8 global-scan row.
    let run_set = run_on(
        "r-set",
        "l-set",
        State::Active,
        "w1:p1",
        terminal,
        Some("sess-set"),
        inc,
    );
    seed_run(store, &run_set, LaunchPhase::Routed);
    let stored = read_run(store, "r-set");
    store
        .apply(&settle(&stored, Settlement::Cancelled, NOW, &policy()), NOW)
        .expect("settle");
    seed_dispatching(
        store,
        &run_effect(
            "run:r-set:close",
            EffectKind::Close,
            "r-set",
            Some(&EffectTarget::Child(
                stored.identity.clone().expect("identity"),
            )),
        ),
    );

    // An `evaluating` launch mid-`evaluate` — the launch-bound ask.
    store
        .apply(
            &changes(vec![StateChange::RecordLaunch(launch_row(
                "l-eval",
                LaunchPhase::Evaluating,
            ))]),
            NOW,
        )
        .expect("record launch");
    seed_dispatching(
        store,
        &launch_effect("launch:l-eval:evaluate", EffectKind::JevEvaluate, "l-eval"),
    );

    // A `starting` run with its topology journaled and `start:0`
    // in flight — the launch-failed convergence row.
    let run_st = run_row("r-st", "l-st", State::Starting);
    seed_run(store, &run_st, LaunchPhase::Launching);
    seed_acknowledged(
        store,
        &run_effect(
            "run:r-st:tab",
            EffectKind::TabCreate,
            "r-st",
            Some(&EffectTarget::CallerContext(PaneId("w1:p1".into()))),
        ),
        EffectReceipt::TabCreated {
            tab: TabId("w1:t9".into()),
            pane: PaneId("w1:p9".into()),
        },
    );
    seed_dispatching(
        store,
        &run_effect(
            "run:r-st:start:0",
            EffectKind::AgentStart,
            "r-st",
            Some(&EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
    );
}

/// F28 — restart's global `dispatching` scan marks every stranded row
/// `unconfirmed` before the socket binds, across every subject shape a
/// kill leaves behind: a `nudge` on an `active` run, a `close` on a
/// `settled` one, an `evaluate` on an `evaluating` launch (finished
/// `abstained`), and `start:0` on a `starting` run (converged
/// `failed/unknown`, then settled `launch_failed`). Nothing dispatches —
/// the scan is a journal rewrite, not a wire pass.
#[tokio::test]
async fn f28_dispatching_becomes_unconfirmed_before_bind() {
    let (topology, terminal) = agent_topology("gov-r-act", Some("sess-act"));
    let mut world = World::build(topology, |catalog| {
        catalog.reconcile_secs = 3600;
    });
    let inc = socket_incarnation(world.fake().socket_path());
    seed_stranded(&mut world.store(), &terminal, &inc);

    world.start().await;

    // Bound — the startup scan already ran.
    let store = world.store();
    assert_eq!(
        effect_at(&store, "run:r-act:nudge:1").state,
        EffectState::Unconfirmed,
        "the active run's dispatching prompt"
    );
    assert_eq!(
        effect_at(&store, "run:r-set:close").state,
        EffectState::Unconfirmed,
        "the settled run's dispatching close"
    );
    let eval = effect_at(&store, "launch:l-eval:evaluate");
    assert_eq!(eval.state, EffectState::Failed);
    assert_eq!(eval.certainty, Some(EffectCertainty::Unknown));
    let launch_eval = launch_at(&store, "k-l-eval");
    assert_eq!(
        launch_eval.outcome,
        Some(LaunchOutcome::Abstained {
            reason: AbstainReason::InterruptedBeforeDecision
        }),
        "the interrupted evaluation abstains before bind"
    );
    assert_eq!(
        effect_at(&store, "run:r-st:start:0").state,
        EffectState::Unconfirmed,
        "the starting run's dispatching start"
    );
    let launch = launch_at(&store, "k-l-st");
    let Some(LaunchOutcome::Failed {
        certainty,
        run: failed_run,
        created_topology,
    }) = &launch.outcome
    else {
        panic!("the stranded start converges failed: {:?}", launch.outcome);
    };
    assert_eq!(*certainty, EffectCertainty::Unknown);
    assert_eq!(failed_run.as_ref().map(|id| id.0.as_str()), Some("r-st"));
    assert_eq!(
        created_topology.tab.as_ref().map(|t| t.0.as_str()),
        Some("w1:t9")
    );
    assert_eq!(created_topology.panes.len(), 1);
    assert_eq!(
        read_run(&store, "r-st").settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed
        }),
        "no agent answers for the run — it settles launch_failed"
    );
    assert_eq!(
        read_run(&store, "r-set").settlement,
        Some(Settlement::Cancelled),
        "the settled run's settlement is untouched"
    );
    drop(store);
    assert!(
        !saw_wire(world.fake(), "pane.close")
            && !saw_wire(world.fake(), "agent.start")
            && !saw_wire(world.fake(), "agent.prompt")
            && !saw_wire(world.fake(), "tab.create")
            && !saw_wire(world.fake(), "pane.split")
            && world.jev().requests().is_empty(),
        "the scan marks journals; it never dispatches"
    );
    world.shutdown().await;
}
