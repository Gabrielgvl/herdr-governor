//! F22 — the total Appendix C transition function: every `State` against
//! every `Event`, matched exhaustively. This is the dispatch — the `on_*`
//! lanes and the `effect_result` consequences (launch pipeline, prompt,
//! repair) live one per event family under `transition/`.

use crate::acceptance::FrozenHandoff;
use crate::config::Policy;
use crate::identity::Timestamp;
use crate::routing::Decision;

use super::{
    Effect, Event, Run, Settlement, Transition, VersionTriple, Versioned, nothing, settle,
};

mod cancel;
mod deadline;
mod effect_result;
mod handoff;
mod judgment;
mod obs;
mod restart;

use self::{
    cancel::on_cancel, deadline::on_deadline, effect_result::on_effect_result, handoff::on_handoff,
    judgment::on_judgment, obs::on_observation, restart::on_restart,
};

/// F20 — the version stamp a Run reads as "still holding".
fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// F22 — the total Appendix C transition function: every `State` against
/// every `Event`, matched exhaustively. `read` is the persisted context the
/// transition consults beyond the Run row — `(decision, journal, handoffs)`:
/// the Launch's routing decision, the Run's effect journal, and its frozen
/// handoffs. `freeze_path` is the coordinator-supplied destination a new
/// freeze writes (F24).
///
/// Async results — `obs`, `handoff`, `judgment`, `deadline` and the
/// `provider_limited` judgment — apply only while the
/// `(version, work_generation, evidence_generation)` they were requested
/// against still hold (F20); a stale stamp produces nothing. `cancel`,
/// `restart` and `effect_result` are synchronous or journal-bound and apply
/// unconditionally (the journal write is durable fact).
#[must_use]
pub fn transition(
    run: &Run,
    event: &Versioned<Event>,
    now: Timestamp,
    policy: &Policy,
    read: (Option<&Decision>, &[Effect], &[FrozenHandoff]),
    freeze_path: &str,
) -> Transition {
    let (decision, journal, handoffs) = read;
    if carries_versions(&event.value) && event.requested_against != triple_of(run) {
        return nothing();
    }
    match &event.value {
        Event::Obs {
            observation,
            handoff_reading,
        } => on_observation(
            run,
            observation,
            handoff_reading.as_ref(),
            (now, policy),
            journal,
            (handoffs, freeze_path),
        ),
        Event::Handoff { digest } => {
            on_handoff(run, *digest, (now, policy), (handoffs, freeze_path))
        }
        Event::Judgment(verdict) => on_judgment(run, *verdict, (now, policy)),
        Event::Deadline(kind) => on_deadline(run, *kind, (now, policy)),
        Event::Cancel { close_pane } => on_cancel(run, *close_pane, (now, policy), journal),
        Event::ProviderLimited => settle(run, Settlement::ProviderLimited, now, policy),
        Event::EffectResult(result) => {
            on_effect_result(run, result, (now, policy), decision, journal)
        }
        Event::Restart => on_restart(run, journal),
    }
}

/// F20 — the event kinds that carry the version stamp: Jev results,
/// observations and deadlines. `cancel`, `restart` and `effect_result` are
/// not async results — the conditional writes guard them at apply time.
fn carries_versions(event: &Event) -> bool {
    match event {
        Event::Obs { .. }
        | Event::Handoff { .. }
        | Event::Judgment(..)
        | Event::Deadline(..)
        | Event::ProviderLimited => true,
        Event::Cancel { .. } | Event::EffectResult(..) | Event::Restart => false,
    }
}
