//! F22/F24 — the `judgment` lane: a fresh verdict settles `accepted`, opens
//! `repair` (arming `repair_deadline` once per work generation), or waits on
//! `judgment_deadline`; everywhere but `judging` it is ignored.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::Timestamp;
use crate::lifecycle::{
    JudgmentVerdict, Run, Settlement, State, Transition, deadline_after, edited, mailbox_event,
    nothing, settle, update_if_changed, write_run,
};

pub(super) fn on_judgment(
    run: &Run,
    verdict: JudgmentVerdict,
    env: (Timestamp, &Policy),
) -> Transition {
    let (now, policy) = env;
    match run.state {
        State::Judging => match verdict {
            JudgmentVerdict::Accept => settle(run, Settlement::Accepted, now, policy),
            JudgmentVerdict::Reject => {
                // repair_deadline arms on the first rejection of a work
                // generation and is never extended (F24); rejected_at
                // persists that first rejection's time — never derived
                // from the deadline, a policy reload would shift it.
                let deadline = run
                    .repair_deadline
                    .unwrap_or_else(|| deadline_after(now, policy.repair_window));
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
                if next.judgment_deadline.is_none() {
                    next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
                }
            }),
        },
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
