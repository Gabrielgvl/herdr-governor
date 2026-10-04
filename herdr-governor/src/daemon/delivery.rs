//! `delivery` — §4.8 (F9/F17/F18): the shared `linked_outbox_resolution`
//! the effect-result and restart paths compose (C2 owns it — B3's
//! `reconcile::outbox` private copy is replaced, never kept in step),
//! the `follow_up_expired` events every `ExpireFollowUps` write owes in
//! the same transaction, and the §4.7 steps 5–6 tick pass: follow-up
//! dispatch planning, subject-independent hint planning (`hints/`), and
//! the published-body retention sweep (`publish/` holds publication).

mod hints;
mod publish;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use governor_core::config::{Capability, Catalog, OperatingPoint};
use governor_core::delivery::{
    FollowUpWrite, MailboxEvent, MailboxEventKind, MailboxSubject, MessageBody, OutboxState,
    follow_up_effect, follow_up_file_retained, next_dispatchable_follow_up,
};
use governor_core::identity::{
    CallerKey, EffectId, EffectKey, EventId, Observation, RunId, Timestamp, classify,
};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectOutcome, EffectTarget, StateChange, Transition, op_digest,
};

use crate::daemon::coordinator::apply::apply_with_retry;
use crate::daemon::paths::Paths;
use crate::daemon::reconcile::SnapshotView;
use crate::store::{ApplyError, Store};

use super::DaemonError;

pub(super) use publish::publish_body;

/// Parse `run:<id>:outbox:<seq>` — the F9/F17 journal-key convention.
/// `rsplit_once` keeps run ids containing `:` honest.
fn outbox_key(key: &EffectKey) -> Option<(RunId, u64)> {
    let rest = key.0.strip_prefix("run:")?;
    let (run, seq) = rest.rsplit_once(":outbox:")?;
    Some((RunId(run.to_owned()), seq.parse().ok()?))
}

/// For an outbox-linked effect whose `outcome` is terminal, the
/// `WriteFollowUp::Resolve` the same transaction owes plus — on
/// `unconfirmed` — the `follow_up_unconfirmed` event. The row's own
/// `resolve_dispatch` carries the state rule (a non-`dispatching` row
/// resolves to `None`, so a restart that finds the entry already
/// resolved writes nothing and emits nothing). The shared helper both
/// the §4.4 result path and the §4.3 step-5 restart marking call.
pub(in crate::daemon) fn linked_outbox_resolution(
    store: &Store,
    effect: &Effect,
    outcome: EffectOutcome,
) -> Option<(StateChange, Option<MailboxEvent>)> {
    let (run, seq) = outbox_key(&effect.key)?;
    let resolved = store
        .outbox(&run)
        .ok()?
        .into_iter()
        .find(|message| message.seq == seq)?
        .resolve_dispatch(outcome)?;
    let write = StateChange::WriteFollowUp(FollowUpWrite::Resolve {
        run: run.clone(),
        seq,
        state: resolved.state,
    });
    let event = if resolved.state == OutboxState::Unconfirmed {
        let subject = MailboxSubject::Run(run);
        let dedup = MailboxEventKind::FollowUpUnconfirmed.dedup_key(&subject, Some(seq))?;
        MailboxEvent::emitted(
            EventId(format!("evt:{}", dedup.0)),
            subject,
            MailboxEventKind::FollowUpUnconfirmed,
            Some(seq),
            format!("{{\"seq\":{seq}}}"),
        )
    } else {
        None
    };
    Some((write, event))
}

/// The `follow_up_expired` events an `ExpireFollowUps` write owes — one
/// per `queued` entry the write is about to expire, qualifier `seq`
/// (`run:<id>:follow_up_expired:<seq>` dedups a repeated emission).
/// Composed into the transition *before* it applies so the events and
/// the expiry land in one transaction; a read failure aborts the apply
/// (`apply_with_retry` maps it to `Err`) rather than silently dropping
/// an owner-facing notification.
pub(in crate::daemon) fn expiry_events(
    store: &Store,
    transition: &Transition,
) -> Result<Vec<MailboxEvent>, ApplyError> {
    let mut events = Vec::new();
    for change in &transition.state_changes {
        let StateChange::ExpireFollowUps { run, reason } = change else {
            continue;
        };
        for message in store.outbox(run)? {
            if message.state != OutboxState::Queued {
                continue;
            }
            let subject = MailboxSubject::Run(run.clone());
            let Some(dedup) =
                MailboxEventKind::FollowUpExpired.dedup_key(&subject, Some(message.seq))
            else {
                continue;
            };
            if let Some(event) = MailboxEvent::emitted(
                EventId(format!("evt:{}", dedup.0)),
                subject,
                MailboxEventKind::FollowUpExpired,
                Some(message.seq),
                format!(
                    "{{\"seq\":{},\"reason\":\"{}\"}}",
                    message.seq,
                    reason.as_str()
                ),
            ) {
                events.push(event);
            }
        }
    }
    Ok(events)
}

/// §4.7 steps 5–6 — the delivery pass on one tick. Step 5 plans a
/// `run:<id>:outbox:<seq>` prompt effect for every Run whose outbox head
/// is dispatchable (`next_dispatchable_follow_up` on durable state —
/// settlement, the F9 barrier, the in-flight slot, `blocked`, and the
/// `mid_turn_input` qualification). Step 6 plans subject-independent
/// hints on the tick's fresh snapshot and runs the retention sweep.
/// Both apply through `apply_with_retry` (`insert_dedup` dedups
/// replans); a hard error aborts the pass like `reconcile::pass` — the
/// next tick resumes.
pub(in crate::daemon) fn pass(
    store: &mut Store,
    catalog: &Catalog,
    view: Option<&SnapshotView>,
    absent_since: &mut BTreeMap<RunId, Timestamp>,
    last_hint_at: &mut BTreeMap<CallerKey, Timestamp>,
    paths: &Paths,
    now: Timestamp,
) -> Result<(), DaemonError> {
    apply_with_retry(store, now, |st| dispatch_transition(st, catalog))?;
    let Some(live) = view else {
        // A failed snapshot leaves dispatch planning running (it reads
        // persisted state only); hints need a fresh read and the sweep a
        // valid classification — both wait for the next tick.
        return Ok(());
    };
    let mut hinted = Vec::new();
    apply_with_retry(store, now, |st| {
        let planned = hints::plan(st, catalog, live, last_hint_at, now);
        hinted = planned.iter().map(|(owner, _)| owner.clone()).collect();
        Transition {
            state_changes: Vec::new(),
            events: Vec::new(),
            effects: planned.into_iter().map(|(_, effect)| effect).collect(),
        }
    })?;
    // The rate limit stamps whichever owners the last recompute planned —
    // Applied or Dropped alike (a dropped plan re-attempts after the
    // interval, never in a hot loop).
    for owner in hinted {
        last_hint_at.insert(owner, now);
    }
    sweep(store, live, absent_since, paths, now)
}

/// Step 5's recompute: the `planned` prompt effect each Run with queued
/// entries earns this pass. `outbox_pending` supplies the candidate set;
/// the full outbox + journal feed `next_dispatchable_follow_up`. A Run
/// with no captured identity has nowhere to send — the entry waits.
fn dispatch_transition(store: &Store, catalog: &Catalog) -> Transition {
    let mut effects = Vec::new();
    let mut seen = BTreeSet::new();
    for message in store.outbox_pending().unwrap_or_default() {
        if !seen.insert(message.run.clone()) {
            continue;
        }
        let Ok(Some(run)) = store.run(&message.run) else {
            continue;
        };
        let Ok(outbox) = store.outbox(&run.id) else {
            continue;
        };
        let journal = store.journal(&run.id).unwrap_or_default();
        let qualified = run_caps(store, catalog, &run);
        let Some(head) = next_dispatchable_follow_up(&run, &outbox, &journal, &qualified) else {
            continue;
        };
        let Some(identity) = run.identity.clone() else {
            continue;
        };
        let key = format!("run:{}:outbox:{}", run.id.0, head.seq);
        let target = EffectTarget::Child(identity.clone());
        effects.push(follow_up_effect(
            head,
            identity,
            EffectId(format!("eff:{key}")),
            op_digest(EffectKind::Prompt, Some(&target), key.as_bytes()),
        ));
    }
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects,
    }
}

/// The Run's operating-point capabilities carrying a current F26 pass —
/// `mid_turn_input` (F17's send-immediately rule) reads this set.
fn run_caps(
    store: &Store,
    catalog: &Catalog,
    run: &governor_core::lifecycle::Run,
) -> Vec<Capability> {
    let Some(point) = run.operating_point.as_ref().and_then(|id| {
        catalog
            .operating_points
            .iter()
            .find(|point| point.id == *id)
    }) else {
        return Vec::new();
    };
    point_caps(store, point)
}

/// The capabilities `point` currently offers — its claims ∩ the passed
/// `qualifications` rows keyed by the current args digest (F26).
fn point_caps(store: &Store, point: &OperatingPoint) -> Vec<Capability> {
    point
        .capabilities
        .iter()
        .filter(|capability| {
            store
                .qualification(&point.id, point.args_digest(), capability)
                .ok()
                .flatten()
                .is_some_and(|q| point.has_current_pass(capability, &[q]))
        })
        .cloned()
        .collect()
}

/// §4.8's retention sweep — reap a published body file only when
/// `follow_up_file_retained` releases it: an `expired` message's file
/// immediately, a `submitted`/`unconfirmed` (possibly consumed) file
/// `FOLLOWUP_FILE_RETENTION` after the child's identity was first
/// observed `absent`. `absent_since` is the coordinator's in-memory
/// clock — a restart re-arms the window, the stated ceiling
/// (retention only lengthens). Tmp artifacts and files no outbox row
/// names are crash debris — the publish → enqueue path is atomic, so an
/// unreferenced file can only be a leftover; they always reap. Fail-
/// closed: an fs error aborts the pass rather than guessing. A missing
/// `followups/` is the common case — nothing was ever published, so
/// nothing needs sweeping.
fn sweep(
    store: &Store,
    view: &SnapshotView,
    absent_since: &mut BTreeMap<RunId, Timestamp>,
    paths: &Paths,
    now: Timestamp,
) -> Result<(), DaemonError> {
    let entries = match fs::read_dir(paths.followups()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for dir_entry in entries {
        let dir = dir_entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(dir_name) = dir.file_name().and_then(|n| n.to_str().map(str::to_owned)) else {
            continue;
        };
        let run_id = RunId(dir_name);
        // The absent clock — `Invalid` leaves a stamp standing
        // (ambiguity favors retention); `Unique` clears it: a child
        // back on screen is live, not absent.
        match child_observation(store, &run_id, view) {
            Some(Observation::Absent) => {
                absent_since.entry(run_id.clone()).or_insert(now);
            }
            Some(Observation::Unique { .. }) => {
                absent_since.remove(&run_id);
            }
            _ => {}
        }
        let absent = absent_since.get(&run_id).copied();
        let outbox = store.outbox(&run_id)?;
        for file_entry in fs::read_dir(&dir)? {
            let file = file_entry?.path();
            if !file.is_file() {
                continue;
            }
            let Some(base) = file.file_name().and_then(|n| n.to_str().map(str::to_owned)) else {
                continue;
            };
            if Path::new(&base)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
            {
                fs::remove_file(&file)?;
                continue;
            }
            let Some(seq) = base
                .strip_suffix(".md")
                .and_then(|text| text.parse::<u64>().ok())
            else {
                continue;
            };
            let keep = outbox.iter().any(|message| {
                message.seq == seq
                    && matches!(&message.body, MessageBody::File { path } if Path::new(path) == file)
                    && follow_up_file_retained(message.state, absent, now)
            });
            if !keep {
                fs::remove_file(&file)?;
            }
        }
    }
    Ok(())
}

/// The Run's child classification under `view` — the same `classify`
/// the reconcile pass runs — or `None` when the Run is gone or carries
/// no captured identity to classify.
fn child_observation(store: &Store, run: &RunId, view: &SnapshotView) -> Option<Observation> {
    let identity = store.run(run).ok().flatten()?.identity?;
    Some(classify(&identity, Some(view.incarnation()), view.agents()))
}
