//! F21 — the `recoveries` obligation record: the stored `origin`/`state`
//! spellings and the `pending` → `dispatched` | `blocked` | `failed`
//! lifecycle, including expiry.

use core::time::Duration;

use crate::identity::{LaunchId, RunId, Timestamp};
use crate::recovery::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
use crate::task::AbstainReason;

use super::builders::obligation;

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
    let dispatched = obligation(RecoveryStatus::Pending).dispatched(LaunchId("launch-2".into()));
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

// `Config::validate` bounds every policy window to MAX_POLICY_WINDOW (10
// years), so no duration that entered through config reaches this path —
// the test builds unvalidated values directly to pin `Timestamp::after`'s
// backstop: saturate rather than wrap a deadline into the past.
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
