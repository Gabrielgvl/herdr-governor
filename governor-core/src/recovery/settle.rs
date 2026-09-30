//! F21 — the recovery share of a `provider_limited` settlement's
//! transaction (Appendix B "Settle"): the obligation, the provider's
//! cooldown and the mailbox events in one `Transition`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{Policy, Provider};
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{DedupKey, EventId, RunId, Timestamp};
use crate::lifecycle::{StateChange, Transition};

use super::{Cooldown, RecoveryObligation, RecoveryOrigin};

/// F21 — the `provider_limited` settlement's recovery share: the unique
/// `pending` obligation, the provider's merged cooldown, and the
/// `cooldown_hit` + `recovery_pending` mailbox events — the latter tells
/// the owner that `cancel` with `closePane` triggers the dispatch
/// (ADR-0003).
#[must_use]
pub fn provider_limited(
    predecessor: &RunId,
    provider: Provider,
    existing_cooldown: Option<&Cooldown>,
    now: Timestamp,
    policy: &Policy,
    cooldown_event: EventId,
    recovery_event: EventId,
) -> Transition {
    let obligation = RecoveryObligation::pending(
        predecessor.clone(),
        RecoveryOrigin::ProviderLimit,
        now,
        policy.recovery_expiry,
    );
    let candidate = Cooldown::limited(provider, predecessor.clone(), now, policy.cooldown);
    let cooldown = match existing_cooldown {
        Some(existing) => existing.merged(candidate),
        None => candidate,
    };
    let events = Vec::from([
        MailboxEvent {
            id: cooldown_event,
            dedup_key: DedupKey(format!("run:{}:cooldown_hit", predecessor.0)),
            subject: MailboxSubject::Run(predecessor.clone()),
            kind: MailboxEventKind::CooldownHit,
            body: format!(
                "{{\"provider\":{},\"until\":{}}}",
                json_str(&cooldown.provider.0),
                cooldown.until.0
            ),
        },
        MailboxEvent {
            id: recovery_event,
            dedup_key: DedupKey(format!("run:{}:recovery_pending", predecessor.0)),
            subject: MailboxSubject::Run(predecessor.clone()),
            kind: MailboxEventKind::RecoveryPending,
            body: format!(
                "{{\"predecessor\":{},\"expires_at\":{},\"message\":{}}}",
                json_str(&predecessor.0),
                obligation.expires_at.0,
                json_str(
                    "close the predecessor's pane (cancel with closePane) to dispatch the recovery"
                )
            ),
        },
    ]);
    Transition {
        state_changes: Vec::from([
            StateChange::RecordRecovery(obligation),
            StateChange::SetCooldown(cooldown),
        ]),
        events,
        effects: Vec::new(),
    }
}

/// Minimal JSON string escaping for mailbox `body_json` interpolation —
/// quotes, backslashes and control characters are escaped so a free-form
/// `Provider` or `RunId` value cannot break the event body.
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < '\u{20}' => {
                let code = u32::from(c);
                out.push_str("\\u00");
                out.push(char::from_digit(code >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit(code & 0xf, 16).unwrap_or('0'));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
