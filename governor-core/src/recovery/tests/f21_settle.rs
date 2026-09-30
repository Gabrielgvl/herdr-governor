//! F21 — the `provider_limited` settlement's recovery transaction: the
//! pending obligation, the merged provider cooldown and the two mailbox
//! events, including `body_json` escaping.

use alloc::vec::Vec;

use crate::config::Provider;
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{DedupKey, EventId, RunId, Timestamp};
use crate::lifecycle::{StateChange, Transition};
use crate::recovery::{Cooldown, RecoveryStatus, provider_limited};

use super::builders::{obligation, policy};

#[test]
fn f21_provider_limited_writes() {
    let predecessor = RunId("run-1".into());
    let transition = provider_limited(
        &predecessor,
        Provider("prov-1".into()),
        None,
        Timestamp(1_000),
        &policy(),
        EventId("event-1".into()),
        EventId("event-2".into()),
    );
    let mut expected_obligation = obligation(RecoveryStatus::Pending);
    expected_obligation.expires_at = Timestamp(1_000 + 86_400_000);
    let expected = Transition {
        state_changes: Vec::from([
            StateChange::RecordRecovery(expected_obligation),
            StateChange::SetCooldown(Cooldown {
                provider: Provider("prov-1".into()),
                until: Timestamp(1_000 + 3_600_000),
                reason: "provider_limited".into(),
                source_run: Some(predecessor.clone()),
            }),
        ]),
        events: Vec::from([
            MailboxEvent {
                id: EventId("event-1".into()),
                dedup_key: DedupKey("run:run-1:cooldown_hit".into()),
                subject: MailboxSubject::Run(predecessor.clone()),
                kind: MailboxEventKind::CooldownHit,
                body: "{\"provider\":\"prov-1\",\"until\":3601000}".into(),
            },
            MailboxEvent {
                id: EventId("event-2".into()),
                dedup_key: DedupKey("run:run-1:recovery_pending".into()),
                subject: MailboxSubject::Run(predecessor.clone()),
                kind: MailboxEventKind::RecoveryPending,
                body: "{\"predecessor\":\"run-1\",\"expires_at\":86401000,\"message\":\"close the predecessor's pane (cancel with closePane) to dispatch the recovery\"}"
                    .into(),
            },
        ]),
        effects: Vec::new(),
    };
    assert_eq!(
        transition, expected,
        "settle records the pending obligation, the provider cooldown and both events in one transaction"
    );
}

#[test]
fn f21_provider_limited_merges_existing_cooldown() {
    let existing = Cooldown {
        provider: Provider("prov-1".into()),
        until: Timestamp(9_999_999),
        reason: "earlier".into(),
        source_run: Some(RunId("run-0".into())),
    };
    let transition = provider_limited(
        &RunId("run-1".into()),
        Provider("prov-1".into()),
        Some(&existing),
        Timestamp(1_000),
        &policy(),
        EventId("event-1".into()),
        EventId("event-2".into()),
    );
    assert_eq!(
        transition.state_changes[1],
        StateChange::SetCooldown(existing),
        "the longer existing cooldown is never shortened"
    );
    assert_eq!(
        transition.events[0].body, "{\"provider\":\"prov-1\",\"until\":9999999}",
        "the event reports the effective cooldown"
    );
}

#[test]
fn f21_event_bodies_escape_json() {
    let transition = provider_limited(
        &RunId("run-\"1".into()),
        Provider("prov\\\"1".into()),
        None,
        Timestamp(0),
        &policy(),
        EventId("event-1".into()),
        EventId("event-2".into()),
    );
    assert_eq!(
        transition.events[0].body, "{\"provider\":\"prov\\\\\\\"1\",\"until\":3600000}",
        "free-form provider values cannot break body_json"
    );
    assert_eq!(
        transition.events[1].body,
        "{\"predecessor\":\"run-\\\"1\",\"expires_at\":86400000,\"message\":\"close the predecessor's pane (cancel with closePane) to dispatch the recovery\"}",
        "free-form run ids cannot break body_json"
    );

    let control = provider_limited(
        &RunId("run-2".into()),
        Provider("p\u{1f}".into()),
        None,
        Timestamp(0),
        &policy(),
        EventId("event-1".into()),
        EventId("event-2".into()),
    );
    assert_eq!(
        control.events[0].body, "{\"provider\":\"p\\u001f\",\"until\":3600000}",
        "control characters escape as \\u00XX"
    );
}
