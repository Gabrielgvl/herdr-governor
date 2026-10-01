//! F22 — the total Appendix C transition function: every `State` against
//! every `Event`, matched exhaustively. This is the dispatch — the `on_*`
//! lanes and the `effect_result` consequences (launch pipeline, prompt,
//! repair) live one per event family under `transition/`.

use alloc::format;

use crate::acceptance::FrozenHandoff;
use crate::config::Policy;
use crate::identity::Timestamp;
use crate::routing::Decision;

use super::{
    Effect, EffectKind, Event, Run, Settlement, Transition, VersionTriple, Versioned, nothing,
    settle,
};

mod cancel;
mod deadline;
mod effect_result;
mod evidence;
mod handoff;
mod judgment;
mod obs;
mod restart;

use self::{
    cancel::on_cancel, deadline::on_deadline, effect_result::on_effect_result,
    evidence::on_evidence, handoff::on_handoff, judgment::on_judgment, obs::on_observation,
    restart::on_restart,
};

/// F20 — the version stamp a Run reads as "still holding".
fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// F24 — the journal row proves a repair follow-up (`run:<id>:outbox:<seq>`
/// `prompt`) was dispatched inside the rejected generation's window,
/// `rejected_at <= dispatched_at < repair_deadline`: a dispatch before the
/// rejection or at/after the deadline never qualifies, whatever the
/// result or its arrival time. Both window ends are persisted on the row —
/// `rejected_at` is the first rejection's stored time, never derived from
/// `repair_deadline`, so a policy reload cannot shift it.
fn repair_dispatch_in_window(run: &Run, effect: &Effect) -> bool {
    effect.kind == EffectKind::Prompt
        && effect
            .key
            .0
            .starts_with(&format!("run:{}:outbox:", run.id.0))
        && run
            .rejected_at
            .zip(run.repair_deadline)
            .zip(effect.dispatched_at)
            .is_some_and(|((rejected, deadline), dispatched)| {
                rejected <= dispatched && dispatched < deadline
            })
}

/// F22 — the total Appendix C transition function: every `State` against
/// every `Event`, matched exhaustively. `read` is the persisted context the
/// transition consults beyond the Run row — `(decision, journal, handoffs)`:
/// the Launch's routing decision, the Run's effect journal, and its frozen
/// handoffs. `freeze_path` is the coordinator-supplied destination a new
/// freeze writes (F24).
///
/// Async results apply only while the versions they were requested against
/// still hold (F20); a stale stamp produces nothing. Jev results —
/// `judgment` and `provider_limited` — are stale only when
/// `work_generation` or `evidence_generation` moved; `version` is the
/// conditional write's compare-and-swap guard, retried by the shell on
/// conflict, never a staleness test for them. The other stamped results —
/// `obs`, `handoff`, `deadline`, `evidence` — keep the full triple.
/// `cancel`, `restart` and `effect_result` are synchronous or journal-bound
/// and apply unconditionally (the journal write is durable fact; a Jev
/// receipt's own stamp is checked inside `on_effect_result`).
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
    let stale = match &event.value {
        Event::Judgment(..) | Event::ProviderLimited => {
            event.requested_against.work_generation != run.work_generation
                || event.requested_against.evidence_generation != run.evidence_generation
        }
        Event::Obs { .. }
        | Event::Handoff { .. }
        | Event::Deadline(..)
        | Event::Evidence { .. } => event.requested_against != triple_of(run),
        Event::Cancel { .. } | Event::EffectResult(..) | Event::Restart => false,
    };
    if stale {
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
        Event::Deadline(kind) => on_deadline(run, *kind, (now, policy), journal),
        Event::Cancel { close_pane } => on_cancel(run, *close_pane, (now, policy), journal),
        Event::ProviderLimited => settle(run, Settlement::ProviderLimited, now, policy),
        Event::Evidence { digest } => on_evidence(run, *digest, (now, policy)),
        Event::EffectResult(result) => {
            on_effect_result(run, result, (now, policy), decision, journal)
        }
        Event::Restart => on_restart(run, journal),
    }
}
