use alloc::string::ToString as _;
use alloc::vec::Vec;
use core::time::Duration;

use super::{
    Catalog, Config, ConfigError, ConfigVersion, DEFAULT_EXPLORATION_RATE, DEFAULT_IDLE_WINDOW,
    DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY, DEFAULT_REPAIR_WINDOW,
    MAX_POLICY_WINDOW, Policy, Tier,
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
