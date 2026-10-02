//! F27 — the typed values of `catalog.toml`: the operating-point catalog,
//! the routing policy, and the value rules `Config::validate` checks.
//! Harness kinds are catalog data here — never literals (N8, ADR-0002).

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::identity::AgentKind;

use super::Capability;

/// A quality tier — the stable contract between Task judgments and operating
/// points (CONTEXT). Order is defined by `Policy::tiers` position, not by the
/// name; `Tier` is deliberately not `Ord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier(pub String);

/// A provider whose quota limits its operating points (F21 cooldown unit).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Provider(pub String);

/// The catalog id of an operating point.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperatingPointId(pub String);

/// F13 — the relative cost of an operating point; candidates order by it,
/// then catalog order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CostClass(pub u32);

/// F27 — the catalog+policy version recorded into every Launch's decision
/// (`launches.config_version`); decisions never reload (F27).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigVersion(pub String);

/// F27 — one operating point: a harness plus the exact launch arguments,
/// rated by tier, capabilities, cost class and provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatingPoint {
    /// The catalog id.
    pub id: OperatingPointId,
    /// The harness kind it launches (catalog data — ADR-0002).
    pub harness: AgentKind,
    /// The exact start arguments persisted into each decision; changing them
    /// invalidates qualification (F26).
    pub args: Vec<String>,
    /// The tier this point serves.
    pub tier: Tier,
    /// The capabilities it claims (usable only with a current F26 pass).
    pub capabilities: Vec<Capability>,
    /// Its cost class (F13 candidate ordering).
    pub cost_class: CostClass,
    /// The provider whose quota limits it (F21 cooldown unit).
    pub provider: Provider,
}

/// F27 — the owner-authored operating-point list in `catalog.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    /// `catalog` order is the F13 tiebreak after cost class.
    pub operating_points: Vec<OperatingPoint>,
}

/// F13/F21–F25 — the owner-authored routing policy: the ordered tier list and
/// the rule values the judgments turn into a start tier and requirements.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// F12 — the policy tiers `weakest_sufficient_tier` chooses over, ordered
    /// weakest first.
    pub tiers: Vec<Tier>,
    /// F13 step 2 — the cap for a Task that changes no files and touches no
    /// security boundary.
    pub no_change_cap: Option<Tier>,
    /// F13 step 2 — the floor a security boundary raises.
    pub security_floor: Option<Tier>,
    /// F13 step 2 — the floor broad file changes raise.
    pub broad_change_floor: Option<Tier>,
    /// F21 — the `provider_limited` noul must clear this threshold before the
    /// Run settles `provider_limited`.
    pub provider_limit_threshold: f64,
    /// F13 step 5 — the exploration rate (`sha256(caller ‖ idempotencyKey)`
    /// below it lowers the start one tier); default `DEFAULT_EXPLORATION_RATE`.
    pub exploration_rate: f64,
    /// F21 — how long a pending recovery obligation lives before it fails
    /// `expired`; default `DEFAULT_RECOVERY_EXPIRY`.
    pub recovery_expiry: Duration,
    /// F21 — how long a provider's operating points stay in cooldown.
    pub cooldown: Duration,
    /// F22 — `max_age_deadline`, set at reserve; default `DEFAULT_MAX_AGE`.
    pub max_age: Duration,
    /// F24 — `repair_deadline`: 15 minutes after the first rejection in a work
    /// generation; default `DEFAULT_REPAIR_WINDOW`.
    pub repair_window: Duration,
    /// F24 — `judgment_deadline`: the Jev-unavailable bound after freezing;
    /// default `DEFAULT_JUDGMENT_WINDOW`.
    pub judgment_window: Duration,
    /// F25 — `idle_deadline`: 15 minutes after an idle episode begins; default
    /// `DEFAULT_IDLE_WINDOW`.
    pub idle_window: Duration,
}

/// F13 step 5 — the default exploration rate: 5%.
pub const DEFAULT_EXPLORATION_RATE: f64 = 0.05;

/// F21 — the default recovery-obligation expiry: 24 hours.
pub const DEFAULT_RECOVERY_EXPIRY: Duration = Duration::from_hours(24);

/// F22 — the default `max_age_deadline`: 24 hours.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_hours(24);

/// F24 — the default repair window: 15 minutes.
pub const DEFAULT_REPAIR_WINDOW: Duration = Duration::from_mins(15);

/// F24 — the default judgment window: 30 minutes.
pub const DEFAULT_JUDGMENT_WINDOW: Duration = Duration::from_mins(30);

/// F25 — the default idle window: 15 minutes.
pub const DEFAULT_IDLE_WINDOW: Duration = Duration::from_mins(15);

/// F22 — the `Config::validate` bound on every policy window: 10 years.
/// Any validated `now.after(window)` stays inside the store's year-9999
/// deadline horizon, so `Timestamp::after`'s saturation is a backstop for
/// unvalidated `Policy` values, not a live path. (`Duration::from_days`
/// is not a stable `const fn` on the pinned toolchain, hence hours.)
pub const MAX_POLICY_WINDOW: Duration = Duration::from_hours(3_650 * 24);

/// F27 — one loaded `catalog.toml`: the catalog, the policy and the version
/// every decision made under it records.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The version decisions record (`launches.config_version`).
    pub version: ConfigVersion,
    /// The operating-point catalog.
    pub catalog: Catalog,
    /// The routing policy.
    pub policy: Policy,
}

/// F27 — one invalid typed value found in a decoded `catalog.toml`. `field`
/// is the dotted path of the offending value (`policy.tiers`,
/// `catalog.operating_points[opus].provider`); the config adapter renders the
/// list into the startup refusal's one sanitized stderr line.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// A required value is absent: an empty or whitespace-only string, or an
    /// empty list where at least one entry is required.
    Missing {
        /// The dotted path of the missing value.
        field: String,
    },
    /// A value repeats where uniqueness is required.
    Duplicate {
        /// The dotted path of the repeated value.
        field: String,
        /// The repeated entry.
        value: String,
    },
    /// A tier reference names a tier `policy.tiers` does not declare.
    UnknownTier {
        /// The dotted path of the reference.
        field: String,
        /// The undeclared tier name.
        tier: String,
    },
    /// A rate or threshold is outside `[0, 1]` — NaN and the infinities are
    /// outside it too (TOML admits `nan`/`inf` literals; the range check is
    /// what rejects them).
    RateOutOfRange {
        /// The dotted path of the rate.
        field: String,
        /// The offending value.
        value: f64,
    },
    /// A duration bound is zero — a deadline that fires instantly or a
    /// cooldown that excludes nothing.
    ZeroDuration {
        /// The dotted path of the duration.
        field: String,
    },
    /// A duration bound exceeds `MAX_POLICY_WINDOW` — a 10-year-plus
    /// window is a config mistake, not a spelling of "no deadline".
    DurationTooLong {
        /// The dotted path of the duration.
        field: String,
    },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing { field } => {
                write!(formatter, "{field}: a required value is missing")
            }
            Self::Duplicate { field, value } => {
                write!(formatter, "{field}: '{value}' is duplicated")
            }
            Self::UnknownTier { field, tier } => {
                write!(
                    formatter,
                    "{field}: '{tier}' is not declared in policy.tiers"
                )
            }
            Self::RateOutOfRange { field, value } => {
                write!(formatter, "{field}: {value} is outside [0, 1]")
            }
            Self::ZeroDuration { field } => {
                write!(formatter, "{field}: duration must be positive")
            }
            Self::DurationTooLong { field } => {
                write!(
                    formatter,
                    "{field}: duration exceeds the 10-year policy bound"
                )
            }
        }
    }
}

impl core::error::Error for ConfigError {}

impl Config {
    /// F27 — validate the typed values of a decoded `catalog.toml`: tiers,
    /// operating points, capabilities, rates, deadlines and cooldown. The
    /// adapter owns TOML decoding; this owns the value rules and returns
    /// EVERY problem it finds, so an owner fixes one file rather than one
    /// error at a time. `Err` — keep the last good config on SIGHUP reload,
    /// or refuse startup (F27).
    ///
    /// Deliberately valid: an empty catalog (every Launch then abstains
    /// `no_candidates` — a bring-up state, not a malformed file), any `args`
    /// list (argv permits anything; the qualification digest pins whatever
    /// was launched), and a rate at either `0` or `1` (`0` disables, `1`
    /// always applies — reckless but expressible owner policy).
    pub fn validate(&self) -> Result<(), Vec<ConfigError>> {
        let mut errors = Vec::new();
        if self.version.0.trim().is_empty() {
            errors.push(ConfigError::Missing {
                field: "version".into(),
            });
        }
        validate_policy(&self.policy, &mut errors);
        validate_catalog(&self.catalog, &self.policy, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// F27 — the policy half: declared tiers, the cap/floor references into
/// them, the two unit-interval rates and the positive-duration bounds.
fn validate_policy(policy: &Policy, errors: &mut Vec<ConfigError>) {
    if policy.tiers.is_empty() {
        errors.push(ConfigError::Missing {
            field: "policy.tiers".into(),
        });
    }
    let mut declared = BTreeSet::new();
    for (index, tier) in policy.tiers.iter().enumerate() {
        if tier.0.trim().is_empty() {
            errors.push(ConfigError::Missing {
                field: format!("policy.tiers[{index}]"),
            });
        }
        if !declared.insert(&tier.0) {
            errors.push(ConfigError::Duplicate {
                field: format!("policy.tiers[{index}]"),
                value: tier.0.clone(),
            });
        }
    }
    for (field, floor) in [
        ("policy.no_change_cap", &policy.no_change_cap),
        ("policy.security_floor", &policy.security_floor),
        ("policy.broad_change_floor", &policy.broad_change_floor),
    ] {
        if let Some(named) = floor
            && !policy.tiers.contains(named)
        {
            errors.push(ConfigError::UnknownTier {
                field: field.into(),
                tier: named.0.clone(),
            });
        }
    }
    for (field, rate) in [
        (
            "policy.provider_limit_threshold",
            policy.provider_limit_threshold,
        ),
        ("policy.exploration_rate", policy.exploration_rate),
    ] {
        if !(0.0..=1.0).contains(&rate) {
            errors.push(ConfigError::RateOutOfRange {
                field: field.into(),
                value: rate,
            });
        }
    }
    for (field, bound) in [
        ("policy.recovery_expiry", policy.recovery_expiry),
        ("policy.cooldown", policy.cooldown),
        ("policy.max_age", policy.max_age),
        ("policy.repair_window", policy.repair_window),
        ("policy.judgment_window", policy.judgment_window),
        ("policy.idle_window", policy.idle_window),
    ] {
        if bound.is_zero() {
            errors.push(ConfigError::ZeroDuration {
                field: field.into(),
            });
        }
        if bound > MAX_POLICY_WINDOW {
            errors.push(ConfigError::DurationTooLong {
                field: field.into(),
            });
        }
    }
}

/// F27 — the catalog half: unique non-empty operating-point ids, declared
/// tiers, named harness and provider, and non-empty unique capability names.
fn validate_catalog(catalog: &Catalog, policy: &Policy, errors: &mut Vec<ConfigError>) {
    let mut ids = BTreeSet::new();
    for (index, point) in catalog.operating_points.iter().enumerate() {
        let base = point_path(index, point);
        if point.id.0.trim().is_empty() {
            errors.push(ConfigError::Missing {
                field: format!("{base}.id"),
            });
        }
        if !ids.insert(&point.id.0) {
            errors.push(ConfigError::Duplicate {
                field: format!("{base}.id"),
                value: point.id.0.clone(),
            });
        }
        if !policy.tiers.contains(&point.tier) {
            errors.push(ConfigError::UnknownTier {
                field: format!("{base}.tier"),
                tier: point.tier.0.clone(),
            });
        }
        if point.harness.0.trim().is_empty() {
            errors.push(ConfigError::Missing {
                field: format!("{base}.harness"),
            });
        }
        if point.provider.0.trim().is_empty() {
            errors.push(ConfigError::Missing {
                field: format!("{base}.provider"),
            });
        }
        let mut claimed = BTreeSet::new();
        for (position, capability) in point.capabilities.iter().enumerate() {
            if capability.0.trim().is_empty() {
                errors.push(ConfigError::Missing {
                    field: format!("{base}.capabilities[{position}]"),
                });
            }
            if !claimed.insert(&capability.0) {
                errors.push(ConfigError::Duplicate {
                    field: format!("{base}.capabilities[{position}]"),
                    value: capability.0.clone(),
                });
            }
        }
    }
}

/// The `catalog.operating_points[…]` path of a point — by its catalog id
/// when it has one, else by position (an empty id has nothing else to name
/// it by).
fn point_path(index: usize, point: &OperatingPoint) -> String {
    if point.id.0.trim().is_empty() {
        format!("catalog.operating_points[{index}]")
    } else {
        format!("catalog.operating_points[{}]", point.id.0)
    }
}
