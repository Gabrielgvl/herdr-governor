//! F22/F24 — the `judgment` lane: a fresh verdict settles `accepted`, opens
//! `repair` (arming `repair_deadline` once per work generation), or waits on
//! `judgment_deadline`; everywhere but `judging` it is ignored. While a
//! qualifying in-window repair dispatch is still `dispatching` every verdict
//! defers — its pending result decides the generation (F24).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::{judgment_deadline, repair_deadline};
use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::Timestamp;
use crate::lifecycle::{
    Effect, JudgmentVerdict, Run, Settlement, State, Transition, edited, mailbox_event, nothing,
    settle, update_if_changed, write_run,
};

use super::repair_dispatch_pending;

pub(super) fn on_judgment(
    run: &Run,
    verdict: JudgmentVerdict,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    match run.state {
        State::Judging => {
            // F24 — an in-window repair dispatch still in flight decides the
            // verdict's fate itself: a qualifying resolution advances the
            // work generation (the verdict goes stale under F20); a
            // provably-absent one re-plans the ask (see `repair_result`).
            // Until then the judgment defers — it produces nothing.
            if journal
                .iter()
                .any(|effect| repair_dispatch_pending(run, effect))
            {
                return nothing();
            }
            match verdict {
                JudgmentVerdict::Accept => settle(run, Settlement::Accepted, now, policy),
                JudgmentVerdict::Reject => {
                    // repair_deadline arms on the first rejection of a work
                    // generation and is never extended (F24); rejected_at
                    // persists that first rejection's time — never derived
                    // from the deadline, a policy reload would shift it.
                    let deadline = repair_deadline(run.repair_deadline, now, policy.repair_window);
                    let record = edited(run, |next| {
                        next.state = State::Repair;
                        next.repair_deadline = Some(deadline);
                        next.rejected_at = next.rejected_at.or(Some(now));
                        next.idle_since = None;
                        next.idle_deadline = None;
                    });
                    Transition {
                        state_changes: Vec::from([write_run(run, record)]),
                        events: Vec::from([mailbox_event(
                            run,
                            MailboxEventKind::HandoffRejected,
                            &format!(
                                "handoff_rejected:{}:{}",
                                run.work_generation, run.evidence_generation
                            ),
                            String::from("{\"verdict\":\"reject\"}"),
                        )]),
                        effects: Vec::new(),
                    }
                }
                // Jev could not complete: the Run waits on judgment_deadline
                // (armed at the freeze; defensive re-arm here).
                JudgmentVerdict::Unavailable => update_if_changed(run, |next| {
                    next.judgment_deadline = Some(judgment_deadline(
                        next.judgment_deadline,
                        now,
                        policy.judgment_window,
                    ));
                }),
            }
        }
        // A fresh verdict is meaningful only while a frozen handoff is being
        // judged; everywhere else it is ignored.
        State::Reserved
        | State::Starting
        | State::Prompting
        | State::Active
        | State::Repair
        | State::Settled => nothing(),
    }
}
