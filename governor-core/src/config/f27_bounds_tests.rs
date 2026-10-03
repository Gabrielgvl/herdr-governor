use alloc::string::ToString as _;
use alloc::vec::Vec;
use core::time::Duration;

use crate::identity::AgentKind;

use super::{
    Catalog, Config, ConfigError, ConfigVersion, CostClass, DEFAULT_EXPLORATION_RATE,
    DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY,
    DEFAULT_REPAIR_WINDOW, MAX_POLICY_WINDOW, OperatingPoint, OperatingPointId,
    PROVIDER_NAME_MAX_BYTES, Policy, Provider, Tier,
};

/// A config that is valid but for the one policy window a test perturbs;
/// the empty catalog surfaces policy faults alone.
fn valid_config() -> Config {
    Config {
        version: ConfigVersion("catalog-v1".into()),
        catalog: Catalog {
            operating_points: Vec::new(),
        },
        policy: Policy {
            tiers: Vec::from([Tier("standard".into())]),
            no_change_cap: None,
            security_floor: None,
            broad_change_floor: None,
            provider_limit_threshold: 0.6,
            exploration_rate: DEFAULT_EXPLORATION_RATE,
            recovery_expiry: DEFAULT_RECOVERY_EXPIRY,
            cooldown: Duration::from_hours(1),
            max_age: DEFAULT_MAX_AGE,
            repair_window: DEFAULT_REPAIR_WINDOW,
            judgment_window: DEFAULT_JUDGMENT_WINDOW,
            idle_window: DEFAULT_IDLE_WINDOW,
        },
    }
}

/// Set the policy window `validate` reports as `field` to `bound`.
fn with_window(config: &mut Config, field: &str, bound: Duration) {
    match field {
        "policy.recovery_expiry" => config.policy.recovery_expiry = bound,
        "policy.cooldown" => config.policy.cooldown = bound,
        "policy.max_age" => config.policy.max_age = bound,
        "policy.repair_window" => config.policy.repair_window = bound,
        "policy.judgment_window" => config.policy.judgment_window = bound,
        "policy.idle_window" => config.policy.idle_window = bound,
        _ => panic!("test data names a real policy duration, got {field}"),
    }
}

#[test]
fn f27_duration_above_the_cap_is_refused() {
    assert_eq!(
        MAX_POLICY_WINDOW.as_secs(),
        315_360_000,
        "the cap is ten years (the owner ruling, §19)"
    );
    for field in [
        "policy.recovery_expiry",
        "policy.cooldown",
        "policy.max_age",
        "policy.repair_window",
        "policy.judgment_window",
        "policy.idle_window",
    ] {
        let mut at_cap = valid_config();
        with_window(&mut at_cap, field, MAX_POLICY_WINDOW);
        assert_eq!(at_cap.validate(), Ok(()), "{field} at the cap is valid");

        let mut over = valid_config();
        with_window(&mut over, field, MAX_POLICY_WINDOW + Duration::from_secs(1));
        assert_eq!(
            over.validate().unwrap_err(),
            Vec::from([ConfigError::DurationTooLong {
                field: field.into(),
            }]),
            "one second over the cap reports DurationTooLong on {field}"
        );
    }
}

/// An operating point on tier `standard` with `provider` — the catalog half
/// of a valid config, for the provider-name bound.
fn point(provider: &str) -> OperatingPoint {
    OperatingPoint {
        id: OperatingPointId("op-1".into()),
        harness: AgentKind("kind-1".into()),
        args: Vec::new(),
        tier: Tier("standard".into()),
        capabilities: Vec::new(),
        cost_class: CostClass(0),
        provider: Provider(provider.into()),
    }
}

#[test]
fn f27_provider_name_bounded_at_64_bytes() {
    assert_eq!(
        PROVIDER_NAME_MAX_BYTES, 64,
        "provider names are bounded at 64 bytes so the status cooldowns list fits its page budget (F7)"
    );
    let mut at = valid_config();
    at.catalog.operating_points = Vec::from([point(&"p".repeat(64))]);
    assert_eq!(at.validate(), Ok(()), "a 64-byte provider name is legal");
    let mut over = valid_config();
    over.catalog.operating_points = Vec::from([point(&"p".repeat(65))]);
    assert_eq!(
        over.validate().unwrap_err(),
        Vec::from([ConfigError::NameTooLong {
            field: "catalog.operating_points[op-1].provider".into(),
        }]),
        "65 bytes reports NameTooLong on the provider field"
    );
    // The bound is bytes, not characters.
    let mut wide = valid_config();
    wide.catalog.operating_points = Vec::from([point(&"é".repeat(33))]);
    assert_eq!(
        wide.validate().unwrap_err(),
        Vec::from([ConfigError::NameTooLong {
            field: "catalog.operating_points[op-1].provider".into(),
        }]),
        "33 two-byte characters exceed the byte bound"
    );
    let mut wide_ok = valid_config();
    wide_ok.catalog.operating_points = Vec::from([point(&"é".repeat(32))]);
    assert_eq!(
        wide_ok.validate(),
        Ok(()),
        "32 two-byte characters are exactly 64 bytes"
    );
}

#[test]
fn f27_duration_too_long_display_names_the_bound() {
    let rendered = ConfigError::DurationTooLong {
        field: "policy.cooldown".into(),
    }
    .to_string();
    assert!(
        rendered.contains("policy.cooldown"),
        "display names the field: {rendered}"
    );
    assert!(
        rendered.contains("10-year policy bound"),
        "display names the bound: {rendered}"
    );
}
