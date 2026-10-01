//! F21 — the `provider_limited` settlement's recovery transaction: the
//! pending obligation, the merged provider cooldown and the two mailbox
//! events, including `body_json` escaping. This is the one implementation
//! — the lifecycle settle path extends it with the terminal event.

use alloc::vec::Vec;

use crate::config::Provider;
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{DedupKey, EventId, RunId, Timestamp};
use crate::lifecycle::{StateChange, Transition};
use crate::recovery::{Cooldown, RecoveryStatus, provider_limited};

use super::builders::{caller, obligation, policy, run};

#[test]
fn f21_provider_limited_writes() {
    let mut predecessor = run("run-1", &caller(), None);
    predecessor.provider = Some(Provider("prov-1".into()));
    let transition = provider_limited(&predecessor, None, Timestamp(1_000), &policy());
    let mut expected_obligation = obligation(RecoveryStatus::Pending);
    expected_obligation.expires_at = Timestamp(1_000 + 86_400_000);
    let expected = Transition {
        state_changes: Vec::from([
            StateChange::RecordRecovery(expected_obligation),
            StateChange::SetCooldown(Cooldown {
                provider: Provider("prov-1".into()),
                until: Timestamp(1_000 + 3_600_000),
                reason: "provider_limited".into(),
                source_run: Some(RunId("run-1".into())),
            }),
        ]),
        events: Vec::from([
            MailboxEvent {
                id: EventId("evt:run:run-1:cooldown_hit".into()),
                dedup_key: DedupKey("run:run-1:cooldown_hit".into()),
                subject: MailboxSubject::Run(RunId("run-1".into())),
                kind: MailboxEventKind::CooldownHit,
                body: "{\"provider\":\"prov-1\",\"until\":3601000}".into(),
            },
            MailboxEvent {
                id: EventId("evt:run:run-1:recovery_pending".into()),
                dedup_key: DedupKey("run:run-1:recovery_pending".into()),
                subject: MailboxSubject::Run(RunId("run-1".into())),
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
    let mut predecessor = run("run-1", &caller(), None);
    predecessor.provider = Some(Provider("prov-1".into()));
    let existing = Cooldown {
        provider: Provider("prov-1".into()),
        until: Timestamp(9_999_999),
        reason: "earlier".into(),
        source_run: Some(RunId("run-0".into())),
    };
    let transition = provider_limited(&predecessor, Some(&existing), Timestamp(1_000), &policy());
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
fn f21_provider_limited_without_provider_cools_down_nothing() {
    let transition = provider_limited(
        &run("run-1", &caller(), None),
        None,
        Timestamp(0),
        &policy(),
    );
    assert_eq!(
        transition.state_changes.len(),
        1,
        "a provider-less run records only the obligation"
    );
    assert_eq!(
        transition.events.len(),
        1,
        "only `recovery_pending` is emitted — no cooldown, no `cooldown_hit`"
    );
    assert_eq!(transition.events[0].kind, MailboxEventKind::RecoveryPending,);
}

#[test]
fn f21_event_bodies_escape_json() {
    let mut predecessor = run("run-\"1", &caller(), None);
    predecessor.provider = Some(Provider("prov\\\"1".into()));
    let transition = provider_limited(&predecessor, None, Timestamp(0), &policy());
    assert_eq!(
        transition.events[0].body, "{\"provider\":\"prov\\\\\\\"1\",\"until\":3600000}",
        "free-form provider values cannot break body_json"
    );
    assert_eq!(
        transition.events[1].body,
        "{\"predecessor\":\"run-\\\"1\",\"expires_at\":86400000,\"message\":\"close the predecessor's pane (cancel with closePane) to dispatch the recovery\"}",
        "free-form run ids cannot break body_json"
    );

    let mut control_run = run("run-2", &caller(), None);
    control_run.provider = Some(Provider("p\u{1f}".into()));
    let control = provider_limited(&control_run, None, Timestamp(0), &policy());
    assert_eq!(
        control.events[0].body, "{\"provider\":\"p\\u001f\",\"until\":3600000}",
        "control characters escape as \\u00XX"
    );
}
