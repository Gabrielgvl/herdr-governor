//! `coordinator::tick` — the `Msg::Tick`/`Msg::Observation` arms (§4.7):
//! the snapshot's health record, the steps-0–2 `reconcile::pass`, the
//! steps-5–6 `delivery::pass`, the per-Run observation the subscription
//! feed triggers, and the child-pane set pushed to the step-8
//! maintainer.

use std::collections::BTreeSet;

use tokio::sync::watch;

use governor_core::identity::{ChildStatus, PaneId, RunId};

use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::daemon::reconcile::{self, SnapshotView};
use crate::daemon::{DaemonError, delivery, log, recovery};

use super::Coordinator;

impl Coordinator {
    /// `Msg::Tick` — record health, run the §4.7 steps-0–2 pass then the
    /// steps-5–6 delivery pass on the same fresh view (a hard error is
    /// logged and dropped; the next tick resumes), answer the waiters of
    /// Launches it finished, then feed the step-8 maintainer the current
    /// pane set.
    pub(super) fn on_tick(&mut self, snapshot: &Result<Observed<SessionSnapshot>, HerdrError>) {
        let now = self.clock.now();
        let panes = snapshot
            .as_ref()
            .map_or(0, |observed| observed.value.panes.len());
        log::tick(snapshot.is_ok(), panes);
        self.health.record(snapshot, now);
        if let Ok(observed) = snapshot {
            self.latest_snapshot = Some(observed.clone());
        }
        // §4.7 step 0 — launch convergence, before any observation has
        // settled the Runs its rows depend on (F21).
        self.converge_launches(snapshot.as_ref().ok());
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        if let Err(error) = reconcile::pass(store, (policy, &self.paths), now, snapshot) {
            log::apply_dropped(1, kind_of(&error));
        }
        let view = snapshot.as_ref().ok().map(reconcile::view_of);
        // §4.7 step 4 — evidence gathers (async) and the acceptance retry.
        self.supervise(view.as_ref());
        if let Err(error) = delivery::pass(
            &mut self.store,
            &self.loaded.config.catalog,
            view.as_ref(),
            &mut self.followup_absent_since,
            &mut self.last_hint_at,
            &self.paths,
            now,
        ) {
            log::apply_dropped(1, kind_of(&error));
        }
        // §4.10 — the recovery sweep on the same fresh snapshot: expire
        // elapsed obligations (skipping any with an in-flight
        // successor), admit the deterministic successor for a
        // `dispatch_ready` one — still `pending` until routing moves it.
        if let Err(error) = recovery::pass(&mut self.store, now, snapshot.as_ref().ok()) {
            log::apply_dropped(1, kind_of(&error));
        }
        self.drain_done_waiters();
        self.push_subscription_specs();
    }

    /// `Msg::Observation` — §4.7 step 1 for the one Run the event's pane
    /// resolves to. The subscription task's fresh snapshot rides the
    /// message; a `Connect`-class failure only updates health (the
    /// "gone" report of step 8's re-arm path).
    pub(super) fn on_observation(
        &mut self,
        pane_id: &PaneId,
        _status: Option<ChildStatus>,
        snapshot: &Result<Observed<SessionSnapshot>, HerdrError>,
    ) {
        let now = self.clock.now();
        self.health.record(snapshot, now);
        let Ok(observed) = snapshot else {
            return;
        };
        self.latest_snapshot = Some(observed.clone());
        let view = reconcile::view_of(observed);
        let Some(run_id) = self.run_for_pane(pane_id, &view) else {
            return;
        };
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        if let Err(error) =
            reconcile::observe_run(store, (policy, &self.paths), now, &run_id, &view)
        {
            log::apply_dropped(1, kind_of(&error));
        }
        self.push_subscription_specs();
    }

    /// §4.3 steps 6–7 — the same steps-0–2 pass against the one startup
    /// snapshot, strictly before the bind (H#5); a hard error refuses
    /// startup exactly like `mark_restart`'s does.
    pub(in crate::daemon) fn startup_pass(
        &mut self,
        snapshot: &Result<Observed<SessionSnapshot>, HerdrError>,
    ) -> Result<(), DaemonError> {
        let now = self.clock.now();
        self.health.record(snapshot, now);
        if let Ok(observed) = snapshot {
            self.latest_snapshot = Some(observed.clone());
        }
        // §4.3 step 5's table — `evaluating`/`routed` convergence runs
        // before any observation is derived (F21); the routed row
        // re-resolves the caller's pane against this read.
        self.converge_launches(snapshot.as_ref().ok());
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        reconcile::pass(store, (policy, &self.paths), now, snapshot)?;
        // §4.10 at startup too — an obligation that outlived a restart
        // expires or admits against the one startup snapshot.
        recovery::pass(&mut self.store, now, snapshot.as_ref().ok())
    }

    /// The feed setter — `daemon::run` hands the maintainer's `watch`
    /// sender over once the task exists (before `serve`).
    pub(in crate::daemon) fn arm_subscriptions(&mut self, feed: watch::Sender<BTreeSet<String>>) {
        self.subs_feed = Some(feed);
    }

    /// Push the current child-pane set to the subscription feed (step
    /// 8). A store-read failure keeps the last set — the next pass
    /// retries; `send_if_modified` skips no-change pushes.
    pub(super) fn push_subscription_specs(&mut self) {
        let Some(feed) = &self.subs_feed else {
            return;
        };
        let panes: BTreeSet<String> = self
            .store
            .unsettled_runs()
            .unwrap_or_default()
            .iter()
            .filter_map(|run| {
                run.identity
                    .as_ref()
                    .map(|identity| identity.pane_id.0.clone())
            })
            .collect();
        let _changed = feed.send_if_modified(|current| {
            if *current == panes {
                false
            } else {
                *current = panes;
                true
            }
        });
    }

    /// Which unsettled Run a `pane.agent_status_changed` event belongs
    /// to: the identity's current locator, or — when a move renumbered
    /// the pane the event names — the agent row there carrying the
    /// Run's minted name. Misses are fine: the next tick classifies the
    /// Run anyway (events are latency, never correctness).
    fn run_for_pane(&self, pane_id: &PaneId, view: &SnapshotView) -> Option<RunId> {
        let unsettled = self.store.unsettled_runs().ok()?;
        unsettled
            .iter()
            .find_map(|run| {
                let identity = run.identity.as_ref()?;
                (identity.pane_id == *pane_id).then(|| run.id.clone())
            })
            .or_else(|| {
                let name = view.name_on(pane_id)?;
                unsettled.iter().find_map(|run| {
                    let identity = run.identity.as_ref()?;
                    (identity.agent_name == *name).then(|| run.id.clone())
                })
            })
    }
}

/// The `apply_dropped` kind spelling for a pass failure — `pass`
/// variants are `Store`/`Apply`; `delivery::pass` adds `Io` from the
/// retention sweep.
pub(super) fn kind_of(error: &DaemonError) -> &'static str {
    match error {
        DaemonError::Store(_) => "store",
        DaemonError::Io(_) => "io",
        DaemonError::Usage(_)
        | DaemonError::Config(_)
        | DaemonError::NoDaemonTable
        | DaemonError::Credential(_)
        | DaemonError::Jev(_)
        | DaemonError::Locked { .. }
        | DaemonError::Apply(_)
        | DaemonError::Seam(_) => "apply",
    }
}
