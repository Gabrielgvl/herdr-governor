//! F20 — the "Settle" transaction as a pure value: the conditional run
//! write (`settlement IS NULL AND version = :v`), the terminal event, the
//! expiry of queued follow-ups, and for `provider_limited` the recovery
//! obligation plus the provider cooldown (F21).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::{ExpiryReason, MailboxEventKind};
use crate::recovery::provider_limited;

use super::{
    Run, Settlement, State, StateChange, Timestamp, Transition, edited, mailbox_event, nothing,
    write_run,
};

/// The `settled` event's body — the settlement plus the reason for
/// `unresolved` (Appendix B `settlement_reason`).
fn settled_body(settlement: Settlement) -> String {
    match settlement {
        Settlement::Unresolved { reason } => format!(
            "{{\"settlement\":\"unresolved\",\"reason\":\"{}\"}}",
            reason.as_str()
        ),
        Settlement::Accepted
        | Settlement::Rejected
        | Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::ProviderLimited => {
            format!("{{\"settlement\":\"{}\"}}", settlement.as_str())
        }
    }
}

/// F20 — the "Settle" transaction as a pure value: the conditional run write
/// (`settlement IS NULL AND version = :v`), the terminal event, the expiry of
/// queued follow-ups that were never dispatched, and for `provider_limited`
/// the recovery obligation plus the provider cooldown (F21).
///
/// First-commit-wins: on an already-settled Run this produces nothing — the
/// losing transition commits nothing and causes no effect.
#[must_use]
pub fn settle(run: &Run, settlement: Settlement, now: Timestamp, policy: &Policy) -> Transition {
    if run.settlement.is_some() {
        return nothing();
    }
    let record = edited(run, |next| {
        next.state = State::Settled;
        next.settlement = Some(settlement);
        next.settled_at = Some(now);
    });
    let mut state_changes = Vec::from([
        write_run(run, record),
        StateChange::ExpireFollowUps {
            run: run.id.clone(),
            reason: ExpiryReason::RunSettled,
        },
    ]);
    let mut events = Vec::new();
    match settlement {
        Settlement::Accepted => events.push(mailbox_event(
            run,
            MailboxEventKind::HandoffAccepted,
            "handoff_accepted",
            String::from("{\"handoff\":\"accepted\"}"),
        )),
        Settlement::Rejected => events.push(mailbox_event(
            run,
            MailboxEventKind::HandoffRejected,
            "handoff_rejected",
            String::from("{\"handoff\":\"rejected\"}"),
        )),
        Settlement::ProviderLimited => {
            // F21 — one implementation: the recovery share (the unique
            // obligation, the provider's merged cooldown and the
            // F18/F21-bodied `cooldown_hit`/`recovery_pending` events)
            // lives in `recovery::provider_limited`; the settle
            // transaction extends it with the terminal event. No
            // `existing_cooldown` reaches `transition` — the Appendix B
            // upsert still keeps `max(existing, new)` at the store.
            let share = provider_limited(run, None, now, policy);
            state_changes.extend(share.state_changes);
            events.extend(share.events);
        }
        Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::Unresolved { reason: _ } => {}
    }
    events.push(mailbox_event(
        run,
        MailboxEventKind::Settled,
        "settled",
        settled_body(settlement),
    ));
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}
