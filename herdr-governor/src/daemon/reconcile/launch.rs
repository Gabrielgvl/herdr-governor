//! `launch` — §4.7 step 0, the launch convergence's store-visible rows
//! (§4.5): `launching` Launch rows reconciled against the journaled
//! effects and the owning Run before any observation is derived. The
//! `routed`-phase rows that resolve the caller's pane in the fresh
//! snapshot are P5.B2's (`launch/convergence.rs`); they slot in here
//! ahead of step 1.

use governor_core::config::Policy;
use governor_core::identity::Timestamp;
use governor_core::lifecycle::{
    CreatedTopology, Effect, EffectCertainty, EffectKind, EffectReceipt, EffectState, Run, State,
    Transition,
};
use governor_core::task::{Launch, LaunchOutcome, LaunchPhase, finish};

use crate::daemon::DaemonError;
use crate::daemon::coordinator::apply::apply_with_retry;
use crate::daemon::coordinator::empty;
use crate::store::Store;

/// The `createdTopology` the failed outcome reports: every `TabCreated`/
/// `PaneCreated` receipt journaled `acknowledged` on the Run.
fn created_topology(journal: &[Effect]) -> CreatedTopology {
    let mut topology = CreatedTopology {
        tab: None,
        panes: Vec::new(),
    };
    for effect in journal {
        if effect.state != EffectState::Acknowledged {
            continue;
        }
        match &effect.receipt {
            Some(EffectReceipt::TabCreated { tab, pane }) => {
                topology.tab = Some(tab.clone());
                topology.panes.push(pane.clone());
            }
            Some(EffectReceipt::PaneCreated { pane }) => {
                topology.panes.push(pane.clone());
            }
            _ => {}
        }
    }
    topology
}

/// A topology or start effect — the launch leg §4.5's rows reason over.
fn is_launch_leg(effect: &Effect) -> bool {
    matches!(
        effect.kind,
        EffectKind::TabCreate | EffectKind::PaneSplit | EffectKind::AgentStart
    )
}

/// The F20 certainty a terminated launch reports: `unconfirmed` rows (or
/// any `unknown` certainty already journaled) mean *unknown*; otherwise
/// provable absence.
fn launch_certainty(journal: &[Effect]) -> EffectCertainty {
    let unknown = journal.iter().any(|effect| {
        effect.state == EffectState::Unconfirmed
            || effect.certainty == Some(EffectCertainty::Unknown)
    });
    if unknown {
        EffectCertainty::Unknown
    } else {
        EffectCertainty::Absent
    }
}

/// §4.5 convergence for one `launching` Launch — the rows decidable from
/// the store alone:
///
/// * Run `settled` → `finish(Failed{certainty, run, createdTopology})`
///   (the run ended while the launch row stayed open) once no launch leg
///   is still `dispatching`;
/// * Run ≥ `prompting` → `finish(Launched{…})`, idempotent;
/// * Run `starting` with every topology/start leg terminal and at least
///   one failed/unconfirmed → `finish(Failed{certainty, run, …})` — the
///   Run itself still waits for obs(absent) or max age;
/// * Run `starting`/`reserved` mid-leg → nothing (the leg decides).
///
/// The `routed`-phase rows resolve the caller's pane in the fresh
/// snapshot — `launch/convergence.rs` (P5.B2) owns them; they slot in
/// here ahead of step 1.
fn converge_launch(
    launch: &Launch,
    run: &Run,
    journal: &[Effect],
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    match run.state {
        // A leg still `dispatching` may yet create topology — wait for its
        // result (a restart marks it `unconfirmed` → `unknown`) so the
        // outcome's certainty and `createdTopology` are not guessed. A
        // `planned` leg never dispatches for a settled Run (the commit
        // gate skips it): provably absent, nothing to wait for.
        State::Settled
            if journal.iter().any(|effect| {
                is_launch_leg(effect) && effect.state == EffectState::Dispatching
            }) =>
        {
            empty()
        }
        State::Settled => finish(
            launch,
            LaunchOutcome::Failed {
                certainty: launch_certainty(journal),
                run: Some(run.id.clone()),
                created_topology: created_topology(journal),
            },
            None,
            None,
            now,
            policy,
        ),
        // The restart convergence of the start-ack compose — the same
        // outcome (`requestedOperatingPointId` after a fallback, F15).
        State::Prompting | State::Active | State::Judging | State::Repair => {
            let Some(outcome) = crate::daemon::launch::launched(launch, run) else {
                return empty();
            };
            finish(launch, outcome, None, None, now, policy)
        }
        State::Starting => {
            let mut failed = None;
            for effect in journal.iter().filter(|effect| is_launch_leg(effect)) {
                match effect.state {
                    // A leg still in flight — the launch is healthy, wait.
                    EffectState::Planned | EffectState::Dispatching => return empty(),
                    EffectState::Failed | EffectState::Unconfirmed => {
                        failed = Some(effect);
                    }
                    EffectState::Acknowledged => {}
                }
            }
            let Some(leg) = failed else {
                return empty();
            };
            let certainty = if leg.state == EffectState::Unconfirmed {
                EffectCertainty::Unknown
            } else {
                leg.certainty.unwrap_or(EffectCertainty::Unknown)
            };
            finish(
                launch,
                LaunchOutcome::Failed {
                    certainty,
                    run: Some(run.id.clone()),
                    created_topology: created_topology(journal),
                },
                None,
                None,
                now,
                policy,
            )
        }
        // `launch_plan`'s writes are atomic with `begin`'s — a `launching`
        // Launch over a `reserved` Run can't exist; the `routed` row is
        // P5.B2's (`launch/convergence.rs`).
        State::Reserved => empty(),
    }
}

/// §4.7 step 0 — convergence across the `launching` rows. Each launch's
/// apply recomputes against fresh reads so a phase that moved mid-pass
/// writes nothing.
pub(super) fn converge(
    store: &mut Store,
    policy: &Policy,
    now: Timestamp,
) -> Result<(), DaemonError> {
    for launch in store.launches_in_phase(LaunchPhase::Launching)? {
        let Some(run_id) = store.run_by_launch(&launch.id)?.map(|run| run.id) else {
            continue;
        };
        apply_with_retry(store, now, |st| {
            let Some(current) = st.launch(&launch.id).ok().flatten() else {
                return empty();
            };
            if current.phase != LaunchPhase::Launching {
                return empty();
            }
            let Some(run) = st.run(&run_id).ok().flatten() else {
                return empty();
            };
            let journal = st.journal(&run_id).unwrap_or_default();
            converge_launch(&current, &run, &journal, now, policy)
        })?;
    }
    Ok(())
}
