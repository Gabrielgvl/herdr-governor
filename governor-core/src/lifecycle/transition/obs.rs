//! F22 — the `obs` lane: `invalid` changes nothing in any state, `absent`
//! settles the launch states or performs the one-shot handoff read in
//! `active`, and `unique` defers to the F23/F25 supervision mapping.

use crate::acceptance::{FrozenHandoff, HandoffReading};
use crate::config::Policy;
use crate::identity::{Observation, Timestamp};
use crate::lifecycle::supervision::on_unique;
use crate::lifecycle::{
    Effect, Run, Settlement, State, Transition, UnresolvedReason, nothing, settle,
};

use super::handoff::{enter_judging, freeze_and_judge};

pub(super) fn on_observation(
    run: &Run,
    observation: &Observation,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    journal: &[Effect],
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    match observation {
        // obs(invalid) changes nothing in any state (F3/H#74).
        Observation::Invalid => nothing(),
        Observation::Absent => on_absent(run, reading, env, handoffs),
        Observation::Unique {
            status,
            pane,
            native_session,
        } => on_unique(run, *status, pane, native_session.as_ref(), env, journal),
    }
}

fn on_absent(
    run: &Run,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let (now, policy) = env;
    match run.state {
        // The pane that would host the child is gone before it existed.
        State::Reserved => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchNotStarted,
            },
            now,
            policy,
        ),
        State::Starting => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchFailed,
            },
            now,
            policy,
        ),
        State::Prompting => settle(run, Settlement::PaneLost, now, policy),
        State::Active => {
            // F25 — a frozen handoff that has not been judged goes to
            // judgment first; otherwise the marked file is read once.
            // Unreachable through persisted states: a freeze row at the
            // current generation is written with the move to `judging`,
            // and every return to `active` advances `work_generation`.
            if handoffs
                .0
                .iter()
                .any(|h| h.work_generation == run.work_generation)
            {
                enter_judging(run, env)
            } else {
                match reading {
                    Some(HandoffReading::Valid { digest }) => {
                        freeze_and_judge(run, *digest, env, handoffs.1)
                    }
                    Some(HandoffReading::NotWritten) | None => {
                        settle(run, Settlement::PaneLost, now, policy)
                    }
                }
            }
        }
        // `judging` still judges the frozen handoff; `repair` waits out its
        // deadline; `settled` ignores everything but the close rule.
        State::Judging | State::Repair | State::Settled => nothing(),
    }
}
