//! `hints` — §4.7 step 6 / F18: subject-independent hint planning. Each
//! unacked, unhinted mailbox event's destination owner — the Run's
//! current owner for a Run subject, the Launch's caller for a Launch
//! subject — is re-derived per event, so adoption redirects hints just
//! like the mailbox read does. The `event:<id>:hint` effect key is the
//! journal's once-only rule: a hint is never retried, never gated by the
//! child's follow-up queue, and lands on the owner's own pane — a
//! different capture than the Run's child identity.

use std::collections::BTreeMap;

use governor_core::config::{Capability, Catalog};
use governor_core::delivery::{MailboxEvent, MailboxSubject, hint_effect, hint_eligible};
use governor_core::identity::{CallerKey, EffectId, Observation, Timestamp};
use governor_core::lifecycle::{Effect, EffectKind, EffectTarget, op_digest};

use crate::daemon::identity::AgentRow;
use crate::daemon::reconcile::SnapshotView;
use crate::store::Store;

/// Every `(owner, effect)` this pass plans: at most one per owner — the
/// per-owner interval binds inside the batch too, so a same-owner event
/// behind a just-planned hint waits the full interval, not one pass.
/// `view` is the tick's own fresh read — a snapshot with duplicate pane
/// locators hints no one (`valid` is the F3 guard). `last_hint_at` is
/// read, not written: the caller stamps the owners it journaled, so a
/// CAS re-run derives the same transition — which is why `stamps` must
/// stay a local overlay rather than a write-through.
pub(super) fn plan(
    store: &Store,
    catalog: &Catalog,
    view: &SnapshotView,
    last_hint_at: &BTreeMap<CallerKey, Timestamp>,
    now: Timestamp,
) -> Vec<(CallerKey, Effect)> {
    if !view.valid() {
        return Vec::new();
    }
    let mut stamps = last_hint_at.clone();
    let mut planned = Vec::new();
    for event in store.mailbox_unhinted().unwrap_or_default() {
        if let Some((owner, effect)) = plan_one(store, catalog, view, &stamps, now, &event) {
            stamps.insert(owner.clone(), now);
            planned.push((owner, effect));
        }
    }
    planned
}

/// One event's hint — `None` when the owner resolves to nothing
/// eligible (absent/ambiguous pane, busy, unqualified, or inside the
/// rate interval). The effect binds the event's subject so dispatch
/// re-derives the destination owner fresh.
fn plan_one(
    store: &Store,
    catalog: &Catalog,
    view: &SnapshotView,
    last_hint_at: &BTreeMap<CallerKey, Timestamp>,
    now: Timestamp,
    event: &MailboxEvent,
) -> Option<(CallerKey, Effect)> {
    let owner = owner_of(store, event)?;
    let observation = owner_observation(&owner, view.agents());
    let qualified = owner_caps(store, catalog, &owner);
    let pane = hint_eligible(
        &owner,
        &observation,
        &qualified,
        last_hint_at.get(&owner).copied(),
        now,
    )?;
    let key = format!("event:{}:hint", event.id.0);
    let target = EffectTarget::CallerContext(pane.clone());
    let effect = hint_effect(
        event,
        pane,
        EffectId(format!("eff:{key}")),
        op_digest(EffectKind::Prompt, Some(&target), key.as_bytes()),
    );
    Some((owner, effect))
}

/// The event's destination owner: the Run's *current* owner (adoption
/// moves it — H#84) or the Launch's caller; `None` when the subject row
/// is gone.
fn owner_of(store: &Store, event: &MailboxEvent) -> Option<CallerKey> {
    match &event.subject {
        MailboxSubject::Run(run) => store.run(run).ok().flatten().map(|row| row.owner),
        MailboxSubject::Launch(launch) => store.launch(launch).ok().flatten().map(|row| row.caller),
    }
}

/// The owner as an `Observation`: `Unique` when exactly one agent row
/// still holds the owner's `(agent_kind, native_session)`, `Invalid`
/// when several do (a duplicated session is untrustworthy), `Absent`
/// when none does.
fn owner_observation(owner: &CallerKey, agents: &[AgentRow]) -> Observation {
    let mut matching = agents.iter().filter(|row| {
        row.2.as_ref() == Some(&owner.agent_kind) && row.4.as_ref() == Some(&owner.native_session)
    });
    let Some(row) = matching.next() else {
        return Observation::Absent;
    };
    if matching.next().is_some() {
        return Observation::Invalid;
    }
    Observation::Unique {
        status: row.5,
        pane: row.0.clone(),
        native_session: row.4.clone(),
    }
}

/// `hint_consumption` (and every other cap) the owner's harness offers —
/// the union over catalog points matching `agent_kind`, each filtered to
/// its current F26 passes.
fn owner_caps(store: &Store, catalog: &Catalog, owner: &CallerKey) -> Vec<Capability> {
    catalog
        .operating_points
        .iter()
        .filter(|point| point.harness == owner.agent_kind)
        .flat_map(|point| super::point_caps(store, point))
        .collect()
}
