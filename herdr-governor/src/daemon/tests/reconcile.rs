//! `reconcile` — the §4.7 gates at unit level: the identity-less absence
//! rule's attempted-and-terminated table, the sessionless
//! foreign-incarnation settlement through `observe_run`, invalid
//! snapshots never settling, and `HerdrHealth` freshness.

use std::collections::BTreeMap;
use std::io;

use governor_core::identity::{
    AgentKind, AgentName, ChildIdentity, ChildStatus, EffectKey, HerdrIncarnation, NativeSession,
    PaneId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    EffectCertainty, EffectKind, EffectResolution, EffectState, EffectWrite, Run, Settlement,
    State, StateChange, UnresolvedReason,
};

use crate::adapters::herdr::{
    AgentInfo, AgentStatus, ConnEpoch, HerdrError, Observed, SessionSnapshot,
};
use crate::daemon::reconcile::{self, HealthState, HerdrHealth, SnapshotView};
use crate::store::Store;

use super::coordinator::{
    NOW, bind_caller, changes, coordinator_with, effect, plan, policy, run_row, run_row_on,
    seed_dispatching, seed_run, store_in,
};

// — Fixture builders —————————————————————————————————————————————————

/// One core agent row — `reconcile`'s private `CoreRow` tuple spelled
/// out; the name is what the identity-less gate matches on.
type Row = (
    PaneId,
    TerminalId,
    Option<AgentKind>,
    Option<AgentName>,
    Option<NativeSession>,
    Option<ChildStatus>,
);

fn agent_row(name: Option<&str>) -> Row {
    (
        PaneId("w1:p1".into()),
        TerminalId("term_1".into()),
        Some(AgentKind("kind-a".into())),
        name.map(|n| AgentName(n.to_owned())),
        Some(NativeSession("sess-1".into())),
        Some(ChildStatus::Working),
    )
}

fn observed(agents: Vec<AgentInfo>, inode: u64) -> Observed<SessionSnapshot> {
    Observed {
        epoch: ConnEpoch {
            seq: 1,
            socket_inode: inode,
            socket_mtime_secs: 1_700_000_000,
            socket_mtime_nsecs: 42,
        },
        value: SessionSnapshot {
            version: "t".into(),
            protocol: 22,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
        },
    }
}

fn agent_info(pane_id: &str, name: Option<&str>) -> AgentInfo {
    AgentInfo {
        pane_id: pane_id.to_owned(),
        tab_id: "w1:t1".into(),
        workspace_id: "w1".into(),
        terminal_id: format!("term-{pane_id}"),
        revision: 0,
        focused: false,
        agent_status: AgentStatus::Working,
        agent: Some("kind-a".into()),
        agent_session: None,
        cwd: None,
        display_agent: None,
        foreground_cwd: None,
        label: None,
        scroll: None,
        state_labels: BTreeMap::default(),
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
        name: name.map(str::to_owned),
        interactive_ready: None,
        launch_pending: None,
        screen_detection_skipped: None,
        state_change_seq: None,
    }
}

fn view(agents: Vec<AgentInfo>, inode: u64) -> SnapshotView {
    reconcile::view_of(&observed(agents, inode))
}

/// An `active` Run with a captured identity — `session` chooses the
/// native session (None = sessionless, the F28 shape); `incarnation`
/// spells the stored `herdr_incarnation`.
fn identified_run(id: &str, session: Option<&str>, incarnation: &str) -> Run {
    let mut run = run_row_on(id, &format!("l-{id}"));
    run.state = State::Active;
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(incarnation.to_owned()),
        terminal_id: TerminalId("term-w1:p1".into()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName(format!("gov-{id}")),
        native_session: session.map(|s| NativeSession(s.to_owned())),
        pane_id: PaneId("w1:p1".into()),
    });
    run
}

/// Journal `key`'s `agent_start` leg `failed` (plan → dispatch → result),
/// the terminated-leg spelling the gate requires.
fn seed_failed_leg(store: &mut Store, key: &str, run: &str) {
    store
        .apply(
            &plan(effect(key, EffectKind::AgentStart, Some(run), None)),
            NOW,
        )
        .expect("plan leg");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey(key.into()),
            })]),
            NOW,
        )
        .expect("dispatch leg");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Result {
                key: EffectKey(key.into()),
                resolution: EffectResolution::Failed {
                    certainty: EffectCertainty::Absent,
                    cause: None,
                },
            })]),
            NOW,
        )
        .expect("fail leg");
}

// — The identity-less absence rule (§4.7's revised F3/F21 gate) ————————

/// A `reserved` Run with no journal rows and no `agent_name` row on the
/// snapshot is *not* absent — nothing was attempted yet; launch
/// convergence owns it. And a `starting` Run whose only start leg is
/// still `planned` holds the absence back the same way.
#[test]
fn absent_by_name_only_without_planned_start() {
    let reserved = run_row("r-res");
    assert!(!reconcile::identity_less_absent(
        &reserved,
        &[],
        &[agent_row(Some("gov-other"))],
        true,
    ));

    let mut starting = run_row("r-start");
    starting.state = State::Starting;
    let planned = effect(
        "run:r-start:start",
        EffectKind::AgentStart,
        Some("r-start"),
        None,
    );
    assert!(!reconcile::identity_less_absent(
        &starting,
        &[planned],
        &[agent_row(Some("gov-other"))],
        true,
    ));
}

/// The full attempted-and-terminated table: `Absent` derives only when
/// every topology/start leg is terminal (`acknowledged`/`failed`/
/// `unconfirmed`) and at least one exists. In-flight legs, a present
/// name, an invalid snapshot and a leg-free journal all hold it back.
#[test]
fn identity_less_absence_requires_terminated_launch_leg() {
    let mut run = run_row("r-1");
    run.state = State::Starting;
    let name_absent = vec![agent_row(Some("gov-other"))];

    for (i, state) in [
        EffectState::Acknowledged,
        EffectState::Failed,
        EffectState::Unconfirmed,
    ]
    .into_iter()
    .enumerate()
    {
        let mut leg = effect(
            &format!("run:r-1:start:{i}"),
            EffectKind::AgentStart,
            Some("r-1"),
            None,
        );
        leg.state = state;
        assert!(
            reconcile::identity_less_absent(&run, std::slice::from_ref(&leg), &name_absent, true),
            "terminal {state:?} derives absent"
        );
    }
    for (i, state) in [EffectState::Planned, EffectState::Dispatching]
        .into_iter()
        .enumerate()
    {
        let mut leg = effect(
            &format!("run:r-1:live:{i}"),
            EffectKind::AgentStart,
            Some("r-1"),
            None,
        );
        leg.state = state;
        assert!(
            !reconcile::identity_less_absent(&run, std::slice::from_ref(&leg), &name_absent, true),
            "in-flight {state:?} holds"
        );
    }

    // A failed leg plus a planned next-candidate start: still in flight.
    let mut failed = effect("run:r-1:a", EffectKind::AgentStart, Some("r-1"), None);
    failed.state = EffectState::Failed;
    let planned = effect("run:r-1:b", EffectKind::AgentStart, Some("r-1"), None);
    assert!(!reconcile::identity_less_absent(
        &run,
        &[failed, planned],
        &name_absent,
        true,
    ));

    // The name present, an invalid snapshot, and an empty journal each
    // veto on their own.
    let mut terminal = effect("run:r-1:term", EffectKind::AgentStart, Some("r-1"), None);
    terminal.state = EffectState::Failed;
    let journal = std::slice::from_ref(&terminal);
    assert!(!reconcile::identity_less_absent(
        &run,
        journal,
        &[agent_row(Some("gov-r-1"))],
        true,
    ));
    assert!(!reconcile::identity_less_absent(
        &run,
        journal,
        &name_absent,
        false
    ));
    assert!(!reconcile::identity_less_absent(
        &run,
        &[],
        &name_absent,
        true
    ));
}

// — `observe_run`: the sessionless F28 settlement ——————————————————————

/// A sessionless identity under a *valid foreign* incarnation settles
/// `unresolved(identity_unprovable)` — the core's `classify` returns
/// `Invalid` there by contract, so the daemon settles it itself. The
/// same snapshot under the stored (same) incarnation does not.
#[test]
fn sessionless_identity_settles_only_on_valid_foreign_incarnation() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);

    // Foreign incarnation: the stored identity was minted under a dead
    // server epoch; the fresh snapshot is valid and carries nobody.
    let foreign = identified_run("r-foreign", None, "99:1.1");
    seed_run(&mut store, &foreign);
    let snapshot = view(Vec::new(), 7);
    reconcile::observe_run(&mut store, &policy(), NOW, &foreign.id, &snapshot).expect("observe");
    let settled = store.run(&foreign.id).expect("read").expect("row");
    assert_eq!(
        settled.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::IdentityUnprovable,
        }),
        "the sessionless identity settles identity_unprovable"
    );

    // Same incarnation: the snapshot's epoch mints the identity's
    // spelling — `classify` yields `absent`, which on `active` is a
    // pane-lost settlement path, NOT identity_unprovable.
    let same = identified_run("r-same", None, "7:1700000000.000000042");
    seed_run(&mut store, &same);
    reconcile::observe_run(&mut store, &policy(), NOW, &same.id, &snapshot).expect("observe");
    let same_after = store.run(&same.id).expect("read").expect("row");
    assert_ne!(
        same_after.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::IdentityUnprovable,
        }),
        "same-incarnation absence is pane-loss, not unprovable"
    );
}

/// An invalid snapshot (duplicated pane locators make the whole view
/// untrustworthy) never settles: the sessionless gate demands `valid`,
/// and `classify` itself returns `Invalid` → the transition is a no-op.
#[test]
fn f3_invalid_snapshot_never_settles() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);

    let sessionless = identified_run("r-1", None, "99:1.1");
    seed_run(&mut store, &sessionless);
    // Two rows on the same `pane_id` — F3's untrustworthy snapshot.
    let snapshot = view(
        vec![
            agent_info("w1:p1", Some("a")),
            agent_info("w1:p1", Some("b")),
        ],
        7,
    );
    reconcile::observe_run(&mut store, &policy(), NOW, &sessionless.id, &snapshot)
        .expect("observe");
    let after = store.run(&sessionless.id).expect("read").expect("row");
    assert!(
        after.settlement.is_none(),
        "an invalid snapshot settles nothing: {:?}",
        after.settlement
    );
    assert_eq!(after.state, State::Active, "the run is untouched");

    // And the identity-less gate holds on the same invalid view: a
    // starting run with a terminated leg still does not go absent.
    let mut starting = run_row_on("r-2", "l-2");
    starting.state = State::Starting;
    seed_run(&mut store, &starting);
    seed_failed_leg(&mut store, "run:r-2:start", "r-2");
    reconcile::observe_run(&mut store, &policy(), NOW, &starting.id, &snapshot).expect("observe");
    let starting_after = store.run(&starting.id).expect("read").expect("row");
    assert!(
        starting_after.settlement.is_none() && starting_after.state == State::Starting,
        "invalid view holds the identity-less absence"
    );
}

// — `HerdrHealth` freshness ————————————————————————————————————————————

/// The health record's verdicts: `Unknown` before any read, `Fresh` +
/// freshness + the minted incarnation on a good snapshot, `Gone` on a
/// connect failure (before *and* after a good read), `Degraded` on a
/// non-connect failure only once a good read exists.
#[test]
fn health_freshness_reports_state_incarnation_and_age() {
    let mut health = HerdrHealth::default();
    assert_eq!(health.state(), HealthState::Unknown);
    assert_eq!(health.fresh_secs_ago(NOW), None);
    assert_eq!(health.incarnation(), None);

    let gone: Result<Observed<SessionSnapshot>, HerdrError> =
        Err(HerdrError::Connect(io::Error::other("refused")));
    health.record(&gone, NOW);
    assert_eq!(health.state(), HealthState::Gone, "connect failure is gone");

    let ok: Result<Observed<SessionSnapshot>, HerdrError> = Ok(observed(Vec::new(), 7));
    health.record(&ok, NOW);
    assert_eq!(health.state(), HealthState::Fresh);
    assert_eq!(
        health.incarnation(),
        Some(&HerdrIncarnation("7:1700000000.000000042".into())),
        "the epoch's minted incarnation is reported"
    );
    assert_eq!(health.fresh_secs_ago(NOW), Some(0));
    assert_eq!(health.fresh_secs_ago(Timestamp(NOW.0 + 61_000)), Some(61));

    let degraded: Result<Observed<SessionSnapshot>, HerdrError> =
        Err(HerdrError::Io(io::Error::other("reset")));
    health.record(&degraded, Timestamp(NOW.0 + 62_000));
    assert_eq!(
        health.state(),
        HealthState::Degraded,
        "a non-connect failure after a good read degrades"
    );
    assert_eq!(
        health.fresh_secs_ago(Timestamp(NOW.0 + 62_000)),
        Some(62),
        "freshness still measures the last good read"
    );

    health.record(&gone, Timestamp(NOW.0 + 63_000));
    assert_eq!(
        health.state(),
        HealthState::Gone,
        "a connect failure is always gone"
    );
}

/// The coordinator's `health()` accessor reflects the startup pass's
/// snapshot — `gone` on a connect failure, `fresh` on a good read.
#[test]
fn coordinator_health_reflects_the_startup_snapshot() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = store_in(tmp.path());
    let mut coordinator = coordinator_with(store, tmp.path());

    let gone: Result<Observed<SessionSnapshot>, HerdrError> =
        Err(HerdrError::Connect(io::Error::other("refused")));
    coordinator.startup_pass(&gone).expect("pass");
    assert_eq!(coordinator.health().state(), HealthState::Gone);

    coordinator
        .startup_pass(&Ok(observed(Vec::new(), 11)))
        .expect("pass");
    assert_eq!(coordinator.health().state(), HealthState::Fresh);
    assert_eq!(
        coordinator.health().incarnation(),
        Some(&HerdrIncarnation("11:1700000000.000000042".into())),
    );
}

// — The startup ordering's restart-marking interplay (B3-owned rows) ———

/// `startup_pass` after `mark_restart`: a `starting` Run whose start leg
/// was `dispatching` at kill gets the leg `unconfirmed` (the global
/// scan), which the identity-less gate then reads as a terminated leg —
/// the snapshot shows no such agent, so the run settles `launch_failed`.
#[test]
fn identity_less_settles_after_restart_marks_the_leg_unconfirmed() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);

    let mut run = run_row("r-1");
    run.state = State::Starting;
    seed_dispatching(&mut store, &run, "run:r-1:start", EffectKind::AgentStart);

    let mut coordinator = coordinator_with(store, tmp.path());
    coordinator.mark_restart(NOW).expect("mark");
    coordinator
        .startup_pass(&Ok(observed(Vec::new(), 3)))
        .expect("pass");

    // Read the outcome on a second connection (the coordinator owns
    // `store` now).
    let after = store_in(tmp.path());
    let settled = after.run(&RunId("r-1".into())).expect("read").expect("row");
    assert_eq!(
        settled.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed,
        }),
        "the restarted starting run settles launch_failed"
    );
}
