//! `effects` — B1's dispatch arms (§4.4): `hand_out` (the ready-effect
//! scan with per-subject serialization), the `DispatchCommit` arm (the
//! composed commit/refusal over `gate`'s revalidation), and the
//! `EffectResult` arm (the core's `Event::EffectResult` lane plus the
//! linked-outbox resolution and the `result_committed` checkpoint).
//!
//! `gate` — the §4.4 step-3 revalidation and its audits.

mod gate;

use std::sync::Arc;

use governor_core::delivery::FollowUpWrite;
use governor_core::identity::{EffectKey, LaunchId, RunId, Timestamp};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectResolution, EffectResult, EffectState, EffectWrite, Event,
    JudgmentVerdict, StateChange, Transition, VersionTriple, Versioned, transition,
};
use tokio::sync::oneshot;

use crate::daemon::log;
use crate::daemon::reconcile;
use crate::daemon::runner::{self, Dispatch};
use crate::daemon::seam::Boundary;
use crate::daemon::{CommitVerdict, RenderContext};

use super::apply::{ApplyOutcome, apply_with_retry, concat};
use super::{Coordinator, empty, versioned};
use gate::Gate;

/// The subject an in-flight effect serializes on — its Run when bound to
/// one, else its Launch (launch evaluations, launch-bound hints). §4.4's
/// "at most one in-flight effect per subject" key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Subject {
    /// `subject_run`.
    Run(RunId),
    /// `subject_launch`.
    Launch(LaunchId),
}

/// `subject_run` wins when both are set (Appendix B requires exactly one).
fn subject_of(effect: &Effect) -> Option<Subject> {
    if let Some(run) = &effect.subject_run {
        return Some(Subject::Run(run.clone()));
    }
    effect.subject_launch.clone().map(Subject::Launch)
}

impl Coordinator {
    /// Wire the runner pool in — called once by `daemon::run` after the
    /// mailbox channel exists (`tx` is part of the env).
    pub(in crate::daemon) fn arm_runner(&mut self, env: runner::RunnerEnv) {
        self.runner = Some(env);
    }

    /// The §4.4 hand-off scan: every `planned` effect the store offers, in
    /// `planned_at, effect_id` order, minus subjects already in flight. A
    /// `None` context leaves the row `planned` (a later pass may render
    /// it); the subject key is claimed only once the dispatch is spawned.
    pub(in crate::daemon) fn hand_out(&mut self) {
        let Some(env) = self.runner.clone() else {
            return;
        };
        let Ok(ready) = self.store.ready_effects() else {
            return;
        };
        for effect in ready {
            let Some(subject) = subject_of(&effect) else {
                continue;
            };
            if self.in_flight.contains_key(&subject) {
                continue;
            }
            let Some(context) = runner::context_for(&self.store, &self.paths, &effect) else {
                continue;
            };
            self.in_flight.insert(subject, effect.key.clone());
            let dispatch = Dispatch {
                effect,
                context: Arc::new(context),
            };
            drop(tokio::spawn(runner::drive(env.clone(), dispatch)));
        }
    }

    /// §4.4 step 3 — the `DispatchCommit` arm. The runner asks; the
    /// coordinator re-reads every durable input and answers `Go`,
    /// `Skip`, or `Refused` after committing the corresponding write —
    /// the process-to-completion contract means no `cancel`, settlement
    /// or reload can interleave between the re-read and the write.
    pub(super) fn on_dispatch_commit(
        &mut self,
        key: &EffectKey,
        context: &RenderContext,
        reply: oneshot::Sender<CommitVerdict>,
    ) {
        let now = self.clock.now();
        let verdict = match self.commit_gate(key, context, now) {
            Gate::Skip => CommitVerdict::Skip,
            Gate::Go => match self.commit_dispatch(key, now) {
                CommitOutcome::Applied => CommitVerdict::Go,
                // The CAS never landed — the row state is unknown to this
                // arm, so the wire write must not proceed; the next
                // hand-off re-asks.
                CommitOutcome::Lost => CommitVerdict::Skip,
            },
            Gate::Refuse(resolution) => {
                self.commit_refused(key, &resolution, now);
                CommitVerdict::Refused
            }
        };
        if verdict != CommitVerdict::Go {
            self.free(key);
        }
        let _gone = reply.send(verdict);
    }

    /// §4.4 step 7 — the `EffectResult` arm: the core's
    /// `Event::EffectResult` lane against the current durable state, the
    /// linked-outbox resolution composed into the same apply, and the
    /// `result_committed` seam checkpoint after it lands.
    pub(super) async fn on_effect_result(&mut self, result: Box<EffectResult>) {
        let now = self.clock.now();
        let outcome = result.resolution.outcome();
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        let applied = apply_with_retry(store, now, |st| {
            let Some(row) = st.effect(&result.key).ok().flatten() else {
                return empty();
            };
            if row.state != EffectState::Dispatching {
                return empty();
            }
            let mut applied = match row.subject_run.clone() {
                Some(run_id) => {
                    let Some(run) = st.run(&run_id).ok().flatten() else {
                        return empty();
                    };
                    let launch = st.launch(&run.launch).ok().flatten();
                    let journal = st.journal(&run.id).unwrap_or_default();
                    let handoffs = st.handoffs(&run.id).unwrap_or_default();
                    let mut emitted = transition(
                        &run,
                        &versioned(&run, Event::EffectResult((*result).clone())),
                        now,
                        policy,
                        (
                            launch.as_ref().and_then(|l| l.decision.as_ref()),
                            journal.as_slice(),
                            handoffs.as_slice(),
                        ),
                        "",
                    );
                    // F24 — an answered acceptance set's verdict rides the
                    // same commit as a `judgment` event stamped with the
                    // set's own request versions (§4.9; a stale set's stamp
                    // is dropped by the transition's staleness check).
                    if let Some(judgment) = acceptance_lift(&result, launch.as_ref()) {
                        let lifted = transition(
                            &run,
                            &judgment,
                            now,
                            policy,
                            (
                                launch.as_ref().and_then(|l| l.decision.as_ref()),
                                journal.as_slice(),
                                handoffs.as_slice(),
                            ),
                            "",
                        );
                        emitted = concat(emitted, [lifted]);
                    }
                    emitted
                }
                // Launch-bound results (the admission lane's `evaluate`)
                // journal the result write directly — no Run exists to
                // transition against; B2's admission consumes the set.
                None => Transition {
                    state_changes: Vec::from([StateChange::WriteEffect(EffectWrite::Result {
                        key: result.key.clone(),
                        resolution: result.resolution.clone(),
                    })]),
                    events: Vec::new(),
                    effects: Vec::new(),
                },
            };
            // §4.8 — an outbox-linked effect's terminal outcome resolves
            // its entry in the same transaction.
            if let Some((write, event)) = reconcile::linked_outbox_resolution(st, &row, outcome) {
                applied.state_changes.push(write);
                if let Some(mailbox) = event {
                    applied.events.push(mailbox);
                }
            }
            applied
        });
        self.free(&result.key);
        match applied {
            Ok(ApplyOutcome::Applied { .. }) => {
                self.push_subscription_specs();
                runner::seam::checkpoint(
                    &result.key,
                    Boundary::ResultCommitted,
                    self.seam().as_ref(),
                )
                .await;
            }
            Ok(ApplyOutcome::Dropped { .. }) | Err(_) => {
                log::apply_dropped(3, "result_apply");
            }
        }
    }

    /// Release a subject slot by its effect key — the result/commit arms
    /// and the `ReleaseSubject` message drop the entry whichever subject
    /// it was claimed under.
    pub(super) fn free(&mut self, key: &EffectKey) {
        self.in_flight.retain(|_, held| held != key);
    }

    /// The `[WriteEffect::Dispatch]` commit — plus the outbox link for a
    /// `run:<id>:outbox:<seq>` prompt (the two rows move together, §4.8).
    fn commit_dispatch(&mut self, key: &EffectKey, now: Timestamp) -> CommitOutcome {
        match apply_with_retry(&mut self.store, now, |st| {
            let Some(row) = st.effect(key).ok().flatten() else {
                return empty();
            };
            if row.state != EffectState::Planned {
                return empty();
            }
            let mut transition = Transition {
                state_changes: Vec::from([StateChange::WriteEffect(EffectWrite::Dispatch {
                    key: key.clone(),
                })]),
                events: Vec::new(),
                effects: Vec::new(),
            };
            if let Some((run, seq)) = outbox_target(key, &row) {
                transition.state_changes.push(StateChange::WriteFollowUp(
                    FollowUpWrite::Dispatch {
                        run,
                        seq,
                        effect: row.id.clone(),
                    },
                ));
            }
            transition
        }) {
            Ok(ApplyOutcome::Applied { .. }) => CommitOutcome::Applied,
            Ok(ApplyOutcome::Dropped { .. }) | Err(_) => CommitOutcome::Lost,
        }
    }

    /// The governor-refused commit (§4.2): `[Dispatch]` + the core's
    /// `EffectResult` transition in ONE apply — `planned → dispatching →
    /// failed` atomically, so a refusal journals the cause and still
    /// runs the result lane's consequences (F15's fallback plan). On a
    /// lost CAS the row's truth is unknown — the refused write stays
    /// uncommitted and the effect re-asks on the next pass.
    fn commit_refused(&mut self, key: &EffectKey, resolution: &EffectResolution, now: Timestamp) {
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        let applied = apply_with_retry(store, now, |st| {
            let Some(row) = st.effect(key).ok().flatten() else {
                return empty();
            };
            if row.state != EffectState::Planned {
                return empty();
            }
            let dispatch = Transition {
                state_changes: Vec::from([StateChange::WriteEffect(EffectWrite::Dispatch {
                    key: key.clone(),
                })]),
                events: Vec::new(),
                effects: Vec::new(),
            };
            let result = EffectResult {
                key: key.clone(),
                kind: row.kind,
                resolution: resolution.clone(),
            };
            match row.subject_run.clone() {
                Some(run_id) => {
                    let Some(run) = st.run(&run_id).ok().flatten() else {
                        return empty();
                    };
                    let launch = st.launch(&run.launch).ok().flatten();
                    let journal = st.journal(&run.id).unwrap_or_default();
                    let handoffs = st.handoffs(&run.id).unwrap_or_default();
                    concat(
                        dispatch,
                        [transition(
                            &run,
                            &versioned(&run, Event::EffectResult(result.clone())),
                            now,
                            policy,
                            (
                                launch.as_ref().and_then(|l| l.decision.as_ref()),
                                journal.as_slice(),
                                handoffs.as_slice(),
                            ),
                            "",
                        )],
                    )
                }
                None => concat(
                    dispatch,
                    [Transition {
                        state_changes: Vec::from([StateChange::WriteEffect(EffectWrite::Result {
                            key: key.clone(),
                            resolution: resolution.clone(),
                        })]),
                        events: Vec::new(),
                        effects: Vec::new(),
                    }],
                ),
            }
        });
        if let Err(_error) = applied {
            log::apply_dropped(3, "refused_apply");
        }
    }
}

/// The dispatch-commit apply's verdict, internal to `commit_dispatch`.
enum CommitOutcome {
    /// The CAS landed.
    Applied,
    /// Conflicted out / the row moved between revalidation and apply.
    Lost,
}

/// An outbox prompt's `(run, seq)` — the `WriteFollowUp::Dispatch` link
/// that rides the commit (`run:<id>:outbox:<seq>`, `rsplit` keeps run ids
/// containing `:` honest).
fn outbox_target(key: &EffectKey, row: &Effect) -> Option<(RunId, u64)> {
    if row.kind != EffectKind::Prompt {
        return None;
    }
    let rest = key.0.strip_prefix("run:")?;
    let (run, seq) = rest.rsplit_once(":outbox:")?;
    Some((RunId(run.to_owned()), seq.parse().ok()?))
}

/// The F24 acceptance lift: an `answered` `acceptance` record yields the
/// verdict `Event::Judgment` carries, stamped with the *set's* request
/// versions (§4.9) so a moved generation drops it in the transition.
fn acceptance_lift(
    result: &EffectResult,
    launch: Option<&governor_core::task::Launch>,
) -> Option<Versioned<Event>> {
    let EffectResolution::Acknowledged {
        receipt: Some(governor_core::lifecycle::EffectReceipt::Judgments(record)),
    } = &result.resolution
    else {
        return None;
    };
    let versions: VersionTriple = record.set.versions?;
    let count = u8::try_from(launch?.task.done_when.len()).unwrap_or(u8::MAX);
    let verdict: JudgmentVerdict = governor_core::acceptance::acceptance_verdict(record, count)?;
    Some(Versioned {
        requested_against: versions,
        value: Event::Judgment(verdict),
    })
}
