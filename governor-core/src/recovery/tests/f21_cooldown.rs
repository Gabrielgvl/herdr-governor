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
