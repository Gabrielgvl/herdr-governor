//! `reconcile` — §4.7 steps 0–2 and 8, and the §4.3 steps 6–7 startup
//! pass they serve: launch convergence → observations (with the revised
//! identity-less absence rule and the sessionless foreign-incarnation
//! settlement) → the deadline sweep, all computed as `Transition`s and
//! applied through `apply_with_retry` on the coordinator's store.
//! `launch/` holds step 0's convergence, `subscribe/` step 8's
//! `pane.agent_status_changed` feed, `health/` the freshness record,
//! `outbox/` §4.8's linked resolution. Steps 3–7 (handoff poll,
//! evidence, follow-ups, hints, recoveries) land in `pass` with their
//! owning nodes.

mod health;
mod launch;
mod outbox;
mod subscribe;

#[cfg(test)]
pub(super) use health::HealthState;
pub(super) use health::HerdrHealth;
pub(super) use outbox::linked_outbox_resolution;
pub(super) use subscribe::spawn_subscriptions;

use std::collections::BTreeSet;

use governor_core::config::Policy;
use governor_core::identity::{
    AgentName, HerdrIncarnation, Observation, PaneId, RunId, Timestamp, classify,
};
use governor_core::lifecycle::{
    DeadlineKind, Effect, EffectKind, EffectState, Event, Run, Settlement, State, UnresolvedReason,
    settle, transition,
};

use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::store::Store;

use super::DaemonError;
use super::coordinator::apply::{ApplyOutcome, apply_with_retry};
use super::coordinator::{empty, versioned};
use super::identity::{self, AgentRow};

// — The snapshot view —————————————————————————————————————————————————

/// One answered snapshot reduced to what the reconcile passes consume:
/// the core agent rows (F1/F3's shared `identity::agent_rows` mapping),
/// the incarnation the read arrived under, and the F3 validity flag (a
/// duplicated pane locator makes the whole snapshot untrustworthy —
/// `classify` re-checks this internally; the daemon-side gates need it
/// as a value).
pub(super) struct SnapshotView {
    agents: Vec<AgentRow>,
    incarnation: HerdrIncarnation,
    valid: bool,
}

/// Reduce one `Observed<SessionSnapshot>` to its `SnapshotView`.
pub(super) fn view_of(observed: &Observed<SessionSnapshot>) -> SnapshotView {
    let agents = identity::agent_rows(&observed.value);
    let mut locators = BTreeSet::new();
    let valid = !agents.iter().any(|row| !locators.insert(&row.0));
    SnapshotView {
        agents,
        incarnation: identity::incarnation(&observed.epoch),
        valid,
    }
}

impl SnapshotView {
    /// The agent name reported on `pane`, when the snapshot carries one —
    /// how a `pane.agent_status_changed` event resolves a Run after a
    /// move renamed its pane.
    pub(super) fn name_on(&self, pane: &PaneId) -> Option<&AgentName> {
        self.agents.iter().find(|row| row.0 == *pane)?.3.as_ref()
    }
}

// — The identity-less and sessionless gates ———————————————————————————

/// §4.7's revised identity-less rule (→ F3/F21): `Absent` may be derived
/// for a Run without a captured identity **only** when the launch leg
/// was attempted and terminated — the snapshot is valid, no agent row
/// carries `agent_name == run.child_name`, the journal holds at least one
/// topology/start effect, and every such effect is terminal
/// (`acknowledged`/`failed`/`unconfirmed`). A `reserved` Run has no
/// effects and never qualifies; a `planned`/`dispatching` leg (a slow or
/// next-candidate start) holds the observation back entirely.
pub(super) fn identity_less_absent(
    run: &Run,
    journal: &[Effect],
    agents: &[AgentRow],
    valid: bool,
) -> bool {
    if run.identity.is_some() || !valid {
        return false;
    }
    let name = &run.child_name;
    if agents
        .iter()
        .any(|row| row.3.as_ref().is_some_and(|agent| agent.0 == *name))
    {
        return false;
    }
    let mut any = false;
    let all_terminal = journal
        .iter()
        .filter(|effect| {
            matches!(
                effect.kind,
                EffectKind::TabCreate | EffectKind::PaneSplit | EffectKind::AgentStart
            )
        })
        .all(|effect| {
            any = true;
            matches!(
                effect.state,
                EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed
            )
        });
    any && all_terminal
}

/// §4.3 step 7's precondition: a *valid* snapshot under a known foreign
/// incarnation, for a Run whose captured identity carries no
/// `native_session` to re-prove by — `classify` returns `Invalid` by
/// contract, so the daemon settles `unresolved(identity_unprovable)`
/// itself. Same-incarnation and malformed/unavailable reads never match.
fn sessionless_unprovable(run: &Run, valid: bool, incarnation: &HerdrIncarnation) -> bool {
    if !valid || run.settlement.is_some() {
        return false;
    }
    let Some(identity) = &run.identity else {
        return false;
    };
    identity.native_session.is_none()
        && identity.herdr_incarnation != *incarnation
        && matches!(
            run.state,
            State::Prompting | State::Active | State::Judging | State::Repair
        )
}

/// The observation a Run's row earns under `view`: `classify` for an
/// identity-bearing Run (its own invalid/duplicate handling intact), the
/// attempted-and-terminated `Absent` for an identity-less one, `None`
/// when nothing honest can be derived.
fn observation_for(run: &Run, journal: &[Effect], view: &SnapshotView) -> Option<Observation> {
    if let Some(identity) = &run.identity {
        return Some(classify(
            identity,
            Some(&view.incarnation),
            view.agents.as_slice(),
        ));
    }
    identity_less_absent(run, journal, &view.agents, view.valid).then_some(Observation::Absent)
}

// — §4.7 steps 0–2 ————————————————————————————————————————————————————

/// One reconcile pass against a snapshot that may have failed: step 0's
/// convergence, step 1's observations (skipped wholesale on an
/// unavailable snapshot — `Invalid` is per-run and only reachable inside
/// a read), then step 2's deadline sweep (deadlines fire even while
/// Herdr is down). Per-Run applies — a hard error aborts the pass; at
/// startup that refuses the daemon like `mark_restart` does, on a tick
/// the next pass resumes.
pub(super) fn pass(
    store: &mut Store,
    policy: &Policy,
    now: Timestamp,
    snapshot: &Result<Observed<SessionSnapshot>, HerdrError>,
) -> Result<(), DaemonError> {
    launch::converge(store, policy, now)?;
    if let Ok(observed) = snapshot {
        let view = view_of(observed);
        for run in store.unsettled_runs()? {
            observe_run(store, policy, now, &run.id, &view)?;
        }
    }
    deadline_sweep(store, policy, now)
}

/// §4.7 step 1 for one Run — the `Msg::Observation` path and the per-Run
/// half of `pass`. The sessionless foreign-incarnation settlement of
/// §4.3 step 7 runs here ahead of the observation itself: both are
/// computed from the same store read in one apply.
pub(super) fn observe_run(
    store: &mut Store,
    policy: &Policy,
    now: Timestamp,
    run_id: &RunId,
    view: &SnapshotView,
) -> Result<ApplyOutcome, DaemonError> {
    Ok(apply_with_retry(store, now, |st| {
        let Some(run) = st.run(run_id).ok().flatten() else {
            return empty();
        };
        if sessionless_unprovable(&run, view.valid, &view.incarnation) {
            return settle(
                &run,
                Settlement::Unresolved {
                    reason: UnresolvedReason::IdentityUnprovable,
                },
                now,
                policy,
            );
        }
        let journal = st.journal(run_id).unwrap_or_default();
        let Some(observation) = observation_for(&run, &journal, view) else {
            return empty();
        };
        // C3's one-shot marked-file read wires into this call site (§4.7
        // step 1, F4): `None` is the honest not-written answer until the
        // freeze machinery lands — an `active` × `absent` Run with a
        // frozen handoff still enters `judging` on the journal read.
        let handoffs = st.handoffs(run_id).unwrap_or_default();
        transition(
            &run,
            &versioned(
                &run,
                Event::Obs {
                    observation,
                    handoff_reading: None,
                },
            ),
            now,
            policy,
            (None, journal.as_slice(), handoffs.as_slice()),
            "",
        )
    })?)
}

/// Every armed deadline, attempted in one pass: enum order
/// (`Idle`, `Repair`, `Judgment`, `MaxAge`) so an informative settlement
/// wins over the catastrophic bound when several elapsed together; the
/// core's `on_deadline` re-checks arming and elapse per kind, so an
/// unarmed kind costs one recompute and no transaction.
const DEADLINE_KINDS: [DeadlineKind; 4] = [
    DeadlineKind::Idle,
    DeadlineKind::Repair,
    DeadlineKind::Judgment,
    DeadlineKind::MaxAge,
];

/// §4.7 step 2 — the deadline sweep over every unsettled Run.
fn deadline_sweep(store: &mut Store, policy: &Policy, now: Timestamp) -> Result<(), DaemonError> {
    for run in store.unsettled_runs()? {
        for kind in DEADLINE_KINDS {
            apply_with_retry(store, now, |st| {
                let Some(current) = st.run(&run.id).ok().flatten() else {
                    return empty();
                };
                let journal = st.journal(&current.id).unwrap_or_default();
                transition(
                    &current,
                    &versioned(&current, Event::Deadline(kind)),
                    now,
                    policy,
                    (None, journal.as_slice(), &[]),
                    "",
                )
            })?;
        }
    }
    Ok(())
}
