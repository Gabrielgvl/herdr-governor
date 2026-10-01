//! F22 — the `deadline` lane: `max_age` bounds every unsettled Run, `idle`
//! settles an open episode `no_handoff`, `repair` keeps running through
//! `judging`, and `judgment` settles `unresolved(judgment_unavailable)`;
//! `settled` answers nothing.

use crate::config::Policy;
use crate::identity::Timestamp;
use crate::lifecycle::{
    DeadlineKind, Effect, EffectState, Run, Settlement, State, Transition, UnresolvedReason,
    nothing, settle,
};

use super::repair_dispatch_in_window;

pub(super) fn on_deadline(
    run: &Run,
    kind: DeadlineKind,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    if run.state == State::Settled {
        return nothing();
    }
    let overdue = |deadline: Option<Timestamp>| deadline.is_some_and(|d| now >= d);
    match kind {
        DeadlineKind::MaxAge => {
            if now >= run.max_age_deadline {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::MaxAge,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
        DeadlineKind::Idle => {
            if run.state == State::Active && overdue(run.idle_deadline) {
                settle(run, Settlement::NoHandoff, now, policy)
            } else {
                nothing()
            }
        }
        // The repair deadline keeps running through `judging` — a re-frozen
        // handoff does not extend it (F24). In either state a qualifying
        // follow-up still `dispatching` holds the settle: its in-window
        // dispatch already counts, the pending result decides (F24).
        DeadlineKind::Repair => {
            let in_flight = journal.iter().any(|effect| {
                effect.state == EffectState::Dispatching && repair_dispatch_in_window(run, effect)
            });
            if (run.state == State::Repair || run.state == State::Judging)
                && overdue(run.repair_deadline)
                && !in_flight
            {
                settle(run, Settlement::Rejected, now, policy)
            } else {
                nothing()
            }
        }
        DeadlineKind::Judgment => {
            if run.state == State::Judging && overdue(run.judgment_deadline) {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::JudgmentUnavailable,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
    }
}
