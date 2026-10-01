//! F21 — `recoveryOf` admission: the successor Launch's derivation (key and
//! Task), the fresh-`absent` observation gate (ADR-0003) and the caller
//! admission rules.

use crate::identity::{
    AgentKind, CallerKey, ChildStatus, LaunchId, NativeSession, Observation, RunId, Timestamp,
};
use crate::lifecycle::Settlement;
use crate::recovery::{
    RECOVERY_KEY_PREFIX, RecoveryOrigin, RecoveryStatus, caller_admission, dispatch_ready,
    successor_key, successor_task,
};
use crate::task::Refusal;

use super::builders::{admitted, caller, obligation, policy, run, task, unique};

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
