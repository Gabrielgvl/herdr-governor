//! `restart` — §4.3 step 5's restart marking, the arm `daemon::run`
//! drives once before the bind (no `Msg` carries it): every
//! `dispatching` effect across settled and unsettled Runs reclassifies
//! `unconfirmed`, and a stranded `evaluating` Launch's `jev_evaluate`
//! abstains `interrupted_before_decision`. Hygiene split: the move keeps
//! `coordinator.rs` under the 500-line bound.

use std::collections::BTreeSet;

use governor_core::identity::{LaunchId, RunId, Timestamp};
use governor_core::lifecycle::{EffectKind, EffectOutcome, EffectState, Event, transition};
use governor_core::task::{AbstainReason, LaunchOutcome, LaunchPhase, finish};

use crate::daemon::reconcile;
use crate::daemon::{DaemonError, log};

use super::apply::{ApplyOutcome, Marks, apply_with_retry};
use super::{Coordinator, empty, versioned};

impl Coordinator {
    /// §4.3 step 5 — restart marking over **every** `dispatching` effect
    /// (the [r2] fix: the scan is global, not per unsettled Run — a
    /// settled Run's `close` row is reclassified too):
    ///
    /// * for every Run owning one (settled or not), apply
    ///   `transition(Event::Restart)` — `on_restart` writes
    ///   `dispatching → unconfirmed` and, for `prompting`, records the
    ///   task prompt's certainty;
    /// * for every `evaluating` Launch whose `jev_evaluate` is left
    ///   `dispatching`, apply `finish(Abstained{InterruptedBeforeDecision},
    ///   Some(Dispatching), None, …)` — the row's honest certainty is
    ///   `unknown` (OQ-13).
    ///
    /// Per-subject applies — one bad row drops only its own mark. The
    /// [r2] outbox `Unconfirmed` resolution composes in the same
    /// transition through `reconcile::linked_outbox_resolution` (B3's
    /// private copy; C2's shared helper replaces it).
    pub(in crate::daemon) fn mark_restart(&mut self, now: Timestamp) -> Result<Marks, DaemonError> {
        let dispatching = self.store.effects_in_state(EffectState::Dispatching)?;
        let mut marks = Marks {
            effects: dispatching.len(),
            ..Marks::default()
        };
        let mut runs = BTreeSet::<RunId>::new();
        let mut evals = BTreeSet::<LaunchId>::new();
        for effect in &dispatching {
            if let Some(run) = &effect.subject_run {
                runs.insert(run.clone());
            }
            if effect.kind == EffectKind::JevEvaluate
                && let Some(launch) = &effect.subject_launch
            {
                evals.insert(launch.clone());
            }
        }
        // Disjoint field borrows: the store mutates, the policy only reads.
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        for run_id in runs {
            match apply_with_retry(store, now, |st| {
                let Some(run) = st.run(&run_id).ok().flatten() else {
                    return empty();
                };
                let journal = st.journal(&run_id).unwrap_or_default();
                let mut restart = transition(
                    &run,
                    &versioned(&run, Event::Restart),
                    now,
                    policy,
                    (None, journal.as_slice(), &[]),
                    "",
                );
                // [r2] §4.3 step 5 — an outbox-linked `dispatching` row
                // that this restart just marked `unconfirmed` resolves
                // its outbox entry in the same transaction (and owes the
                // `follow_up_unconfirmed` event with it).
                for effect in &journal {
                    if effect.state == EffectState::Dispatching
                        && let Some((write, event)) = reconcile::linked_outbox_resolution(
                            st,
                            effect,
                            EffectOutcome::Unconfirmed,
                        )
                    {
                        restart.state_changes.push(write);
                        if let Some(mailbox) = event {
                            restart.events.push(mailbox);
                        }
                    }
                }
                restart
            })? {
                ApplyOutcome::Applied { .. } => marks.runs = marks.runs.saturating_add(1),
                ApplyOutcome::Dropped { .. } => {}
            }
        }
        for launch_id in evals {
            match apply_with_retry(store, now, |st| {
                let Some(launch) = st.launch(&launch_id).ok().flatten() else {
                    return empty();
                };
                if launch.phase != LaunchPhase::Evaluating {
                    return empty();
                }
                finish(
                    &launch,
                    LaunchOutcome::Abstained {
                        reason: AbstainReason::InterruptedBeforeDecision,
                    },
                    Some(EffectState::Dispatching),
                    None,
                    now,
                    policy,
                )
            })? {
                ApplyOutcome::Applied { .. } => marks.evals = marks.evals.saturating_add(1),
                ApplyOutcome::Dropped { .. } => {}
            }
        }
        log::restart_marks(marks.effects, marks.runs, marks.evals);
        Ok(marks)
    }
}
