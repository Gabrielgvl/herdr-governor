//! `launch/convergence` — the §4.5 convergence-table rows this side of
//! `launching`: every tick and at startup step 5, for every Launch not
//! `done`. Idempotent by construction — each row recomputes against
//! durable state, so a row that moved under it writes nothing.
//!
//! | Phase | Journal | Action |
//! |---|---|---|
//! | `evaluating`, eval `planned` | — | `hand_out` dispatches (no row here) |
//! | `evaluating`, eval `dispatching` | restart | `mark_restart` already abstained it |
//! | `evaluating`, eval `acknowledged`/`failed`/`unconfirmed`, no decision | tick/restart | re-run `on_evaluated` from the journaled set |
//! | `routed`, run `reserved`, no topology effect | tick/restart | caller pane by native session → `begin ‖ launch_plan`, absent → `Failed{Absent}` |
//!
//! `launching` rows are B3's `reconcile/launch` — a Run ≥ `starting`
//! belongs to the observation pipeline, never to this pass.

use governor_core::identity::LaunchId;
use governor_core::lifecycle::{EffectKind, EffectState, State};
use governor_core::task::LaunchPhase;

use crate::daemon::coordinator::Coordinator;

use super::context::{eval_key, eval_related_tab};

impl Coordinator {
    /// §4.5's convergence pass — runs every tick (step 0, before any
    /// observation) and at startup step 5, strictly before the bind.
    pub(in crate::daemon) fn converge_launches(&mut self) {
        // `evaluating` rows whose eval reached a terminal state but
        // whose decide apply never landed (a drop, or the result→decide
        // kill seam): re-run `on_evaluated` from the journaled set —
        // it recomputes everything, so a re-entry writes nothing twice.
        let evaluating = self
            .store
            .launches_in_phase(LaunchPhase::Evaluating)
            .unwrap_or_default();
        for launch in evaluating {
            let terminal = self
                .store
                .effect(&eval_key(&launch.id))
                .ok()
                .flatten()
                .is_some_and(|eval| {
                    matches!(
                        eval.state,
                        EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed
                    )
                });
            if terminal {
                self.on_evaluated(&launch.id);
            }
        }
        // `routed` rows with a still-`reserved` Run and no topology leg
        // — the decide landed but `begin` never did (the kill seam
        // between the two applies, or a restart between them). The
        // caller's pane re-resolves by native session in the freshest
        // snapshot — never derived from an observation.
        let routed = self
            .store
            .launches_in_phase(LaunchPhase::Routed)
            .unwrap_or_default();
        for launch in routed {
            let Some(run) = self.store.run_by_launch(&launch.id).ok().flatten() else {
                continue;
            };
            if run.state != State::Reserved {
                continue;
            }
            let journal = self.store.journal(&run.id).unwrap_or_default();
            if journal.iter().any(|effect| {
                matches!(
                    effect.kind,
                    EffectKind::TabCreate | EffectKind::PaneSplit | EffectKind::AgentStart
                )
            }) {
                continue;
            }
            let related_tab = eval_related_tab(&self.store, &self.loaded.config.policy, &launch.id);
            self.begin_launch(&launch.id, related_tab.as_ref());
        }
    }

    /// §6.2 — a Launch B3's `launching` reconcile (which owns no
    /// coordinator state) finished resolves its parked callers: the tick
    /// runs this after `reconcile::pass`, so a `Failed`/`Launched`
    /// convergence answers in the same tick; the `done` body is already
    /// journaled.
    pub(in crate::daemon) fn drain_done_waiters(&mut self) {
        let done: Vec<LaunchId> = self
            .launch_waiters
            .keys()
            .filter(|id| {
                self.store
                    .launch(id)
                    .ok()
                    .flatten()
                    .is_some_and(|launch| launch.phase == LaunchPhase::Done)
            })
            .cloned()
            .collect();
        for id in done {
            self.drain_waiters(&id);
        }
    }
}
