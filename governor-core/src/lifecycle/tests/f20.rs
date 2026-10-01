//! F20 — settlement: first-commit-wins, cancel, the provider-limit
//! recovery write and the version stamp.

use alloc::vec::Vec;

use crate::config::Provider;
use crate::delivery::{ExpiryReason, MailboxEventKind};
use crate::identity::{ChildStatus, Digest, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectKind, EffectOutcome, EffectState, EffectTarget, Event, JudgmentVerdict,
    Settlement, State, StateChange, UnresolvedReason, Versioned, settle, transition,
};
use crate::recovery::{RecoveryOrigin, RecoveryStatus};

use super::builders::{
    EMPTY_READ, NOW, effect_keys, effect_writes, event_dedups, event_kinds, identity, is_quiet,
    journal_effect, obs_unique, run_in, run_result, settlement_of, stale_stamped, stamped,
    test_policy, transact, triple, updated_records, updated_run,
};
#[test]
fn f20_settlement_first_commit_wins() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::Cancelled, NOW, &test_policy());
    let record = updated_run(&t);
    assert_eq!(record.state, State::Settled);
    assert_eq!(settlement_of(record), Some(Settlement::Cancelled));
    assert_eq!(record.settled_at, Some(NOW));
    assert_eq!(record.version, run.version.saturating_add(1));
    assert!(
        t.state_changes.iter().any(|c| matches!(
            c,
            StateChange::ExpireFollowUps {
                reason: ExpiryReason::RunSettled,
                ..
            }
        )),
        "settle must expire never-dispatched follow-ups in the same transaction"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::Settled]),
        "the terminal event rides the settle transaction"
    );
    assert_eq!(event_dedups(&t), Vec::from(["run:r-1:settled"]));

    // the losing side: a Run that is already settled produces nothing.
    let mut settled_run = run_in(State::Settled);
    settled_run.settlement = Some(Settlement::Accepted);
    settled_run.settled_at = Some(Timestamp(1));
    let loser = settle(&settled_run, Settlement::Cancelled, NOW, &test_policy());
    assert!(is_quiet(&loser), "a settled Run produces nothing");
}

#[test]
pub(super) fn f20_provider_limited_settlement_records_recovery_and_cooldown() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::ProviderLimited, NOW, &test_policy());
    let record = updated_run(&t);
    assert_eq!(settlement_of(record), Some(Settlement::ProviderLimited));
    let recovery = t.state_changes.iter().find_map(|c| match c {
        StateChange::RecordRecovery(o) => Some(o),
        StateChange::BindCaller(_)
        | StateChange::RecordLaunch(_)
        | StateChange::ReserveRun(_)
        | StateChange::UpdateRun(_)
        | StateChange::ChangeOwner(_)
        | StateChange::WriteEffect(_)
        | StateChange::RecordFollowUp(_)
        | StateChange::ExpireFollowUps { .. }
        | StateChange::SetCooldown(_)
        | StateChange::FreezeHandoff(_)
        | StateChange::AckEvent(_) => None,
    });
    let obligation = recovery.expect("provider_limited must record a recovery obligation");
    assert_eq!(obligation.predecessor, run.id);
    assert_eq!(obligation.origin, RecoveryOrigin::ProviderLimit);
    assert_eq!(obligation.status, RecoveryStatus::Pending);
    assert_eq!(obligation.expires_at, Timestamp(86_400_500));
    let cooldown = t.state_changes.iter().find_map(|c| match c {
        StateChange::SetCooldown(cd) => Some(cd),
        StateChange::BindCaller(_)
        | StateChange::RecordLaunch(_)
        | StateChange::ReserveRun(_)
        | StateChange::UpdateRun(_)
        | StateChange::ChangeOwner(_)
        | StateChange::WriteEffect(_)
        | StateChange::RecordFollowUp(_)
        | StateChange::ExpireFollowUps { .. }
        | StateChange::RecordRecovery(_)
        | StateChange::FreezeHandoff(_)
        | StateChange::AckEvent(_) => None,
    });
    let cooldown_row = cooldown.expect("provider_limited must cool the provider down");
    assert_eq!(cooldown_row.provider, Provider("prov-1".into()));
    assert_eq!(cooldown_row.until, Timestamp(3_600_500));
    assert_eq!(cooldown_row.source_run, Some(run.id.clone()));
    assert_eq!(
        event_kinds(&t),
        Vec::from([
            MailboxEventKind::CooldownHit,
            MailboxEventKind::RecoveryPending,
            MailboxEventKind::Settled,
        ])
    );
}

#[test]
fn f20_cooldown_hit_body_escapes_the_provider() {
    // a free-form provider name — quote, backslash, newline — must land in
    // `body_json` escaped: the event body is JSON, not a template (F21).
    let mut run = run_in(State::Active);
    run.provider = Some(Provider("we\"ird\\pro\nvider".into()));
    let t = transition(
        &run,
        &stamped(&run, Event::ProviderLimited),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let body = t
        .events
        .iter()
        .find(|e| e.kind == MailboxEventKind::CooldownHit)
        .map(|e| e.body.as_str());
    assert_eq!(
        body,
        Some("{\"provider\":\"we\\\"ird\\\\pro\\u000avider\"}"),
        "quote, backslash and the control char all escape — the body parses"
    );
}

#[test]
fn f20_provider_limited_without_provider_skips_cooldown() {
    let mut run = run_in(State::Starting);
    run.provider = None;
    let t = settle(&run, Settlement::ProviderLimited, NOW, &test_policy());
    assert!(
        !t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::SetCooldown(_))),
        "no provider → no cooldown row"
    );
    assert!(
        t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::RecordRecovery(_))),
        "the obligation is recorded regardless"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::RecoveryPending, MailboxEventKind::Settled])
    );
}

#[test]
fn f20_accepted_and_rejected_emit_their_events() {
    let run = run_in(State::Judging);
    let t = settle(&run, Settlement::Accepted, NOW, &test_policy());
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffAccepted, MailboxEventKind::Settled])
    );
    let t_reject = settle(&run, Settlement::Rejected, NOW, &test_policy());
    assert_eq!(
        event_kinds(&t_reject),
        Vec::from([MailboxEventKind::HandoffRejected, MailboxEventKind::Settled])
    );
}

#[test]
pub(super) fn f20_stamped_events_drop_when_versions_moved() {
    let run = run_in(State::Active);
    let events = Vec::from([
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([7; 32]),
        },
        Event::Judgment(JudgmentVerdict::Accept),
        Event::Deadline(DeadlineKind::Idle),
        Event::ProviderLimited,
    ]);
    for event in events {
        let t = transition(
            &run,
            &stale_stamped(&run, event),
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert!(is_quiet(&t), "a stale async result must produce nothing");
    }
    // work_generation and evidence_generation mismatches drop too.
    let mut stamp = triple(&run);
    stamp.work_generation = stamp.work_generation.saturating_add(1);
    let t = transition(
        &run,
        &Versioned {
            requested_against: stamp,
            value: obs_unique(Some(ChildStatus::Working)),
        },
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t), "a stale work_generation must produce nothing");
}

#[test]
fn f20_synchronous_events_apply_regardless_of_stamp() {
    let run = run_in(State::Active);
    // cancel, restart and effect_result are not version-gated (F20: only Jev
    // results, observations and deadlines carry the triple).
    let t = transact(
        &run,
        &stale_stamped(&run, Event::Cancel { close_pane: false }),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Cancelled),
        "cancel applies even with a stale stamp"
    );
    let t_restart = transact(&run, &stale_stamped(&run, Event::Restart));
    assert!(is_quiet(&t_restart));
    let t_journal = transact(
        &run,
        &stale_stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert_eq!(
        effect_writes(&t_journal),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)]),
        "the journal write is durable fact"
    );
}

#[test]
pub(super) fn f20_cancel_on_unsettled_settles_cancelled() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
    ] {
        let run = run_in(state);
        let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: false }));
        let record = updated_run(&t);
        assert_eq!(
            settlement_of(record),
            Some(Settlement::Cancelled),
            "cancel settles any unsettled Run"
        );
        assert!(t.effects.is_empty(), "no close was requested");
    }
}

#[test]
pub(super) fn f20_cancel_with_close_pane_plans_one_verified_close() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: true }));
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:close"]));
    let effect = &t.effects[0];
    assert_eq!(effect.kind, EffectKind::Close);
    assert_eq!(
        effect.target,
        Some(EffectTarget::Child(identity())),
        "the close carries the captured identity to verify against"
    );
    // a second cancel never re-plans the close — the key dedups.
    let journal = Vec::from([journal_effect(
        &run,
        "close",
        EffectKind::Close,
        EffectState::Planned,
    )]);
    let t_again = transition(
        &run,
        &stamped(&run, Event::Cancel { close_pane: true }),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        t_again.effects.is_empty(),
        "an already-planned close is not re-planned"
    );
    assert_eq!(
        settlement_of(updated_run(&t_again)),
        Some(Settlement::Cancelled)
    );
}

#[test]
pub(super) fn f20_settled_accepts_only_cancel_with_close_pane() {
    let mut run = run_in(State::Settled);
    run.settlement = Some(Settlement::Accepted);
    run.settled_at = Some(Timestamp(1));
    // any other event is ignored — settlement is immutable.
    for event in [
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([1; 32]),
        },
        Event::Judgment(JudgmentVerdict::Reject),
        Event::Deadline(DeadlineKind::MaxAge),
        Event::Cancel { close_pane: false },
        Event::ProviderLimited,
        Event::Restart,
    ] {
        let t = transition(
            &run,
            &Versioned {
                requested_against: triple(&run),
                value: event,
            },
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert!(
            is_quiet(&t),
            "settled ignores everything but cancel+closePane"
        );
    }
    let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: true }));
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:close"]));
    assert!(
        updated_records(&t).is_empty(),
        "settled cancel writes no run row"
    );
}

#[test]
fn f20_settled_event_body_carries_the_spelling() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::Cancelled, NOW, &test_policy());
    assert_eq!(
        t.events.last().map(|e| e.body.as_str()),
        Some("{\"settlement\":\"cancelled\"}"),
        "the terminal event body names the settlement"
    );
    let t_unresolved = settle(
        &run,
        Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
        NOW,
        &test_policy(),
    );
    assert_eq!(
        t_unresolved.events.last().map(|e| e.body.as_str()),
        Some("{\"settlement\":\"unresolved\",\"reason\":\"max_age\"}"),
        "unresolved carries settlement_reason"
    );
}
