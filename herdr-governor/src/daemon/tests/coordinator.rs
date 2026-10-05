//! `coordinator` — the bounded apply-retry and the §4.3 step-5 restart
//! marking (including the [r2] settled-Run case) over real tempdir
//! stores.

use std::path::{Path, PathBuf};
use std::time::Duration;

use governor_core::config::{Catalog, Config, ConfigVersion, Policy, Tier};
use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, Digest, EffectId, EffectKey, IdempotencyKey, LaunchId,
    NativeSession, PaneId, ProjectRoot, RelayInstanceId, RunId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectState, EffectWrite, Run, RunUpdate, State, StateChange, Transition,
    settle,
};
use governor_core::task::{Launch, LaunchPhase, Task};

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::daemon::clock::Clock;
use crate::daemon::coordinator::apply::{ApplyOutcome, apply_with_retry};
use crate::daemon::coordinator::{Coordinator, CoordinatorArgs};
use crate::daemon::paths::Paths;
use crate::store::Store;

pub(super) const NOW: Timestamp = Timestamp(1_790_812_800_000);

// — Fixture builders (constructed inputs, no I/O beyond tempfiles) ———————

pub(super) fn caller(n: u8) -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession(format!("sess-{n}")),
    }
}

/// Rows are rejected while the caller is unbound — bind `caller(n)` first.
pub(super) fn binding(n: u8) -> StateChange {
    StateChange::BindCaller(CallerBinding {
        caller: caller(n),
        relay_instance: RelayInstanceId(format!("relay-{n}")),
        pane_at_bind: PaneId("pane-1".into()),
    })
}

pub(super) fn run_row(id: &str) -> Run {
    run_row_on(id, "l-1")
}

pub(super) fn run_row_on(id: &str, launch: &str) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(launch.into()),
        owner: caller(1),
        owner_generation: 0,
        version: 0,
        state: State::Reserved,
        prompt_certainty: None,
        child_name: format!("gov-{id}"),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/p".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(NOW.0 + 86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

pub(super) fn effect(
    key: &str,
    kind: EffectKind,
    run: Option<&str>,
    launch: Option<&str>,
) -> Effect {
    Effect {
        id: EffectId(format!("eff:{key}")),
        key: EffectKey(key.into()),
        kind,
        subject_launch: launch.map(|l| LaunchId(l.into())),
        subject_run: run.map(|r| RunId(r.into())),
        target: None,
        payload_digest: Some(Digest([0x5a; 32])),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

pub(super) fn launch_row(id: &str, phase: LaunchPhase) -> Launch {
    Launch {
        id: LaunchId(id.into()),
        caller: caller(1),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(id.into()),
        digest_version: 1,
        task_digest: Digest([0xab; 32]),
        task: Task {
            objective: "o".into(),
            scope: "s".into(),
            done_when: vec!["d".into()],
            constraints: vec![],
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
            retention: None,
        },
        phase,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

pub(super) fn changes(changes: Vec<StateChange>) -> Transition {
    Transition {
        state_changes: changes,
        events: Vec::new(),
        effects: Vec::new(),
    }
}

pub(super) fn plan(effect: Effect) -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: vec![effect],
    }
}

pub(super) fn policy() -> Policy {
    Policy {
        tiers: vec![Tier("standard".into())],
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.6,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

pub(super) fn daemon_settings() -> DaemonSettings {
    DaemonSettings {
        herdr_socket: PathBuf::from("/nonexistent/herdr.sock"),
        jev_base_url: "http://127.0.0.1:9".into(),
        jev_model: "jev-test".into(),
        jev_timeout: Duration::from_secs(20),
        herdr_op_timeout: Duration::from_millis(200),
        agent_start_timeout: Duration::from_secs(30),
        reconcile: Duration::from_hours(1),
        review_interval: Duration::from_mins(5),
        launch_wait: Duration::from_mins(1),
        shutdown_grace: Duration::from_secs(10),
        retire_enabled: true,
        retire_grace: Duration::from_mins(15),
        transcript_data_dirs: None,
        transcript_project_dirs: None,
        devin_log_dir: None,
    }
}

pub(super) fn loaded(daemon: DaemonSettings) -> LoadedConfig {
    LoadedConfig {
        config: Config {
            version: ConfigVersion("v".into()),
            catalog: Catalog {
                operating_points: Vec::new(),
            },
            policy: policy(),
        },
        version: ConfigVersion("v".into()),
        daemon: Some(daemon),
    }
}

pub(super) fn coordinator_with(store: Store, dir: &Path) -> Coordinator {
    Coordinator::new(
        store,
        CoordinatorArgs {
            loaded: loaded(daemon_settings()),
            daemon: daemon_settings(),
            catalog_path: PathBuf::from("/nonexistent/catalog.toml"),
            clock: Clock::new(),
            seam: None,
            paths: Paths::create(&dir.join("state")).expect("paths"),
        },
    )
}

pub(super) fn store_in(dir: &Path) -> Store {
    Store::open(&dir.join("governor.db")).expect("store opens")
}

// — `apply_with_retry` —————————————————————————————————————————————————

/// §4.2 — a CAS `Conflict` recomputes against a fresh read up to three
/// attempts then drops; a recompute that converges on the second attempt
/// applies. The bound is exactly three calls, never four.
#[test]
fn apply_retry_bounded_to_three() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    seed_run(&mut store, &run_row("r-1"));

    // Always-stale recompute → Conflict{RunVersion} every attempt.
    let mut calls = 0_usize;
    let outcome = apply_with_retry(&mut store, NOW, |_st| {
        calls = calls.saturating_add(1);
        let mut record = run_row("r-1");
        record.version = 43;
        changes(vec![StateChange::UpdateRun(RunUpdate {
            expected_version: 42,
            record,
        })])
    })
    .expect("conflicts drop, not error");
    assert_eq!(
        outcome,
        ApplyOutcome::Dropped { attempts: 3 },
        "three conflicts exhaust the bound"
    );
    assert_eq!(calls, 3, "recompute ran exactly once per attempt");

    // Stale first, fresh-read second → applies on attempt 2.
    let mut tries = 0_usize;
    let landed = apply_with_retry(&mut store, NOW, |st| {
        tries = tries.saturating_add(1);
        let current = st.run(&RunId("r-1".into())).expect("read").expect("row");
        let expected = if tries == 1 { 42 } else { current.version };
        let mut record = current.clone();
        record.version = expected.saturating_add(1);
        changes(vec![StateChange::UpdateRun(RunUpdate {
            expected_version: expected,
            record,
        })])
    })
    .expect("converged apply");
    assert_eq!(
        landed,
        ApplyOutcome::Applied { attempts: 2 },
        "the second attempt landed"
    );
}

// — `Coordinator::mark_restart` (§4.3 step 5) ———————————————————————————

/// Bind `caller(1)` — once per store; a second `BindCaller` conflicts.
pub(super) fn bind_caller(store: &mut Store) {
    store
        .apply(&changes(vec![binding(1)]), NOW)
        .expect("bind caller");
}

/// The canonical seed (store_apply/support.rs's order): record the launch
/// `evaluating`, then `routed` + `ReserveRun` — the `runs.launch_id` FK
/// demands the row exists first. Caller must already be bound.
pub(super) fn seed_run(store: &mut Store, run: &Run) {
    let launch = run.launch.0.clone();
    store
        .apply(
            &changes(vec![StateChange::RecordLaunch(launch_row(
                &launch,
                LaunchPhase::Evaluating,
            ))]),
            NOW,
        )
        .expect("record evaluating");
    store
        .apply(
            &changes(vec![
                StateChange::RecordLaunch(launch_row(&launch, LaunchPhase::Routed)),
                StateChange::ReserveRun(run.clone()),
            ]),
            NOW,
        )
        .expect("route + reserve");
}

/// Seed `store` with `run`, plus `key`'s effect in `dispatching`.
pub(super) fn seed_dispatching(store: &mut Store, run: &Run, key: &str, kind: EffectKind) {
    seed_run(store, run);
    store
        .apply(&plan(effect(key, kind, Some(&run.id.0), None)), NOW)
        .expect("plan");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey(key.into()),
            })]),
            NOW,
        )
        .expect("dispatch");
}

/// §4.3 step 5 [r2] — the restart scan is global: a `dispatching` effect
/// owned by a *settled* Run is reclassified `unconfirmed` just like one on
/// a live Run. This is the `f8_restart_marks_dispatching_close_on_`
/// `settled_run` behavior B3's e2e asserts, at the mark's level.
#[test]
fn restart_marks_dispatching_unconfirmed_including_settled_runs() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());

    let live = run_row("r-live");
    bind_caller(&mut store);
    seed_dispatching(&mut store, &live, "run:r-live:close", EffectKind::Close);

    // A settled Run whose `close` is still dispatching.
    let mut settled = run_row_on("r-settled", "l-2");
    seed_run(&mut store, &settled);
    let settling = settle(
        &settled,
        governor_core::lifecycle::Settlement::Cancelled,
        NOW,
        &policy(),
    );
    store.apply(&settling, NOW).expect("settle");
    settled = store.run(&settled.id).expect("read").expect("row");
    assert!(settled.settlement.is_some(), "the run is settled");
    store
        .apply(
            &plan(effect(
                "run:r-settled:close",
                EffectKind::Close,
                Some(&settled.id.0),
                None,
            )),
            NOW,
        )
        .expect("plan close");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey("run:r-settled:close".into()),
            })]),
            NOW,
        )
        .expect("dispatch close");

    let mut coordinator = coordinator_with(store, tmp.path());
    let marks = coordinator.mark_restart(NOW).expect("mark");
    assert_eq!(
        (marks.effects, marks.runs, marks.evals),
        (2, 2, 0),
        "both dispatching effects marked over both runs"
    );

    // Read through a second connection on the same db: both rows are
    // `unconfirmed`.
    let store_after = store_in(tmp.path());
    for key in ["run:r-live:close", "run:r-settled:close"] {
        let run_id = RunId(if key.contains("r-live") {
            "r-live".into()
        } else {
            "r-settled".into()
        });
        let journal = store_after.journal(&run_id).expect("journal");
        let row = journal.iter().find(|e| e.key.0 == key).expect("effect row");
        assert_eq!(
            row.state,
            EffectState::Unconfirmed,
            "{key} reclassified unconfirmed"
        );
    }
}

/// A `dispatching` `jev_evaluate` on an `evaluating` Launch abstains it
/// `interrupted_before_decision`; the stranded row's certainty is
/// `unknown` (OQ-13).
#[test]
fn restart_abstains_stranded_eval_with_unknown_certainty() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    store
        .apply(&changes(vec![binding(1)]), NOW)
        .expect("bind caller");
    store
        .apply(
            &changes(vec![StateChange::RecordLaunch(launch_row(
                "l-1",
                LaunchPhase::Evaluating,
            ))]),
            NOW,
        )
        .expect("launch");
    store
        .apply(
            &plan(effect(
                "launch:l-1:evaluate",
                EffectKind::JevEvaluate,
                None,
                Some("l-1"),
            )),
            NOW,
        )
        .expect("plan eval");
    store
        .apply(
            &changes(vec![StateChange::WriteEffect(EffectWrite::Dispatch {
                key: EffectKey("launch:l-1:evaluate".into()),
            })]),
            NOW,
        )
        .expect("dispatch eval");

    let mut coordinator = coordinator_with(store, tmp.path());
    let marks = coordinator.mark_restart(NOW).expect("mark");
    assert_eq!(
        (marks.effects, marks.runs, marks.evals),
        (1, 0, 1),
        "the stranded eval abstained"
    );

    let store_after = store_in(tmp.path());
    let launch = store_after
        .launch(&LaunchId("l-1".into()))
        .expect("read")
        .expect("launch row");
    assert_eq!(
        launch.phase,
        LaunchPhase::Done,
        "the evaluating launch is finished"
    );
    assert!(
        matches!(
            launch.outcome,
            Some(governor_core::task::LaunchOutcome::Abstained {
                reason: governor_core::task::AbstainReason::InterruptedBeforeDecision
            })
        ),
        "abstained interrupted_before_decision: {:?}",
        launch.outcome
    );
}
