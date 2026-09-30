use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::time::Duration;

use super::{
    Capability, Catalog, Config, ConfigError, ConfigVersion, CostClass, DEFAULT_EXPLORATION_RATE,
    DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY,
    DEFAULT_REPAIR_WINDOW, OperatingPoint, OperatingPointId, Policy, Provider, Qualification, Tier,
};
use crate::identity::AgentKind;

fn hex(bytes: &[u8; 32]) -> String {
    let mut out = String::new();
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

fn valid_policy() -> Policy {
    Policy {
        tiers: Vec::from([
            Tier("fast".into()),
            Tier("standard".into()),
            Tier("frontier".into()),
        ]),
        no_change_cap: Some(Tier("standard".into())),
        security_floor: Some(Tier("frontier".into())),
        broad_change_floor: Some(Tier("standard".into())),
        provider_limit_threshold: 0.6,
        exploration_rate: DEFAULT_EXPLORATION_RATE,
        recovery_expiry: DEFAULT_RECOVERY_EXPIRY,
        cooldown: Duration::from_hours(1),
        max_age: DEFAULT_MAX_AGE,
        repair_window: DEFAULT_REPAIR_WINDOW,
        judgment_window: DEFAULT_JUDGMENT_WINDOW,
        idle_window: DEFAULT_IDLE_WINDOW,
    }
}

fn valid_point(id: &str) -> OperatingPoint {
    OperatingPoint {
        id: OperatingPointId(id.into()),
        harness: AgentKind("harness-1".into()),
        args: Vec::from(["--model".into(), "x".into()]),
        tier: Tier("standard".into()),
        capabilities: Vec::from([Capability("web_access".into())]),
        cost_class: CostClass(1),
        provider: Provider("provider-1".into()),
    }
}

fn valid_config() -> Config {
    Config {
        version: ConfigVersion("catalog-v1".into()),
        catalog: Catalog {
            operating_points: Vec::from([valid_point("opus")]),
        },
        policy: valid_policy(),
    }
}

/// A config whose catalog is empty, so policy faults surface alone.
fn policy_only_config() -> Config {
    Config {
        catalog: Catalog {
            operating_points: Vec::new(),
        },
        ..valid_config()
    }
}

/// A `passed` qualification row for `point`'s CURRENT args (F26 key).
fn pass(point: &OperatingPoint, capability: &str) -> Qualification {
    Qualification {
        operating_point: point.id.clone(),
        args_digest: point.args_digest(),
        capability: Capability(capability.into()),
        passed: true,
        evidence: "{}".into(),
    }
}

#[test]
fn f27_valid_config_validates() {
    assert_eq!(
        valid_config().validate(),
        Ok(()),
        "a well-formed catalog and policy must pass F27"
    );
}

#[test]
fn f27_empty_catalog_is_valid() {
    assert_eq!(
        policy_only_config().validate(),
        Ok(()),
        "an empty catalog means every Launch abstains; it is not malformed"
    );
}

#[test]
fn f27_version_must_be_present() {
    let mut config = valid_config();
    config.version = ConfigVersion(" ".into());
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([ConfigError::Missing {
            field: "version".into(),
        }]),
        "an empty version string is rejected"
    );
}

#[test]
fn f27_policy_must_declare_a_tier() {
    let mut config = policy_only_config();
    config.policy.tiers.clear();
    config.policy.no_change_cap = None;
    config.policy.security_floor = None;
    config.policy.broad_change_floor = None;
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([ConfigError::Missing {
            field: "policy.tiers".into(),
        }]),
        "an empty tier list leaves weakest_sufficient_tier nothing to choose"
    );
}

#[test]
fn f27_tier_names_must_be_nonempty_and_unique() {
    let mut blank = policy_only_config();
    blank.policy.tiers.push(Tier("   ".into()));
    assert_eq!(
        blank.validate().unwrap_err(),
        Vec::from([ConfigError::Missing {
            field: "policy.tiers[3]".into(),
        }]),
        "a whitespace-only tier name is rejected"
    );

    let mut duplicate = policy_only_config();
    duplicate.policy.tiers.push(Tier("standard".into()));
    assert_eq!(
        duplicate.validate().unwrap_err(),
        Vec::from([ConfigError::Duplicate {
            field: "policy.tiers[3]".into(),
            value: "standard".into(),
        }]),
        "a repeated tier name is rejected — tier order would be ambiguous"
    );
}

#[test]
fn f27_cap_and_floor_tiers_must_be_declared() {
    let mut config = policy_only_config();
    config.policy.no_change_cap = Some(Tier("ghost".into()));
    config.policy.security_floor = Some(Tier("ghost".into()));
    config.policy.broad_change_floor = Some(Tier("ghost".into()));
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([
            ConfigError::UnknownTier {
                field: "policy.no_change_cap".into(),
                tier: "ghost".into(),
            },
            ConfigError::UnknownTier {
                field: "policy.security_floor".into(),
                tier: "ghost".into(),
            },
            ConfigError::UnknownTier {
                field: "policy.broad_change_floor".into(),
                tier: "ghost".into(),
            },
        ]),
        "every cap/floor reference must resolve to a declared tier"
    );
}

#[test]
fn f27_rates_stay_finite_inside_unit_interval() {
    for bad in [f64::NAN, f64::INFINITY, -0.25, 1.5] {
        let mut config = policy_only_config();
        config.policy.provider_limit_threshold = bad;
        config.policy.exploration_rate = bad;
        let errors = config.validate().unwrap_err();
        assert_eq!(
            errors.len(),
            2,
            "rate {bad} must fail both policy fields: {errors:?}"
        );
        for (error, path) in errors
            .iter()
            .zip(["policy.provider_limit_threshold", "policy.exploration_rate"])
        {
            assert!(
                matches!(error, ConfigError::RateOutOfRange { field, value } if field.as_str() == path && value.to_bits() == bad.to_bits()),
                "rate {bad} produced {error:?} where {path} expected"
            );
        }
    }
}

#[test]
fn f27_rate_endpoints_are_valid() {
    for rate in [0.0, 1.0] {
        let mut config = policy_only_config();
        config.policy.provider_limit_threshold = rate;
        config.policy.exploration_rate = rate;
        assert_eq!(
            config.validate(),
            Ok(()),
            "rate {rate} is inside [0, 1] and must validate"
        );
    }
}

#[test]
fn f27_durations_must_be_positive() {
    for field in [
        "policy.recovery_expiry",
        "policy.cooldown",
        "policy.max_age",
        "policy.repair_window",
        "policy.judgment_window",
        "policy.idle_window",
    ] {
        let mut config = policy_only_config();
        match field {
            "policy.recovery_expiry" => config.policy.recovery_expiry = Duration::ZERO,
            "policy.cooldown" => config.policy.cooldown = Duration::ZERO,
            "policy.max_age" => config.policy.max_age = Duration::ZERO,
            "policy.repair_window" => config.policy.repair_window = Duration::ZERO,
            "policy.judgment_window" => config.policy.judgment_window = Duration::ZERO,
            "policy.idle_window" => config.policy.idle_window = Duration::ZERO,
            _ => panic!("test data names a real policy duration, got {field}"),
        }
        assert_eq!(
            config.validate().unwrap_err(),
            Vec::from([ConfigError::ZeroDuration {
                field: field.into(),
            }]),
            "a zero {field} is a deadline that fires instantly or a cooldown that excludes nothing"
        );
    }
}

#[test]
fn f27_operating_point_ids_must_be_nonempty_and_unique() {
    let mut empty = valid_config();
    empty.catalog.operating_points.push(OperatingPoint {
        id: OperatingPointId(String::new()),
        ..valid_point("unused")
    });
    assert_eq!(
        empty.validate().unwrap_err(),
        Vec::from([ConfigError::Missing {
            field: "catalog.operating_points[1].id".into(),
        }]),
        "an empty id is named by position — it has nothing else"
    );

    let mut duplicate = valid_config();
    duplicate.catalog.operating_points.push(valid_point("opus"));
    assert_eq!(
        duplicate.validate().unwrap_err(),
        Vec::from([ConfigError::Duplicate {
            field: "catalog.operating_points[opus].id".into(),
            value: "opus".into(),
        }]),
        "a repeated operating-point id is rejected — qualification keys would collide"
    );
}

#[test]
fn f27_operating_point_tier_must_be_declared() {
    let mut config = valid_config();
    config.catalog.operating_points[0].tier = Tier("ghost".into());
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([ConfigError::UnknownTier {
            field: "catalog.operating_points[opus].tier".into(),
            tier: "ghost".into(),
        }]),
        "a point's tier must name a declared policy tier"
    );
}

#[test]
fn f27_provider_and_harness_must_be_named() {
    let mut point = valid_point("opus");
    point.provider = Provider(" ".into());
    point.harness = AgentKind(String::new());
    let mut config = valid_config();
    config.catalog.operating_points = Vec::from([point]);
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([
            ConfigError::Missing {
                field: "catalog.operating_points[opus].harness".into(),
            },
            ConfigError::Missing {
                field: "catalog.operating_points[opus].provider".into(),
            },
        ]),
        "an unnamed harness cannot launch; an unnamed provider breaks cooldown scoping"
    );
}

#[test]
fn f27_capability_names_must_be_nonempty_and_unique() {
    let mut point = valid_point("opus");
    point.capabilities = Vec::from([
        Capability("web_access".into()),
        Capability(String::new()),
        Capability("web_access".into()),
    ]);
    let mut config = valid_config();
    config.catalog.operating_points = Vec::from([point]);
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([
            ConfigError::Missing {
                field: "catalog.operating_points[opus].capabilities[1]".into(),
            },
            ConfigError::Duplicate {
                field: "catalog.operating_points[opus].capabilities[2]".into(),
                value: "web_access".into(),
            },
        ]),
        "capability claims must be non-empty names, claimed once each"
    );
}

#[test]
fn f27_validation_reports_every_error() {
    let mut config = valid_config();
    config.policy.exploration_rate = 2.0;
    config.catalog.operating_points[0].tier = Tier("ghost".into());
    config.catalog.operating_points[0].provider = Provider(String::new());
    assert_eq!(
        config.validate().unwrap_err(),
        Vec::from([
            ConfigError::RateOutOfRange {
                field: "policy.exploration_rate".into(),
                value: 2.0,
            },
            ConfigError::UnknownTier {
                field: "catalog.operating_points[opus].tier".into(),
                tier: "ghost".into(),
            },
            ConfigError::Missing {
                field: "catalog.operating_points[opus].provider".into(),
            },
        ]),
        "one Err carries every problem found, in validation order"
    );
}

#[test]
fn f27_error_display_names_field_and_value() {
    let rendered = ConfigError::UnknownTier {
        field: "policy.security_floor".into(),
        tier: "t9".into(),
    }
    .to_string();
    assert!(
        rendered.contains("policy.security_floor"),
        "display names the field: {rendered}"
    );
    assert!(
        rendered.contains("t9"),
        "display names the tier: {rendered}"
    );
}

#[test]
fn f26_capability_counts_only_with_a_current_pass() {
    let point = valid_point("opus");
    let capability = Capability("web_access".into());
    // A second, non-matching row rides along so `all` cannot pass for `any`.
    let unrelated = Qualification {
        capability: Capability("other".into()),
        ..pass(&point, "other")
    };
    assert!(
        point.has_current_pass(&capability, &[pass(&point, "web_access"), unrelated]),
        "a claimed capability with a passed qualification under the current args counts"
    );
}

#[test]
fn f26_args_change_invalidates_the_pass() {
    let qualified = valid_point("opus");
    let capability = Capability("web_access".into());
    let qualifications = Vec::from([pass(&qualified, "web_access")]);
    assert!(
        qualified.has_current_pass(&capability, &qualifications),
        "the pass counts while the args are unchanged"
    );

    let mut edited = qualified.clone();
    edited.args = Vec::from(["--model".into(), "y".into()]);
    assert!(
        !edited.has_current_pass(&capability, &qualifications),
        "an args change re-keys the digest and orphans the old pass"
    );
}

#[test]
fn f26_only_a_pass_counts() {
    let point = valid_point("opus");
    let capability = Capability("web_access".into());
    let mut failed = pass(&point, "web_access");
    failed.passed = false;
    assert!(
        !point.has_current_pass(&capability, &[failed]),
        "a failed qualification does not count"
    );
    assert!(
        !point.has_current_pass(&capability, &[]),
        "a never-qualified point offers nothing — no current pass exists"
    );
}

#[test]
fn f26_unclaimed_capability_does_not_count() {
    // The pass exists and is current, but the catalog no longer claims
    // the capability — the owner revoked the offer by editing the claim.
    let mut point = valid_point("opus");
    point.capabilities = Vec::from([Capability("other".into())]);
    let capability = Capability("web_access".into());
    assert!(
        !point.has_current_pass(&capability, &[pass(&point, "web_access")]),
        "a current pass on an unclaimed capability must not count"
    );
}

#[test]
fn f26_pass_is_keyed_per_point_and_capability() {
    let point = valid_point("opus");
    let capability = Capability("web_access".into());
    let other_point = Qualification {
        operating_point: OperatingPointId("mini".into()),
        ..pass(&point, "web_access")
    };
    assert!(
        !point.has_current_pass(&capability, &[other_point]),
        "another operating point's pass does not count"
    );
    let other_capability = pass(&point, "other");
    assert!(
        !point.has_current_pass(&capability, &[other_capability]),
        "another capability's pass does not count"
    );
}

#[test]
fn f26_args_digest_encoding_is_unambiguous() {
    let mut left = valid_point("a");
    left.args = Vec::from(["a".into(), "bc".into()]);
    let mut right = valid_point("a");
    right.args = Vec::from(["ab".into(), "c".into()]);
    assert_ne!(
        left.args_digest(),
        right.args_digest(),
        "concatenation-equal arg lists must not share a digest"
    );
    let mut solo = valid_point("a");
    solo.args = Vec::from(["abc".into()]);
    assert_ne!(
        left.args_digest(),
        solo.args_digest(),
        "the length prefix separates list boundaries"
    );

    // sha256( len-le ‖ bytes … ) of ["--model", "x"] — pins the encoding.
    assert_eq!(
        hex(&valid_point("opus").args_digest().0),
        "bc7b6462018079c9fe3534c0e3f4a05ec7f0ef0e1414a9fb282632e92524b8cf",
        "the args digest pins the sha-256 length-prefixed encoding"
    );
}
