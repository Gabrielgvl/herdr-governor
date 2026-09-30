//! F21 — recovery obligations and provider cooldowns (ADR-0003): a
//! `provider_limited` settlement creates exactly one obligation; dispatch
//! waits for fresh proof the predecessor's identity is absent. Panes are
//! never closed automatically.

mod admission;
mod cooldown;
mod obligation;
mod settle;

pub use admission::{
    RECOVERY_KEY_PREFIX, caller_admission, dispatch_ready, successor_key, successor_task,
};
pub use cooldown::Cooldown;
pub use obligation::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
pub use settle::provider_limited;

#[cfg(test)]
mod tests {
    mod builders;

    use alloc::vec::Vec;
    use core::time::Duration;

    use self::builders::{admitted, caller, obligation, policy, run, task, unique};
    use super::{
        Cooldown, RECOVERY_KEY_PREFIX, RecoveryObligation, RecoveryOrigin, RecoveryStatus,
        caller_admission, dispatch_ready, provider_limited, successor_key, successor_task,
    };
    use crate::config::Provider;
    use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
    use crate::identity::{
        AgentKind, CallerKey, ChildStatus, DedupKey, EventId, LaunchId, NativeSession, Observation,
        RunId, Timestamp,
    };
    use crate::lifecycle::{Settlement, StateChange, Transition};
    use crate::task::{AbstainReason, Refusal};

    #[test]
    fn f21_recovery_origin_spellings() {
        let cases = [
            (RecoveryOrigin::ProviderLimit, "provider_limit"),
            (RecoveryOrigin::Caller, "caller"),
        ];
        for (origin, name) in cases {
            assert_eq!(
                origin.as_str(),
                name,
                "recovery origin spelling must match the DDL"
            );
        }
    }

    #[test]
    fn f21_recovery_status_spellings() {
        let cases = [
            (RecoveryStatus::Pending, "pending"),
            (RecoveryStatus::Blocked, "blocked"),
            (RecoveryStatus::Dispatched, "dispatched"),
            (RecoveryStatus::Failed, "failed"),
        ];
        for (status, name) in cases {
            assert_eq!(
                status.as_str(),
                name,
                "recovery status spelling must match the DDL"
            );
        }
    }

    #[test]
    fn f21_successor_key_is_recovery_prefixed() {
        let key = successor_key(&RunId("run-123".into()));
        assert_eq!(
            key.0, "recovery:run-123",
            "successor key is recovery:<predecessorRunId>"
        );
        assert!(
            key.0.starts_with(RECOVERY_KEY_PREFIX),
            "recovery key never collides with a caller key"
        );
    }

    #[test]
    fn f21_successor_task_carries_task_plus_preamble() {
        let predecessor = RunId("run-1".into());
        let mut task = task();
        task.recovery_of = Some(RunId("grandparent".into()));
        let successor = successor_task(&predecessor, &task);
        assert!(
            successor
                .objective
                .contains("Continue from the observed git and transcript state"),
            "preamble continues from observed git/transcript state"
        );
        assert!(
            successor
                .objective
                .contains("do not repeat side effects that already happened"),
            "preamble forbids repeating side effects"
        );
        assert!(
            successor.objective.ends_with("do the thing"),
            "the predecessor's objective is carried verbatim"
        );
        assert_eq!(
            successor.recovery_of,
            Some(predecessor),
            "recovery_of re-keys to the immediate predecessor (F13 step 4)"
        );
        assert_eq!(successor.scope, "the repo", "scope preserved");
        assert_eq!(successor.done_when, ["it works"], "doneWhen preserved");
        assert_eq!(
            successor.constraints,
            ["stay quiet"],
            "constraints preserved"
        );
        assert_eq!(successor.cwd, Some("/proj".into()), "cwd preserved");
        assert_eq!(successor.label, Some("lbl".into()), "label preserved");
        assert_eq!(successor.tier, None, "tier preserved");
    }

    #[test]
    fn f21_pending_obligation_expires_at_policy_expiry() {
        let obligation = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::ProviderLimit,
            Timestamp(1_000),
            Duration::from_hours(24),
        );
        assert_eq!(obligation.status, RecoveryStatus::Pending, "starts pending");
        assert_eq!(obligation.origin, RecoveryOrigin::ProviderLimit, "origin");
        assert_eq!(obligation.reason, None, "no reason yet");
        assert_eq!(obligation.successor_launch, None, "unclaimed");
        assert_eq!(
            obligation.expires_at,
            Timestamp(1_000 + 86_400_000),
            "expires at now + the 24 h policy expiry"
        );
    }

    #[test]
    fn f21_pending_dispatches_blocks_and_fails() {
        let dispatched =
            obligation(RecoveryStatus::Pending).dispatched(LaunchId("launch-2".into()));
        let mut expected = obligation(RecoveryStatus::Dispatched);
        expected.successor_launch = Some(LaunchId("launch-2".into()));
        assert_eq!(
            dispatched,
            Some(expected),
            "pending -> dispatched binds the successor launch (Appendix B CHECK)"
        );

        let blocked = obligation(RecoveryStatus::Pending).blocked(AbstainReason::NoCandidates);
        let mut expected_blocked = obligation(RecoveryStatus::Blocked);
        expected_blocked.reason = Some("no_candidates".into());
        assert_eq!(
            blocked,
            Some(expected_blocked),
            "pending -> blocked records the abstain reason"
        );

        let failed = obligation(RecoveryStatus::Pending).failed("gone".into());
        let mut expected_failed = obligation(RecoveryStatus::Failed);
        expected_failed.reason = Some("gone".into());
        assert_eq!(
            failed,
            Some(expected_failed),
            "pending -> failed records the reason"
        );
    }

    #[test]
    fn f21_terminal_states_absorb_every_transition() {
        for status in [
            RecoveryStatus::Blocked,
            RecoveryStatus::Dispatched,
            RecoveryStatus::Failed,
        ] {
            let obligation = obligation(status);
            assert_eq!(
                obligation.dispatched(LaunchId("x".into())),
                None,
                "dispatched is pending-only"
            );
            assert_eq!(
                obligation.blocked(AbstainReason::NoCandidates),
                None,
                "blocked is pending-only"
            );
            assert_eq!(
                obligation.failed("x".into()),
                None,
                "failed is pending-only"
            );
            assert_eq!(
                obligation.expired(Timestamp(i64::MAX)),
                None,
                "expired never rewrites a terminal obligation"
            );
        }
    }

    #[test]
    fn f21_pending_expires_at_or_past_expires_at() {
        let pending = obligation(RecoveryStatus::Pending);
        assert_eq!(
            pending.expired(Timestamp(86_400_000 - 1)),
            None,
            "one millisecond early is not expired"
        );
        let mut expected = obligation(RecoveryStatus::Failed);
        expected.reason = Some("expired".into());
        assert_eq!(
            pending.expired(Timestamp(86_400_000)),
            Some(expected.clone()),
            "at expires_at the pending obligation fails expired"
        );
        assert_eq!(
            pending.expired(Timestamp(86_400_000 + 1)),
            Some(expected),
            "past expires_at is expired"
        );
    }

    #[test]
    fn f21_expiry_arithmetic_saturates() {
        let obligation = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::Caller,
            Timestamp(i64::MAX - 10),
            Duration::from_hours(24),
        );
        assert_eq!(
            obligation.expires_at,
            Timestamp(i64::MAX),
            "expiry saturates rather than wrapping"
        );
        let huge = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::Caller,
            Timestamp(0),
            Duration::MAX,
        );
        assert_eq!(
            huge.expires_at,
            Timestamp(i64::MAX),
            "an unrepresentable duration pins to i64::MAX"
        );
    }

    #[test]
    fn f21_cooldown_limited_fields() {
        let cooldown = Cooldown::limited(
            Provider("prov-1".into()),
            RunId("run-1".into()),
            Timestamp(1_000),
            Duration::from_hours(1),
        );
        assert_eq!(cooldown.provider, Provider("prov-1".into()), "provider");
        assert_eq!(
            cooldown.until,
            Timestamp(1_000 + 3_600_000),
            "until is now + the policy window"
        );
        assert_eq!(
            cooldown.reason, "provider_limited",
            "reason records the limiting settlement"
        );
        assert_eq!(
            cooldown.source_run,
            Some(RunId("run-1".into())),
            "source run recorded"
        );
    }

    #[test]
    fn f21_cooldowns_only_lengthen() {
        let existing = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(5_000),
            reason: "first".into(),
            source_run: Some(RunId("run-1".into())),
        };
        let shorter = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(4_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_shorter = existing.merged(shorter);
        assert_eq!(
            merged_shorter.until,
            Timestamp(5_000),
            "a shorter limit never shortens"
        );
        assert_eq!(
            merged_shorter.reason, "first",
            "the surviving exclusion keeps its reason"
        );

        let equal = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(5_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_equal = existing.merged(equal);
        assert_eq!(merged_equal.until, Timestamp(5_000), "equal until stays");
        assert_eq!(
            merged_equal.reason, "first",
            "a tie keeps the existing record"
        );

        let longer = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(9_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_longer = existing.merged(longer);
        assert_eq!(
            merged_longer.until,
            Timestamp(9_000),
            "a later limit lengthens"
        );
        assert_eq!(
            merged_longer.reason, "second",
            "the extending exclusion's reason rides with it"
        );
        assert_eq!(
            merged_longer.source_run,
            Some(RunId("run-2".into())),
            "the extending exclusion's source rides with it"
        );
    }

    #[test]
    fn f21_dispatch_requires_absent() {
        assert!(
            dispatch_ready(&Observation::Absent),
            "only absent dispatches"
        );
        for status in [
            Some(ChildStatus::Working),
            Some(ChildStatus::Idle),
            Some(ChildStatus::Done),
            Some(ChildStatus::Blocked),
            None,
        ] {
            assert!(
                !dispatch_ready(&unique(status)),
                "a present predecessor never dispatches (ADR-0003)"
            );
        }
        assert!(
            !dispatch_ready(&Observation::Invalid),
            "invalid never counts as absence (F3)"
        );
    }

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
    fn f21_recoveryof_requires_owner() {
        let predecessor = run("run-1", &caller(), Some(Settlement::Accepted));
        let foreign = CallerKey {
            agent_kind: AgentKind("kind-b".into()),
            native_session: NativeSession("sess-b".into()),
        };
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &foreign,
            Timestamp(0),
            &policy(),
        );
        assert_eq!(
            result,
            Err(Refusal::NotOwner),
            "a foreign caller cannot claim a run's recovery (F4)"
        );
    }

    #[test]
    fn f21_recoveryof_second_recovery_refused() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::ProviderLimited));
        for origin in [RecoveryOrigin::Caller, RecoveryOrigin::ProviderLimit] {
            for status in [
                RecoveryStatus::Pending,
                RecoveryStatus::Blocked,
                RecoveryStatus::Dispatched,
                RecoveryStatus::Failed,
            ] {
                let mut existing = obligation(status);
                existing.origin = origin;
                let claimable =
                    origin == RecoveryOrigin::ProviderLimit && status == RecoveryStatus::Pending;
                let result = caller_admission(
                    &predecessor,
                    Some(&existing),
                    &Observation::Absent,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                if claimable {
                    let claimed = admitted(result);
                    assert_eq!(
                        claimed.status,
                        RecoveryStatus::Dispatched,
                        "the unclaimed provider_limit obligation is claimable"
                    );
                } else {
                    assert_eq!(
                        result,
                        Err(Refusal::RecoveryExists),
                        "a second recovery of the same predecessor is refused ({origin:?}/{status:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn f21_recovery_of_unsettled_refused() {
        let predecessor = run("run-1", &caller(), None);
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller(),
            Timestamp(0),
            &policy(),
        );
        assert_eq!(
            result,
            Err(Refusal::RecoveryPredecessorUnsettled),
            "an unsettled predecessor refuses RECOVERY_PREDECESSOR_UNSETTLED"
        );
    }

    #[test]
    fn f21_recovery_of_active_refused_retryable() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::ProviderLimited));
        let refused = caller_admission(
            &predecessor,
            None,
            &unique(Some(ChildStatus::Working)),
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(0),
            &policy(),
        );
        assert_eq!(
            refused,
            Err(Refusal::RecoveryPredecessorActive),
            "a predecessor still observed working refuses RECOVERY_PREDECESSOR_ACTIVE"
        );
        let retried = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(0),
            &policy(),
        );
        let obligation = admitted(retried);
        assert_eq!(
            obligation.status,
            RecoveryStatus::Dispatched,
            "the refusal is retryable: once the observation gate is met the same request admits"
        );
    }

    #[test]
    fn f21_recoveryof_provider_limited_needs_absent() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::ProviderLimited));
        for observation in [
            Observation::Invalid,
            unique(Some(ChildStatus::Working)),
            unique(Some(ChildStatus::Idle)),
            unique(Some(ChildStatus::Done)),
            unique(Some(ChildStatus::Blocked)),
            unique(None),
        ] {
            let result = caller_admission(
                &predecessor,
                None,
                &observation,
                &LaunchId("launch-2".into()),
                &caller,
                Timestamp(0),
                &policy(),
            );
            assert_eq!(
                result,
                Err(Refusal::RecoveryPredecessorActive),
                "a provider_limited predecessor must be observed absent first"
            );
        }
        let claimed = caller_admission(
            &predecessor,
            Some(&obligation(RecoveryStatus::Pending)),
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(0),
            &policy(),
        );
        let mut expected = obligation(RecoveryStatus::Dispatched);
        expected.successor_launch = Some(LaunchId("launch-2".into()));
        assert_eq!(
            claimed,
            Ok(expected),
            "the claim dispatches the obligation and binds the caller's launch, preserving origin"
        );
    }

    #[test]
    fn f21_recoveryof_other_settlements_idle_done_or_absent() {
        let caller = caller();
        for settlement in [
            Settlement::Accepted,
            Settlement::Rejected,
            Settlement::NoHandoff,
            Settlement::PaneLost,
            Settlement::Cancelled,
        ] {
            let predecessor = run("run-1", &caller, Some(settlement));
            for observation in [
                unique(Some(ChildStatus::Idle)),
                unique(Some(ChildStatus::Done)),
                Observation::Absent,
            ] {
                let result = caller_admission(
                    &predecessor,
                    None,
                    &observation,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                let obligation = admitted(result);
                assert_eq!(
                    obligation.status,
                    RecoveryStatus::Dispatched,
                    "settled predecessor observed idle/done/absent admits ({settlement:?})"
                );
            }
            for observation in [
                unique(Some(ChildStatus::Working)),
                unique(Some(ChildStatus::Blocked)),
                unique(None),
                Observation::Invalid,
            ] {
                let result = caller_admission(
                    &predecessor,
                    None,
                    &observation,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                assert_eq!(
                    result,
                    Err(Refusal::RecoveryPredecessorActive),
                    "a working/blocked/unknown/invalid predecessor refuses RECOVERY_PREDECESSOR_ACTIVE ({settlement:?})"
                );
            }
        }
    }

    #[test]
    fn f21_recoveryof_creates_caller_obligation() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::Rejected));
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(1_000),
            &policy(),
        );
        let obligation = admitted(result);
        assert_eq!(
            obligation.origin,
            RecoveryOrigin::Caller,
            "a caller-requested recovery records origin caller"
        );
        assert_eq!(
            obligation.status,
            RecoveryStatus::Dispatched,
            "the admitted successor dispatch is recorded"
        );
        assert_eq!(
            obligation.successor_launch,
            Some(LaunchId("launch-2".into())),
            "successor bound (Appendix B CHECK)"
        );
        assert_eq!(
            obligation.predecessor,
            RunId("run-1".into()),
            "predecessor key"
        );
        assert_eq!(
            obligation.expires_at,
            Timestamp(1_000 + 86_400_000),
            "the policy expiry still stamps"
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
}
