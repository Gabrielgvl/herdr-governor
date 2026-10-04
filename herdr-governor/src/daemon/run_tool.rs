//! `run_tool` — `herdr_run` (F6): the run-scoped actions. `observe`
//! pages the Run's outbox by `seq` under an opaque cursor alongside its
//! state, settlement, handoff and per-item acceptance; `message` is
//! F17's owner-bound follow-up admission; `ack` stamps a destined
//! mailbox event idempotently; `handover`/`adopt` run F4/F19's
//! ownership moves against the request-time F1 snapshot; `cancel`
//! settles through `Event::Cancel` and — for `closePane` — parks the
//! reply behind the verified close's confirmation (F20).

/// `message` — F17's follow-up admission.
mod message;
/// `observe` — the F6 Run page.
mod observe;

use governor_core::config::Policy;
use governor_core::identity::{
    CallerKey, EffectKey, EventId, PaneId, RunId, Timestamp, plan_adoption, plan_handover,
    require_owner,
};
use governor_core::lifecycle::{
    Effect, EffectState, Event, Run, StateChange, Transition, transition,
};
use governor_core::task::Refusal;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::daemon::api::{RunAction, ToolError, ToolResponse};
use crate::daemon::coordinator::apply::apply_with_retry;
use crate::daemon::coordinator::{Coordinator, Msg, empty, versioned};
use crate::daemon::identity::AgentRow;
use crate::daemon::launch::Admission;
use crate::daemon::paths::Paths;
use crate::store::Store;

/// The `herdr_run` dispatch — the Tool arm calls this once the caller is
/// bound (F1). `agents` is that same request-time snapshot's row set:
/// handover's successor resolution and adoption's owner-liveness proof
/// run the F1 rules against it, never a cached view.
pub(in crate::daemon) fn call(
    store: &mut Store,
    paths: &Paths,
    caller: &CallerKey,
    action: RunAction,
    agents: &[AgentRow],
    now: Timestamp,
    policy: &Policy,
) -> Admission {
    match action {
        RunAction::Observe { run, cursor } => {
            Admission::Answer(observe::page(store, caller, &run, cursor.as_deref()))
        }
        RunAction::Message { run, key, text } => Admission::Answer(message::enqueue(
            store, paths, caller, &run, &key, &text, now,
        )),
        RunAction::Ack { event } => Admission::Answer(ack(store, caller, &event, now)),
        RunAction::Handover {
            runs,
            successor_pane,
        } => Admission::Answer(handover(store, caller, &runs, &successor_pane, agents, now)),
        RunAction::Adopt { runs } => Admission::Answer(adopt(store, caller, &runs, agents, now)),
        RunAction::Cancel { run, close_pane } => {
            cancel(store, caller, &run, close_pane, now, policy)
        }
    }
}

/// F18 — `ack {eventId}`: stamps `acked_at` on a mailbox event destined
/// for this caller — a store-level once-write (`acked_at IS NULL`
/// guards it), so a repeat is a clean idempotent `acked`. An absent or
/// foreign event answers `NOT_OWNER`: ack must never leak another
/// caller's events, not even their existence (F4).
fn ack(store: &mut Store, caller: &CallerKey, event: &EventId, now: Timestamp) -> ToolResponse {
    let destined = store
        .mailbox_event_destined_to(caller, event)
        .map_err(|_err| unavailable())?;
    if destined.is_none() {
        return not_owner();
    }
    apply_with_retry(store, now, |st| {
        if st
            .mailbox_event_destined_to(caller, event)
            .ok()
            .flatten()
            .is_none()
        {
            return empty();
        }
        Transition {
            state_changes: Vec::from([StateChange::AckEvent(event.clone())]),
            events: Vec::new(),
            effects: Vec::new(),
        }
    })
    .map_err(|_apply| unavailable())?;
    Ok(json!({"eventId": event.0, "acked": true}))
}

/// F4/F6 — `handover {runIds, successorPaneId}`: the caller owns every
/// named Run (F4), the successor resolves under the request-time F1
/// snapshot, and a Run never goes to its own child (`CALLER_IS_RUN`).
/// The named set moves atomically — one transaction of `ChangeOwner`
/// writes, each conditional on the owner the plan read.
fn handover(
    store: &mut Store,
    caller: &CallerKey,
    runs: &[RunId],
    successor_pane: &PaneId,
    agents: &[AgentRow],
    now: Timestamp,
) -> ToolResponse {
    let mut refused = None;
    let applied = apply_with_retry(store, now, |st| {
        let mut planned = empty();
        for run_id in runs {
            let Some(run) = st.run(run_id).ok().flatten() else {
                refused = Some(Refusal::NotOwner);
                return empty();
            };
            match plan_handover(&run, caller, successor_pane, agents) {
                Ok(next) => concat_into(&mut planned, next),
                Err(refusal) => {
                    refused = Some(refusal);
                    return empty();
                }
            }
        }
        planned
    });
    if let Some(refusal) = refused {
        return Err(ToolError::refusal(refusal, "handover refused"));
    }
    applied.map_err(|_apply| unavailable())?;
    Ok(owned_body(store, runs))
}

/// F19 — `adopt {runIds}`: the request-time snapshot must show the prior
/// owner's native session gone (`ADOPT_OWNER_LIVE` while it lives); an
/// unsettled Run is adoptable, a settled one only for unread events or a
/// pending recovery — never re-opened, never re-keyed. The set moves
/// atomically like handover.
fn adopt(
    store: &mut Store,
    caller: &CallerKey,
    runs: &[RunId],
    agents: &[AgentRow],
    now: Timestamp,
) -> ToolResponse {
    let mut refused = None;
    let applied = apply_with_retry(store, now, |st| {
        let mut planned = empty();
        for run_id in runs {
            let Some(run) = st.run(run_id).ok().flatten() else {
                refused = Some(Refusal::NotOwner);
                return empty();
            };
            let has_unread = !st
                .unacked_events_for_run(run_id)
                .unwrap_or_default()
                .is_empty();
            let has_recovery = st.pending_recovery_for_run(run_id).ok().flatten().is_some();
            match plan_adoption(&run, caller, agents, has_unread, has_recovery) {
                Ok(next) => concat_into(&mut planned, next),
                Err(refusal) => {
                    refused = Some(refusal);
                    return empty();
                }
            }
        }
        planned
    });
    if let Some(refusal) = refused {
        return Err(ToolError::refusal(refusal, "adoption refused"));
    }
    applied.map_err(|_apply| unavailable())?;
    Ok(owned_body(store, runs))
}

/// F20 — `cancel {runId, closePane?}`: an unsettled Run settles
/// `cancelled` in the same `Event::Cancel` transition; `closePane`
/// plans the one verified `run:<id>:close`. The reply reports the close
/// confirmation — a still-planned or in-flight close parks the reply
/// behind `close_waiters` until its result commits or the wait bound
/// answers for it; a Run with no captured identity has nothing to
/// verify against and reports `none`.
fn cancel(
    store: &mut Store,
    caller: &CallerKey,
    run_id: &RunId,
    close_pane: bool,
    now: Timestamp,
    policy: &Policy,
) -> Admission {
    let mut refused = None;
    let applied = apply_with_retry(store, now, |st| {
        let Some(run) = st.run(run_id).ok().flatten() else {
            refused = Some(Refusal::NotOwner);
            return empty();
        };
        if let Err(refusal) = require_owner(&run, caller) {
            refused = Some(refusal);
            return empty();
        }
        let journal = st.journal(&run.id).unwrap_or_default();
        transition(
            &run,
            &versioned(&run, Event::Cancel { close_pane }),
            now,
            policy,
            (None, journal.as_slice(), &[]),
            "",
        )
    });
    if let Some(refusal) = refused {
        return Admission::Answer(Err(ToolError::refusal(refusal, "cancel refused")));
    }
    if let Err(_apply) = applied {
        return Admission::Answer(Err(unavailable()));
    }
    let run = store.run(run_id).ok().flatten();
    if !close_pane {
        return Admission::Answer(Ok(cancel_body(run.as_ref(), None)));
    }
    let key = close_key(run_id);
    match store.effect(&key).ok().flatten() {
        Some(row) if matches!(row.state, EffectState::Planned | EffectState::Dispatching) => {
            Admission::ParkClose(key)
        }
        row => Admission::Answer(Ok(cancel_body(run.as_ref(), row.as_ref()))),
    }
}

/// The `run:<id>:close` journal key — the same derivation `effect_key`
/// produces for the F10 close (F20's dedup).
fn close_key(run_id: &RunId) -> EffectKey {
    EffectKey(format!("run:{}:close", run_id.0))
}

impl Coordinator {
    /// Park the `tools/call` reply behind this close effect's waiters —
    /// F20's confirmation wait. The `CloseWait` bound answers the row's
    /// current state; one bound per key (a second reply rides the same
    /// timer like `park_launch_waiter`).
    pub(in crate::daemon) fn park_close_waiter(
        &mut self,
        key: &EffectKey,
        reply: oneshot::Sender<ToolResponse>,
    ) {
        let waiters = self.close_waiters.entry(key.clone()).or_default();
        let first = waiters.is_empty();
        waiters.push(reply);
        if !first {
            return;
        }
        let Some(env) = self.runner.as_ref() else {
            return;
        };
        let tx = env.tx.clone();
        let wait = self.daemon.launch_wait;
        let armed = key.clone();
        drop(tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _dropped = tx.send(Msg::CloseWait { key: armed }).await;
        }));
    }

    /// The `CloseWait` arm — the bound elapsed: parked replies resolve
    /// with whatever the close row now shows.
    pub(in crate::daemon) fn close_wait_expired(&mut self, key: &EffectKey) {
        self.drain_close_waiters(key);
    }

    /// The post-commit drain for every arm that moves an effect to a
    /// state worth answering on: the result arm, the governor-refused
    /// commit and the `CloseWait` bound all land here. A row still
    /// `planned`/`dispatching` answers `confirmed: false` honestly.
    pub(in crate::daemon) fn drain_close_waiters(&mut self, key: &EffectKey) {
        let Some(senders) = self.close_waiters.remove(key) else {
            return;
        };
        let row = self.store.effect(key).ok().flatten();
        let run = row
            .as_ref()
            .and_then(|effect| effect.subject_run.as_ref())
            .and_then(|run_id| self.store.run(run_id).ok().flatten());
        let body = cancel_body(run.as_ref(), row.as_ref());
        for sender in senders {
            let _gone = sender.send(Ok(body.clone()));
        }
    }
}

/// The `cancel` reply: the Run's settlement plus — when `closePane` was
/// asked — the close row's state and whether it is `confirmed` (an
/// acknowledged close; `none` when no close effect exists).
fn cancel_body(run: Option<&Run>, close: Option<&Effect>) -> Value {
    let mut body = json!({
        "runId": run.map(|r| r.id.0.clone()),
        "settlement": run.and_then(|r| r.settlement.map(|s| s.as_str())),
        "settlementReason": run.and_then(observe::settlement_reason),
    });
    if let (Some(effect), Some(object)) = (close, body.as_object_mut()) {
        object.insert(
            "close".into(),
            json!({
                "key": effect.key.0,
                "state": effect.state.as_str(),
                "confirmed": effect.state == EffectState::Acknowledged,
            }),
        );
    }
    body
}

/// The `handover`/`adopt` reply — the moved set with each Run's new
/// `owner_generation` (Appendix B's atomic bump, reported).
fn owned_body(store: &Store, runs: &[RunId]) -> Value {
    json!({
        "runs": runs.iter().map(|run_id| {
            json!({
                "runId": run_id.0,
                "ownerGeneration": store
                    .run(run_id)
                    .ok()
                    .flatten()
                    .map(|run| run.owner_generation),
            })
        }).collect::<Vec<_>>(),
    })
}

/// The caller-visible `NOT_OWNER`: a Run the store does not know has no
/// owner either — the same refusal keeps existence out of the answer
/// (F4).
fn read_run(store: &Store, run_id: &RunId, caller: &CallerKey) -> Result<Option<Run>, ToolError> {
    let stored = store.run(run_id).map_err(|_err| unavailable())?;
    match stored {
        Some(run) if require_owner(&run, caller).is_ok() => Ok(Some(run)),
        Some(_) | None => Ok(None),
    }
}

fn not_owner() -> ToolResponse {
    Err(ToolError::refusal(
        Refusal::NotOwner,
        "the caller does not own this run",
    ))
}

fn unavailable() -> ToolError {
    ToolError::new(ToolError::DAEMON_UNAVAILABLE, "store read failed")
}

fn concat_into(planned: &mut Transition, next: Transition) {
    planned.state_changes.extend(next.state_changes);
    planned.events.extend(next.events);
    planned.effects.extend(next.effects);
}
