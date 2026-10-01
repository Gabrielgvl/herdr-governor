//! F21 — the recovery share of a `provider_limited` settlement's
//! transaction (Appendix B "Settle"): the obligation, the provider's
//! cooldown and the mailbox events in one `Transition`. This is the one
//! implementation — the lifecycle settle path extends it with the
//! terminal event instead of rebuilding it.

use alloc::format;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::Timestamp;
use crate::lifecycle::{Run, StateChange, Transition, mailbox_event};

use super::{Cooldown, RecoveryObligation, RecoveryOrigin, json_str};

/// F21 — the `provider_limited` settlement's recovery share: the unique
/// `pending` obligation, the provider's merged cooldown (only while the
/// Run names one — a provider-less Run cools down nothing), and the
/// `cooldown_hit` + `recovery_pending` mailbox events — the latter tells
/// the owner that `cancel` with `closePane` triggers the dispatch
/// (ADR-0003). Mailbox bodies carry the fields F18/F21 specify: the
/// provider and its effective `until`, and the predecessor, expiry and
/// recovery instruction.
#[must_use]
pub fn provider_limited(
    run: &Run,
    existing_cooldown: Option<&Cooldown>,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    let obligation = RecoveryObligation::pending(
        run.id.clone(),
        RecoveryOrigin::ProviderLimit,
        now,
        policy.recovery_expiry,
    );
    let mut state_changes = Vec::from([StateChange::RecordRecovery(obligation.clone())]);
    let mut events = Vec::new();
    if let Some(provider) = &run.provider {
        let candidate = Cooldown::limited(provider.clone(), run.id.clone(), now, policy.cooldown);
        let cooldown = match existing_cooldown {
            Some(existing) => existing.merged(candidate),
            None => candidate,
        };
        events.push(mailbox_event(
            run,
            MailboxEventKind::CooldownHit,
            "cooldown_hit",
            format!(
                "{{\"provider\":{},\"until\":{}}}",
                json_str(&cooldown.provider.0),
                cooldown.until.0
            ),
        ));
        state_changes.push(StateChange::SetCooldown(cooldown));
    }
    events.push(mailbox_event(
        run,
        MailboxEventKind::RecoveryPending,
        "recovery_pending",
        format!(
            "{{\"predecessor\":{},\"expires_at\":{},\"message\":{}}}",
            json_str(&run.id.0),
            obligation.expires_at.0,
            json_str(
                "close the predecessor's pane (cancel with closePane) to dispatch the recovery"
            )
        ),
    ));
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}
