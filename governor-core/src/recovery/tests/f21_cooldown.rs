//! F21 — the `cooldowns` record: the `limited` stamp a `provider_limited`
//! settlement writes and the merge that only ever lengthens `until`.

use core::time::Duration;

use crate::config::Provider;
use crate::identity::{RunId, Timestamp};
use crate::recovery::Cooldown;

#[test]
fn f21_cooldown_limited_fields() {
    let cooldown = Cooldown::limited(
        Provider("prov-1".into()),
        RunId("run-1".into()),
        Timestamp(1_000),
        Duration::from_hours(1),
        None,
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
fn f21_cooldown_without_reset_is_the_policy_window() {
    let cooldown = Cooldown::limited(
        Provider("prov-1".into()),
        RunId("run-1".into()),
        Timestamp(1_000),
        Duration::from_hours(1),
        None,
    );
    assert_eq!(
        cooldown.until,
        Timestamp(1_000 + 3_600_000),
        "no stated reset — the policy window alone (OQ-X)"
    );
}

#[test]
fn f21_cooldown_reset_inside_the_window_keeps_the_policy_end() {
    let cooldown = Cooldown::limited(
        Provider("prov-1".into()),
        RunId("run-1".into()),
        Timestamp(1_000),
        Duration::from_hours(1),
        Some(Timestamp(1_000 + 60_000)),
    );
    assert_eq!(
        cooldown.until,
        Timestamp(1_000 + 3_600_000),
        "a reset before the policy end never shortens the cooldown"
    );
    let at_the_end = Cooldown::limited(
        Provider("prov-1".into()),
        RunId("run-1".into()),
        Timestamp(1_000),
        Duration::from_hours(1),
        Some(Timestamp(1_000 + 3_600_000)),
    );
    assert_eq!(
        at_the_end.until,
        Timestamp(1_000 + 3_600_000),
        "a reset exactly at the policy end changes nothing"
    );
}

#[test]
fn f21_cooldown_reset_beyond_the_window_runs_until_the_reset() {
    let cooldown = Cooldown::limited(
        Provider("prov-1".into()),
        RunId("run-1".into()),
        Timestamp(1_000),
        Duration::from_hours(1),
        Some(Timestamp(1_000 + 7_200_000)),
    );
    assert_eq!(
        cooldown.until,
        Timestamp(1_000 + 7_200_000),
        "a stated reset later than the policy end is the cooldown's until (OQ-X)"
    );
    // The lengthened candidate still merges only upward.
    let existing = Cooldown {
        provider: Provider("prov-1".into()),
        until: Timestamp(1_000 + 9_000_000),
        reason: "first".into(),
        source_run: Some(RunId("run-0".into())),
    };
    let merged = existing.merged(cooldown);
    assert_eq!(
        merged.until,
        Timestamp(1_000 + 9_000_000),
        "a longer existing exclusion still wins the merge"
    );
}
