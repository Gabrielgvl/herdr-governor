//! F18 — the mailbox: kind spellings, dedup-key rules, event emission, and
//! the hint gate for the owner's pane.

use crate::config::Capability;
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject, hint_effect, hint_eligible};
use crate::identity::{
    ChildStatus, DedupKey, EffectId, EffectKey, EventId, LaunchId, NativeSession, Observation,
    PaneId, RunId, Timestamp,
};
use crate::lifecycle::{EffectKind, EffectState, EffectTarget};

use super::builders::{caller, digest, owner_pane, qualified};

#[test]
fn f18_mailbox_event_kind_spellings() {
    let cases = [
        (MailboxEventKind::HandoffAccepted, "handoff_accepted"),
        (MailboxEventKind::HandoffRejected, "handoff_rejected"),
        (MailboxEventKind::Settled, "settled"),
        (MailboxEventKind::Stalled, "stalled"),
        (MailboxEventKind::BlockedOnInput, "blocked_on_input"),
        (MailboxEventKind::OutsideScope, "outside_scope"),
        (MailboxEventKind::LaunchFailed, "launch_failed"),
        (MailboxEventKind::LaunchAnswered, "launch_answered"),
        (MailboxEventKind::PromptUnconfirmed, "prompt_unconfirmed"),
        (
            MailboxEventKind::FollowUpUnconfirmed,
            "follow_up_unconfirmed",
        ),
        (MailboxEventKind::FollowUpExpired, "follow_up_expired"),
        (MailboxEventKind::CooldownHit, "cooldown_hit"),
        (MailboxEventKind::RecoveryPending, "recovery_pending"),
        (MailboxEventKind::RecoveryBlocked, "recovery_blocked"),
        (MailboxEventKind::RecoveryDispatched, "recovery_dispatched"),
    ];
    for (kind, name) in cases {
        assert_eq!(
            kind.as_str(),
            name,
            "mailbox event kind spelling must match F18"
        );
    }
}

// ---- F18 mailbox ----

#[test]
fn f18_run_scoped_dedup_key_spellings() {
    let subject = MailboxSubject::Run(RunId("r1".into()));
    let cases = [
        (MailboxEventKind::HandoffAccepted, "run:r1:handoff_accepted"),
        (MailboxEventKind::Settled, "run:r1:settled"),
        (
            MailboxEventKind::PromptUnconfirmed,
            "run:r1:prompt_unconfirmed",
        ),
        (MailboxEventKind::CooldownHit, "run:r1:cooldown_hit"),
        (MailboxEventKind::RecoveryPending, "run:r1:recovery_pending"),
        (MailboxEventKind::RecoveryBlocked, "run:r1:recovery_blocked"),
        (
            MailboxEventKind::RecoveryDispatched,
            "run:r1:recovery_dispatched",
        ),
    ];
    for (kind, key) in cases {
        assert_eq!(
            kind.dedup_key(&subject, None),
            Some(DedupKey(key.into())),
            "one-shot run kinds dedup per run (F18)"
        );
    }
}

#[test]
fn f18_recurrent_kinds_dedup_per_qualifier() {
    let subject = MailboxSubject::Run(RunId("r1".into()));
    let cases = [
        (MailboxEventKind::Stalled, 2, "run:r1:stalled:2"),
        (
            MailboxEventKind::BlockedOnInput,
            7,
            "run:r1:blocked_on_input:7",
        ),
        (MailboxEventKind::OutsideScope, 7, "run:r1:outside_scope:7"),
        (
            MailboxEventKind::HandoffRejected,
            5,
            "run:r1:handoff_rejected:5",
        ),
        (
            MailboxEventKind::FollowUpUnconfirmed,
            4,
            "run:r1:follow_up_unconfirmed:4",
        ),
        (
            MailboxEventKind::FollowUpExpired,
            4,
            "run:r1:follow_up_expired:4",
        ),
    ];
    for (kind, qualifier, key) in cases {
        assert_eq!(
            kind.dedup_key(&subject, Some(qualifier)),
            Some(DedupKey(key.into())),
            "recurrent kinds dedup per episode, generation or seq (F18)"
        );
        assert_eq!(
            kind.dedup_key(&subject, None),
            None,
            "a recurrent kind without its qualifier is unkeyable"
        );
    }
    assert_eq!(
        MailboxEventKind::Settled.dedup_key(&subject, Some(1)),
        None,
        "a one-shot kind takes no qualifier"
    );
}

#[test]
fn f18_launch_failed_binds_the_launch() {
    assert_eq!(
        MailboxEventKind::LaunchFailed
            .dedup_key(&MailboxSubject::Launch(LaunchId("l1".into())), None),
        Some(DedupKey("launch:l1:launch_failed".into())),
        "launch_failed keys on the Launch (F18)"
    );
    assert_eq!(
        MailboxEventKind::LaunchFailed.dedup_key(&MailboxSubject::Run(RunId("r1".into())), None),
        None,
        "launch_failed never binds a Run"
    );
    assert_eq!(
        MailboxEventKind::Settled.dedup_key(&MailboxSubject::Launch(LaunchId("l1".into())), None),
        None,
        "run-scoped kinds never bind a Launch"
    );
    assert_eq!(
        MailboxEventKind::LaunchAnswered
            .dedup_key(&MailboxSubject::Launch(LaunchId("l1".into())), None),
        Some(DedupKey("launch:l1:launch_answered".into())),
        "launch_answered keys on the Launch (F18)"
    );
    assert_eq!(
        MailboxEventKind::LaunchAnswered.dedup_key(&MailboxSubject::Run(RunId("r1".into())), None),
        None,
        "launch_answered never binds a Run"
    );
    for kind in [
        MailboxEventKind::LaunchFailed,
        MailboxEventKind::LaunchAnswered,
    ] {
        assert!(
            kind.binds_launch(),
            "launch-only kinds bind the Launch (F18)"
        );
    }
    assert!(
        !MailboxEventKind::FollowUpExpired.binds_launch(),
        "run events are not launch-bound"
    );
}

#[test]
fn f18_emitted_event_carries_its_stable_dedup_key() {
    let emitted = MailboxEvent::emitted(
        EventId("ev7".into()),
        MailboxSubject::Run(RunId("r1".into())),
        MailboxEventKind::Stalled,
        Some(3),
        "body".into(),
    );
    let Some(event) = emitted else {
        panic!("a well-formed emission succeeds")
    };
    assert_eq!(event.id, EventId("ev7".into()), "the event id is kept");
    assert_eq!(
        event.dedup_key,
        DedupKey("run:r1:stalled:3".into()),
        "the event carries the stable dedup key (F18)"
    );
    assert_eq!(event.kind, MailboxEventKind::Stalled, "the kind is kept");
    assert_eq!(event.body, "body", "the body is kept");
    assert_eq!(
        MailboxEvent::emitted(
            EventId("ev8".into()),
            MailboxSubject::Run(RunId("r1".into())),
            MailboxEventKind::LaunchFailed,
            None,
            "b".into(),
        ),
        None,
        "a kind/subject mis-pairing emits nothing"
    );
    assert_eq!(
        MailboxEvent::emitted(
            EventId("ev9".into()),
            MailboxSubject::Run(RunId("r1".into())),
            MailboxEventKind::Stalled,
            None,
            "b".into(),
        ),
        None,
        "a missing qualifier emits nothing"
    );
}

// ---- F18 hints ----

#[test]
fn f18_hint_goes_to_a_fresh_unique_idle_or_done_owner_pane() {
    let caps = qualified(&[Capability::HINT_CONSUMPTION]);
    let owner_session = Some(NativeSession("sess-owner".into()));
    for status in [Some(ChildStatus::Idle), Some(ChildStatus::Done)] {
        assert_eq!(
            hint_eligible(
                &caller(),
                &owner_pane(status, owner_session.clone()),
                &caps,
                None,
                Timestamp(0),
            ),
            Some(PaneId("w6:p1".into())),
            "a unique idle/done owner pane is hinted (F18)"
        );
    }
}

#[test]
fn f18_hint_never_goes_to_a_busy_or_unproven_pane() {
    let caps = qualified(&[Capability::HINT_CONSUMPTION]);
    let owner_session = Some(NativeSession("sess-owner".into()));
    for status in [Some(ChildStatus::Working), Some(ChildStatus::Blocked), None] {
        assert_eq!(
            hint_eligible(
                &caller(),
                &owner_pane(status, owner_session.clone()),
                &caps,
                None,
                Timestamp(0),
            ),
            None,
            "never to a busy pane (F18/H#87)"
        );
    }
    for observation in [Observation::Absent, Observation::Invalid] {
        assert_eq!(
            hint_eligible(&caller(), &observation, &caps, None, Timestamp(0)),
            None,
            "only a fresh unique pane is hinted (F18/F3)"
        );
    }
}

#[test]
fn f18_hint_requires_the_owner_native_session() {
    let caps = qualified(&[Capability::HINT_CONSUMPTION]);
    for session in [Some(NativeSession("sess-replaced".into())), None] {
        assert_eq!(
            hint_eligible(
                &caller(),
                &owner_pane(Some(ChildStatus::Idle), session),
                &caps,
                None,
                Timestamp(0),
            ),
            None,
            "the pane must still hold the owner's native session (F18)"
        );
    }
}

#[test]
fn f18_hint_requires_qualified_hint_consumption() {
    for caps in [
        qualified(&[]),
        qualified(&[Capability::MID_TURN_INPUT]),
        qualified(&[Capability::FOLLOWUP_READ]),
    ] {
        assert_eq!(
            hint_eligible(
                &caller(),
                &owner_pane(
                    Some(ChildStatus::Idle),
                    Some(NativeSession("sess-owner".into()))
                ),
                &caps,
                None,
                Timestamp(0),
            ),
            None,
            "the owner's harness needs a qualified hint_consumption (F18/F26)"
        );
    }
}

#[test]
fn f18_hint_rate_limit_is_one_per_five_seconds_per_owner() {
    let caps = qualified(&[Capability::HINT_CONSUMPTION]);
    let pane = owner_pane(
        Some(ChildStatus::Idle),
        Some(NativeSession("sess-owner".into())),
    );
    let last = Timestamp(10_000);
    assert_eq!(
        hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(14_999)),
        None,
        "inside 5 s the owner gets no second hint (F18/H#86)"
    );
    assert_eq!(
        hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(15_000)),
        Some(PaneId("w6:p1".into())),
        "at exactly 5 s the next hint is allowed (F18)"
    );
    assert_eq!(
        hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(9_000)),
        None,
        "a skewed clock never hurries a hint"
    );
}

#[test]
fn f18_hint_effect_is_keyed_per_event_and_targets_the_owner_pane() {
    let run_event = MailboxEvent {
        id: EventId("ev7".into()),
        dedup_key: DedupKey("run:r1:settled".into()),
        subject: MailboxSubject::Run(RunId("r1".into())),
        kind: MailboxEventKind::Settled,
        body: "b".into(),
    };
    let effect = hint_effect(
        &run_event,
        PaneId("w6:p1".into()),
        EffectId("eff-1".into()),
        digest(3),
    );
    assert_eq!(
        effect.key,
        EffectKey("event:ev7:hint".into()),
        "the hint key is event:<id>:hint — planned at most once (N1/F18)"
    );
    assert_eq!(effect.kind, EffectKind::Prompt, "a hint is a prompt effect");
    assert_eq!(
        effect.target,
        Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
        "the target is the owner's pane locator, re-resolved at dispatch (F10)"
    );
    assert_eq!(
        effect.subject_run,
        Some(RunId("r1".into())),
        "a run-bound event keys a run-bound hint"
    );
    assert_eq!(
        effect.subject_launch, None,
        "no launch subject for a run event"
    );
    assert_eq!(
        effect.state,
        EffectState::Planned,
        "effects land planned (F8)"
    );
    assert_eq!(
        effect.payload_digest,
        Some(digest(3)),
        "the rendered hint's digest is recorded"
    );
    let launch_event = MailboxEvent {
        id: EventId("ev8".into()),
        dedup_key: DedupKey("launch:l1:launch_failed".into()),
        subject: MailboxSubject::Launch(LaunchId("l1".into())),
        kind: MailboxEventKind::LaunchFailed,
        body: "b".into(),
    };
    let hint = hint_effect(
        &launch_event,
        PaneId("w6:p1".into()),
        EffectId("eff-2".into()),
        digest(4),
    );
    assert_eq!(
        hint.subject_launch,
        Some(LaunchId("l1".into())),
        "a launch-bound event keys a launch-bound hint (F18)"
    );
    assert_eq!(hint.subject_run, None, "no run subject for a launch event");
}
