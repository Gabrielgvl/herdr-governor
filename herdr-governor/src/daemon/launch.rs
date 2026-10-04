//! `launch` — §4.5's `herdr_launch` pipeline on the coordinator side:
//! admission (F5 task validation, F11 idempotency, F21 recovery
//! admission, the `evaluating` row plus its planned `jev_evaluate`), the
//! journaled-set evaluation path (validate → verdict → route → `decided`
//! → `begin ‖ launch_plan`), and the `launch_wait` waiters the
//! `tools/call` replies park behind. `launch/context` holds the shared
//! values (incl. the start-ack `finish(Launched)` compose and the F21
//! recovery moves); `launch/decide` the phase-advance applies;
//! `launch/base` the F6 base pin and its restart re-probe;
//! `launch/convergence` the `evaluating`/`routed` rows a restart or tick
//! can leave behind (B3's `reconcile/launch` owns `launching`).

mod base;
mod context;
mod convergence;
mod decide;

pub(in crate::daemon) use base::BasePin;
pub(in crate::daemon) use context::{eval_context, eval_finish, launched, launched_on_start};
#[cfg(test)]
pub(in crate::daemon) use decide::apply_decided;

use tokio::sync::oneshot;

use governor_core::identity::{CallerKey, IdempotencyKey, LaunchId, ProjectRoot, Timestamp};
use governor_core::lifecycle::{EffectReceipt, EffectState};
use governor_core::recovery::{RecoveryObligation, successor_key, successor_task};
use governor_core::routing::{cooling_down, evaluation_verdict, route, validate_evaluation};
use governor_core::task::{
    AbstainReason, LaunchPhase, LaunchResponse, Refusal, Task, admission_decision, admit,
    new_launch,
};

use crate::daemon::api::{ToolError, ToolPrepared, ToolResponse};
use crate::daemon::coordinator::apply::{ApplyOutcome, apply_with_retry, concat};
use crate::daemon::coordinator::{Coordinator, Msg, empty};
use crate::daemon::{identity, ids};

use self::context::{asked_tabs, eval_key, launch_body, recovery_write};

/// The answer the `tools/call` arm resolves to: send now, or park the
/// reply under the Launch's waiters.
pub(in crate::daemon) enum Admission {
    /// Answer immediately — a refusal or an idempotent terminal replay.
    Answer(ToolResponse),
    /// Park the reply behind `launch_waiters` for this Launch.
    Park(LaunchId),
}

impl Coordinator {
    /// §4.5 steps 1–5 — the `herdr_launch` admission: F5 validation, the
    /// prepared `cwd`/`base_commit` evidence (F6 — both refusal classes
    /// precede any recording), F11 idempotency, F21 `caller_admission`,
    /// then the `evaluating` row plus the planned `jev_evaluate` and a
    /// caller-claim obligation atomically. The reply parks under
    /// `launch_waiters` (a `Park` answer) — the §6.2 bodies resolve on
    /// `done` or `launch_wait`.
    pub(in crate::daemon) fn launch_admit(
        &mut self,
        caller: &CallerKey,
        caller_key: &IdempotencyKey,
        request_task: Task,
        project_root: &ProjectRoot,
        prepared: ToolPrepared,
    ) -> Admission {
        let now = self.clock.now();
        // F21 — a `recovery_of` launch records under the deterministic
        // `successor_key`/`successor_task` shape: the key is the durable
        // obligation link the expiry sweep reads, and a caller retry
        // re-derives the same digest (the stored row replays idempotently).
        let (key, task) = match request_task.recovery_of.clone() {
            Some(predecessor) => (
                successor_key(&predecessor),
                successor_task(&predecessor, &request_task),
            ),
            None => (caller_key.clone(), request_task),
        };
        // F5 — violations precede every durable write; a `resolved_cwd`
        // refusal is the same class (the path could not canonicalize).
        let violations = task.violations(project_root);
        if !violations.is_empty() {
            return Admission::Answer(Err(ToolError::new(
                ToolError::TASK_INVALID,
                violations.join(","),
            )));
        }
        // The resolved-cwd refusal is the same `TASK_INVALID` class —
        // before idempotency and before any recording.
        if let Err(error) = prepared.resolved_cwd {
            return Admission::Answer(Err(error));
        }
        // F11 — the (caller, project_root, key) scope: same digest → the
        // stored result or `pending`; a different one → key conflict.
        if let Some(answer) = self.replay(caller, project_root, &key, &task) {
            return answer;
        }
        // F6 — the `base_commit` probe gates a NEW recording only: an
        // idempotent replay is not an admission, so a transient git
        // outage must not mask the stored outcome.
        let base_commit = match prepared.base_commit {
            Ok(base) => base,
            Err(error) => return Admission::Answer(Err(error)),
        };
        // F21 — `recovery_of` admission runs before the row exists: a
        // refusal records nothing; an admitted claim returns the
        // obligation `admit` records `pending`.
        let obligation = match self.recovery_admission(caller, &task, now) {
            Ok(obligation) => obligation,
            Err(error) => return Admission::Answer(Err(error)),
        };
        let launch_id = match ids::mint_launch_id(now) {
            Ok(id) => id,
            Err(error) => {
                return Admission::Answer(Err(ToolError::new(
                    ToolError::DAEMON_UNAVAILABLE,
                    format!("launch id mint: {error}"),
                )));
            }
        };
        let launch = new_launch(
            launch_id,
            caller.clone(),
            project_root.clone(),
            key.clone(),
            task,
        );
        let applied = apply_with_retry(&mut self.store, now, |st| {
            // The idempotency check re-runs inside the apply: a same-key
            // row that landed mid-retry absorbs this attempt.
            if st
                .launch_by_idempotency(caller, project_root, &key)
                .ok()
                .flatten()
                .is_some()
            {
                return empty();
            }
            concat(
                admit(&launch),
                obligation
                    .clone()
                    .map_or_else(Vec::new, |o| vec![recovery_write(o)]),
            )
        });
        match applied {
            Ok(ApplyOutcome::Applied { .. }) => {
                self.launch_bases
                    .insert(launch.id.clone(), BasePin::Pinned(base_commit));
                Admission::Park(launch.id)
            }
            Ok(ApplyOutcome::Dropped { .. }) | Err(_) => Admission::Answer(Err(ToolError::new(
                ToolError::DAEMON_UNAVAILABLE,
                "launch admission apply",
            ))),
        }
    }

    /// F11 — an existing Launch under `(caller, project_root, key)`: the
    /// stored outcome (`done`), a park behind the in-flight one, or the
    /// typed key conflict. `None` — no such Launch; admit a new one.
    fn replay(
        &self,
        caller: &CallerKey,
        project_root: &ProjectRoot,
        key: &IdempotencyKey,
        task: &Task,
    ) -> Option<Admission> {
        let existing = match self.store.launch_by_idempotency(caller, project_root, key) {
            Ok(existing) => existing?,
            Err(_error) => {
                return Some(Admission::Answer(Err(ToolError::new(
                    ToolError::DAEMON_UNAVAILABLE,
                    "idempotency read failed",
                ))));
            }
        };
        let run = self
            .store
            .run_by_launch(&existing.id)
            .ok()
            .flatten()
            .map(|run| run.id);
        match admission_decision(
            caller,
            project_root,
            key,
            &task.digest(),
            Some(&existing),
            run,
        ) {
            Ok(Some(LaunchResponse::Outcome(outcome))) => {
                Some(Admission::Answer(Ok(context::outcome_body(&outcome))))
            }
            Ok(Some(LaunchResponse::Pending { launch, .. })) => Some(Admission::Park(launch)),
            Ok(None) => None,
            Err(refusal) => Some(Admission::Answer(Err(ToolError::refusal(
                refusal,
                "the idempotency key names a different Task",
            )))),
        }
    }

    /// F21 — the `recovery_of` gate: the predecessor must be this
    /// caller's settled Run, its observation must prove it is not still
    /// live (the freshest snapshot classifies it — a Run that never
    /// captured an identity provably has no live child), and an existing
    /// claimable obligation is preserved rather than duplicated. A
    /// successor Launch in ANY idempotency scope is itself a recorded
    /// recovery — a second is `RECOVERY_EXISTS`.
    /// Returns the obligation `admit` records `pending`, or the typed
    /// refusal.
    fn recovery_admission(
        &self,
        caller: &CallerKey,
        task: &Task,
        now: Timestamp,
    ) -> Result<Option<RecoveryObligation>, ToolError> {
        let Some(predecessor_id) = &task.recovery_of else {
            return Ok(None);
        };
        let Some(predecessor) = self.store.run(predecessor_id).ok().flatten() else {
            return Err(ToolError::refusal(
                Refusal::RecoveryPredecessorUnsettled,
                "recoveryOf names no run",
            ));
        };
        // The owner check precedes the successor read: a non-owner's
        // refusal says `NOT_OWNER`, never whether a recovery exists
        // (`caller_admission` re-verifies ownership inside its own
        // gate).
        if predecessor.owner != *caller {
            return Err(ToolError::refusal(
                Refusal::NotOwner,
                "recovery admission refused",
            ));
        }
        // F21 — one recovery per predecessor across every scope: the
        // `(caller, project_root, key)` uniqueness `replay` checks
        // cannot see a `recovery:<pred>` successor admitted under
        // another root, and a claimable `provider_limit` obligation
        // stays `pending` — claimable — through that admission. An
        // exact in-scope replay already returned its stored outcome.
        if self
            .store
            .recovery_successor(predecessor_id)
            .map_err(|_error| {
                ToolError::new(
                    ToolError::DAEMON_UNAVAILABLE,
                    "recovery successor read failed",
                )
            })?
            .is_some()
        {
            return Err(ToolError::refusal(
                Refusal::RecoveryExists,
                "a recovery already exists for the predecessor",
            ));
        }
        let obligation = self
            .store
            .recoveries_by_state(governor_core::recovery::RecoveryStatus::Pending)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .find(|obligation| obligation.predecessor == *predecessor_id);
        let observation = match (&predecessor.identity, &self.latest_snapshot) {
            (Some(child), Some(observed)) => governor_core::identity::classify(
                child,
                Some(&identity::incarnation(&observed.epoch)),
                &identity::agent_rows(&observed.value),
            ),
            // No snapshot → the identity cannot be re-proven — ambiguous,
            // never absence.
            (Some(_), None) => governor_core::identity::Observation::Invalid,
            (None, _) => governor_core::identity::Observation::Absent,
        };
        governor_core::recovery::caller_admission(
            &predecessor,
            obligation.as_ref(),
            &observation,
            caller,
            now,
            &self.loaded.config.policy,
        )
        .map(Some)
        .map_err(|refusal| ToolError::refusal(refusal, "recovery admission refused"))
    }

    /// Park the `tools/call` reply under this Launch's waiters and arm
    /// the `launch_wait` bound — the `LaunchWait` message answers
    /// `pending`; a terminal apply answers the outcome first.
    pub(in crate::daemon) fn park_launch_waiter(
        &mut self,
        launch_id: &LaunchId,
        reply: oneshot::Sender<ToolResponse>,
    ) {
        let waiters = self.launch_waiters.entry(launch_id.clone()).or_default();
        let first = waiters.is_empty();
        waiters.push(reply);
        // One bound per Launch — a second parked reply rides the same
        // `LaunchWait`, never re-arms it.
        if !first {
            return;
        }
        let Some(env) = self.runner.as_ref() else {
            return;
        };
        let tx = env.tx.clone();
        let wait = self.daemon.launch_wait;
        let id = launch_id.clone();
        drop(tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _dropped = tx.send(Msg::LaunchWait { launch: id }).await;
        }));
    }

    /// The `LaunchWait` arm — the wait bound elapsed: answer every parked
    /// reply `pending` (or the terminal outcome, if the apply landed
    /// between the deadline and the message).
    pub(in crate::daemon) fn launch_wait_expired(&mut self, launch_id: &LaunchId) {
        self.drain_waiters(launch_id);
    }

    /// The post-apply drain: only a `done` Launch resolves its waiters —
    /// a `routed`/`launching` row keeps them parked for the terminal
    /// outcome or the `launch_wait` bound. The F18 `launch_answered`
    /// mailbox event the `finish` compose emits is the once-only marker
    /// for callers watching events instead.
    pub(in crate::daemon) fn answer_waiters(&mut self, launch_id: &LaunchId) {
        if self
            .store
            .launch(launch_id)
            .ok()
            .flatten()
            .is_none_or(|launch| launch.phase != LaunchPhase::Done)
        {
            return;
        }
        self.drain_waiters(launch_id);
    }

    /// Send the parked replies the Launch's current body — `done` is the
    /// stored outcome, anything else `pending {launchId, runId?}`.
    fn drain_waiters(&mut self, launch_id: &LaunchId) {
        let Some(launch) = self.store.launch(launch_id).ok().flatten() else {
            return;
        };
        let Some(senders) = self.launch_waiters.remove(launch_id) else {
            return;
        };
        let run = self.store.run_by_launch(launch_id).ok().flatten();
        let body = launch_body(&launch, run.as_ref());
        for sender in senders {
            let _unused = sender.send(Ok(body.clone()));
        }
    }

    /// §4.5 step 6 — one journaled `evaluate` result, one decision pass:
    /// validate the set, verdict or abstain, route, then `decided` and
    /// `begin ‖ launch_plan`. Idempotent — every apply recomputes
    /// against the durable row, so a restart re-entry writes nothing
    /// twice; the convergence rows call it too.
    pub(in crate::daemon) fn on_evaluated(&mut self, launch_id: &LaunchId) {
        let now = self.clock.now();
        let Some(launch) = self.store.launch(launch_id).ok().flatten() else {
            return;
        };
        if launch.phase != LaunchPhase::Evaluating {
            return;
        }
        let Some(eval) = self.store.effect(&eval_key(launch_id)).ok().flatten() else {
            return;
        };
        match eval.state {
            // Still in flight — the runner or the restart marker owns it.
            EffectState::Planned | EffectState::Dispatching => return,
            // A terminal non-ack state is an unanswered ask — the same
            // `evaluation_failed` abstention a Jev refusal produces
            // (F12: evaluation failures are abstentions, never retries).
            EffectState::Failed | EffectState::Unconfirmed => {
                return self.eval_abstain(&launch.id, AbstainReason::EvaluationFailed);
            }
            EffectState::Acknowledged => {}
        }
        let record = match &eval.receipt {
            Some(EffectReceipt::Judgments(record)) => record.clone(),
            _ => return self.eval_abstain(&launch.id, AbstainReason::EvaluationFailed),
        };
        let asked = asked_tabs(&record);
        let evaluation = match validate_evaluation(&record, &self.loaded.config.policy, &asked) {
            Ok(evaluation) => evaluation,
            Err(reason) => return self.eval_abstain(&launch.id, reason),
        };
        if let Some(outcome) = evaluation_verdict(&evaluation) {
            return self.finish_evaluating(&launch.id, &outcome);
        }
        let predecessor = launch
            .task
            .recovery_of
            .as_ref()
            .and_then(|id| self.store.run(id).ok().flatten());
        // A failed read would silently under-constrain routing — the
        // cooldown is a safety bound, so a skipped pass (the tick's
        // converge row retries) is the honest answer, never an empty set.
        let Ok(cooldowns) = self.store.cooldowns() else {
            return;
        };
        let cooling = cooling_down(&cooldowns, now);
        // `required`/`qualifications` stay empty: the `[policy.
        // required_capabilities]` table never landed in the shipped
        // catalog schema (OQ-Q was decided in the plan but no field
        // exists), so the honest derivation is "nothing required" — no
        // qualification row can be consumed either.
        match route(
            &launch,
            predecessor.as_ref(),
            &evaluation,
            &self.loaded.config,
            &[],
            &[],
            &cooling,
        ) {
            Err(reason) => self.eval_abstain(&launch.id, reason),
            Ok(decision) => self.decide(&launch, &decision, &evaluation, now),
        }
    }
}
