//! `launch/decide` — the applies that move a Launch forward once the
//! evaluation answered: `evaluating → done` (abstain/verdict), the
//! `decided` transaction (routing decision + reserved Run + recovery
//! `dispatched`), and `routed → launching` (`begin ‖ launch_plan`) or
//! the absent-caller `Failed{Absent}` finish. Each apply recomputes
//! against the durable row — a phase that moved mid-CAS writes nothing.

use std::io;

use governor_core::config::Policy;
use governor_core::identity::{CallerKey, LaunchId, PaneId, RunId, Timestamp};
use governor_core::lifecycle::{EffectCertainty, State, launch_plan, reserved_run};
use governor_core::routing::{Decision, Evaluation, TabChoice, placement_plan};
use governor_core::task::{
    AbstainReason, Launch, LaunchOutcome, LaunchPhase, begin, decided, finish,
};

use crate::daemon::coordinator::apply::{ApplyOutcome, apply_with_retry, concat};
use crate::daemon::coordinator::{Coordinator, empty};
use crate::daemon::{identity, ids};
use crate::store::{ApplyError, Store};

use super::BasePin;
use super::context::{
    caller_tabs, eval_finish, eval_key, no_topology, pending_obligation, recovery_move,
};

impl Coordinator {
    /// `evaluating` → `done` with an abstention — `eval_finish`'s
    /// compose (stranded-eval write + recovery `blocked`) in one
    /// bounded apply, then the waiter drain and the base cleanup.
    pub(in crate::daemon) fn eval_abstain(&mut self, launch_id: &LaunchId, reason: AbstainReason) {
        self.finish_evaluating(launch_id, &LaunchOutcome::Abstained { reason });
    }

    /// `evaluating` → `done` with any terminal outcome — one bounded
    /// apply recomputing `eval_finish` against the current row (a
    /// phase that moved mid-CAS recomputes to nothing), then the
    /// waiter drain.
    pub(in crate::daemon) fn finish_evaluating(
        &mut self,
        launch_id: &LaunchId,
        outcome: &LaunchOutcome,
    ) {
        let now = self.clock.now();
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        let applied = apply_with_retry(store, now, |st| {
            let Some(current) = st.launch(launch_id).ok().flatten() else {
                return empty();
            };
            if current.phase != LaunchPhase::Evaluating {
                return empty();
            }
            let eval_state = st
                .effect(&eval_key(launch_id))
                .ok()
                .flatten()
                .map(|eval| eval.state);
            eval_finish(st, &current, outcome, eval_state, now, policy)
        });
        self.launch_bases.remove(launch_id);
        if let Err(_error) = applied {
            crate::daemon::log::apply_dropped(3, "conflict");
        }
        self.answer_waiters(launch_id);
    }

    /// §4.5 — `decided`: the immutable routing decision, the config
    /// version and the reserved Run in one transaction, with the
    /// recovery obligation's `dispatched` write riding along (F21). The
    /// F6 base is the admission pin — or, after a restart lost it, the
    /// re-probe `base_for` starts (this pass then waits for its verdict).
    pub(in crate::daemon) fn decide(
        &mut self,
        launch: &Launch,
        decision: &Decision,
        evaluation: &Evaluation,
        now: Timestamp,
    ) {
        let BasePin::Pinned(base) = self.base_for(launch) else {
            return;
        };
        let cwd = launch
            .task
            .cwd
            .clone()
            .unwrap_or_else(|| launch.project_root.0.clone());
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        let applied = apply_decided(
            store,
            (now, policy),
            &launch.id,
            decision,
            (&cwd, base.as_deref()),
            ids::mint_run_id,
        );
        match applied {
            Ok(ApplyOutcome::Applied { .. }) => {
                // The pin served `decided` — clear it only now the
                // Launch left `evaluating` (a dropped apply retries
                // with it).
                self.launch_bases.remove(&launch.id);
                self.begin_launch(&launch.id, evaluation.related_tab.as_ref());
            }
            Ok(ApplyOutcome::Dropped { .. }) | Err(_) => {
                // The row stays `evaluating` + `acknowledged` — the
                // tick's converge row re-runs `on_evaluated`, which
                // recomputes everything against durable state.
            }
        }
    }

    /// `routed` → `launching`: the caller's pane resolves by native
    /// session in the freshest snapshot — never derived from an
    /// observation (the §4.5 restart row's rule). A snapshot-proven
    /// absence finishes `Failed{Absent}` and settles the reserved Run
    /// `launch_not_started` in the same write; `begin ‖ launch_plan`
    /// is one atomic apply.
    pub(in crate::daemon) fn begin_launch(
        &mut self,
        launch_id: &LaunchId,
        related_tab: Option<&TabChoice>,
    ) {
        let now = self.clock.now();
        let Some(launch) = self.store.launch(launch_id).ok().flatten() else {
            return;
        };
        if launch.phase != LaunchPhase::Routed {
            return;
        }
        let Some(decision) = launch.decision.clone() else {
            return;
        };
        if self.store.run_by_launch(launch_id).ok().flatten().is_none() {
            return;
        }
        let counts = self
            .latest_snapshot
            .as_ref()
            .map(|observed| caller_tabs(&launch.caller, &observed.value))
            .unwrap_or_default();
        let plan = placement_plan(related_tab, &counts);
        if let Some(pane) = self.caller_pane(&launch.caller) {
            let applied = apply_with_retry(&mut self.store, now, |st| {
                let Some(current) = st.launch(launch_id).ok().flatten() else {
                    return empty();
                };
                if current.phase != LaunchPhase::Routed {
                    return empty();
                }
                let Some(run) = st.run_by_launch(launch_id).ok().flatten() else {
                    return empty();
                };
                if run.state != State::Reserved {
                    return empty();
                }
                concat(
                    begin(&current),
                    [launch_plan(&run, &decision, &plan, &pane)],
                )
            });
            if let Err(_error) = applied {
                crate::daemon::log::apply_dropped(3, "conflict");
            }
        } else {
            // Caller provably absent — `Failed{Absent}` settles the
            // reserved Run `launch_not_started` in the same write.
            let (store, policy) = (&mut self.store, &self.loaded.config.policy);
            let applied = apply_with_retry(store, now, |st| {
                let Some(current) = st.launch(launch_id).ok().flatten() else {
                    return empty();
                };
                if current.phase != LaunchPhase::Routed {
                    return empty();
                }
                let Some(run) = st.run_by_launch(launch_id).ok().flatten() else {
                    return empty();
                };
                finish(
                    &current,
                    LaunchOutcome::Failed {
                        certainty: EffectCertainty::Absent,
                        run: Some(run.id.clone()),
                        created_topology: no_topology(),
                    },
                    None,
                    Some(&run),
                    now,
                    policy,
                )
            });
            if let Err(_error) = applied {
                crate::daemon::log::apply_dropped(3, "conflict");
            }
        }
        self.answer_waiters(launch_id);
    }

    /// The caller's pane for topology planning: `native_session`
    /// resolved in the freshest snapshot — `None` (proven absent)
    /// means the launch fails `Absent`; without a snapshot the last
    /// bound pane is the best locator the journal can carry (the
    /// wire's fresh `CallerPane` re-resolution is the honest gate
    /// regardless).
    fn caller_pane(&self, caller: &CallerKey) -> Option<PaneId> {
        match &self.latest_snapshot {
            Some(observed) => identity::agent_rows(&observed.value)
                .iter()
                .find(|row| row.4.as_ref() == Some(&caller.native_session))
                .map(|row| row.0.clone()),
            None => self.store.caller_pane(caller).ok().flatten(),
        }
    }
}

/// §4.5/F1 — the `decided` apply: `decided(launch, decision, reserved)`
/// plus the pending obligation's `dispatched` write and its
/// `recovery_dispatched` event (F21), recomputed against
/// the durable row on every attempt. Each attempt reserves under a fresh
/// id from `mint` — a `Conflict{Run}` (a taken `run_id`, or a taken
/// `gov-<runId[0..8]>` child name) re-mints rather than replaying the
/// colliding id; `apply_with_retry` bounds it at three attempts, then
/// drops (the Launch stays `evaluating` for the tick's converge row —
/// never a partial write). A mint failure writes nothing that attempt.
pub(in crate::daemon) fn apply_decided(
    store: &mut Store,
    (now, policy): (Timestamp, &Policy),
    launch_id: &LaunchId,
    decision: &Decision,
    (cwd, base): (&str, Option<&str>),
    mut mint: impl FnMut() -> io::Result<RunId>,
) -> Result<ApplyOutcome, ApplyError> {
    apply_with_retry(store, now, |st| {
        let Some(current) = st.launch(launch_id).ok().flatten() else {
            return empty();
        };
        if current.phase != LaunchPhase::Evaluating {
            return empty();
        }
        let Ok(run_id) = mint() else {
            return empty();
        };
        let run = reserved_run(
            &current,
            run_id,
            cwd.to_owned(),
            base.map(str::to_owned),
            now,
            policy,
        );
        let mut transition = decided(&current, decision, &run);
        if let Some(dispatched) =
            pending_obligation(st, &current).and_then(|o| o.dispatched(current.id.clone()))
        {
            recovery_move(&mut transition, dispatched);
        }
        transition
    })
}
