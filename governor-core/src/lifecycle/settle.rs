//! F20 — the "Settle" transaction as a pure value: the conditional run
//! write (`settlement IS NULL AND version = :v`), the terminal event, the
//! expiry of queued follow-ups, and for `provider_limited` the recovery
//! obligation plus the provider cooldown (F21).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::{ExpiryReason, MailboxEventKind};
use crate::recovery::{Cooldown, RecoveryObligation, RecoveryOrigin, RecoveryStatus};

use super::{
    Run, Settlement, State, StateChange, Timestamp, Transition, deadline_after, edited,
    mailbox_event, nothing, write_run,
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
            // F21 — same transaction: the unique obligation, the cooldown
            // (only ever lengthens), and the event telling the owner that
            // closing the pane triggers recovery.
            state_changes.push(StateChange::RecordRecovery(RecoveryObligation {
                predecessor: run.id.clone(),
                origin: RecoveryOrigin::ProviderLimit,
                status: RecoveryStatus::Pending,
                reason: None,
                successor_launch: None,
                expires_at: deadline_after(now, policy.recovery_expiry),
            }));
            if let Some(provider) = &run.provider {
                state_changes.push(StateChange::SetCooldown(Cooldown {
                    provider: provider.clone(),
                    until: deadline_after(now, policy.cooldown),
                    reason: String::from("provider_limited"),
                    source_run: Some(run.id.clone()),
                }));
                events.push(mailbox_event(
                    run,
                    MailboxEventKind::CooldownHit,
                    "cooldown_hit",
                    format!("{{\"provider\":\"{}\"}}", provider.0),
                ));
            }
            events.push(mailbox_event(
                run,
                MailboxEventKind::RecoveryPending,
                "recovery_pending",
                String::from("{\"recovery\":\"pending\"}"),
            ));
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
