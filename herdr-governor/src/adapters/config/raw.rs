//! `raw` — the serde DTOs `catalog.toml` decodes into, before the mapped
//! `Config` meets `Config::validate` (the F27 authority — the adapter adds
//! no value rules of its own). Field names here are the file's spellings:
//! durations are bare seconds (`*_secs` — TOML has no duration type) and
//! rates are unit-interval floats. Every table is `deny_unknown_fields`: a
//! misspelled or stale key is a decode error, never a silently-dropped
//! setting — F27 fails closed. The file declares no `version`:
//! `ConfigVersion` is the sha256 of its bytes (OQ-7), stamped by the
//! caller of `into_config`.
//!
//! Required vs defaulted follows the core's own vocabulary: a field is
//! required iff `Policy` names no `DEFAULT_*` for it — `tiers`,
//! `provider_limit_threshold` and `cooldown_secs` must be written, the
//! three cap/floor tiers are `Option`s, and every defaulted bound decodes
//! to the `DEFAULT_*` the spec pins. Operating-point fields are all
//! required: an absent `args` or `capabilities` is a forgotten key, not a
//! bare-launch or no-claims statement — those are spelled `= []`.

use core::time::Duration;

use serde::Deserialize;

use governor_core::config::{
    Capability, Catalog, Config, ConfigVersion, CostClass, DEFAULT_EXPLORATION_RATE,
    DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY,
    DEFAULT_REPAIR_WINDOW, OperatingPoint, OperatingPointId, Policy, Provider, Tier,
};
use governor_core::identity::AgentKind;

/// The file's root table: `[policy]` and `[catalog]` are both required —
/// a catalog.toml without one of them is a broken file, not a partial one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawConfig {
    /// `[policy]` — the routing policy.
    policy: RawPolicy,
    /// `[catalog]` — the operating-point list.
    catalog: RawCatalog,
}

/// `[catalog]` — `operating_points` is required; `operating_points = []`
/// is the explicit empty catalog (the bring-up state F27 calls valid, not
/// a decode gap).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCatalog {
    /// `[[catalog.operating_points]]` in file order — catalog order is the
    /// F13 tiebreak after cost class.
    operating_points: Vec<RawOperatingPoint>,
}

/// One `[[catalog.operating_points]]` entry; every field is required —
/// `Config::validate` owns the emptiness/uniqueness rules, decode owns
/// presence.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOperatingPoint {
    /// The catalog id.
    id: String,
    /// The harness kind — opaque catalog data, never a literal (N8,
    /// ADR-0002).
    harness: String,
    /// The exact launch arguments persisted into each decision; `args =
    /// []` is a bare launch and `config::args_digest` pins whatever it was.
    args: Vec<String>,
    /// The served tier — must name a `policy.tiers` entry (validated).
    tier: String,
    /// Capability claims, each usable only under a current F26 pass;
    /// `capabilities = []` claims nothing.
    capabilities: Vec<String>,
    /// The F13 candidate-ordering class.
    cost_class: u32,
    /// The F21 cooldown unit.
    provider: String,
}

/// `[policy]` — ordered tiers, optional cap/floor references into them,
/// the unit-interval rates and the seconds-form duration bounds.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    /// F12 — the tier order `weakest_sufficient_tier` chooses over,
    /// weakest first.
    tiers: Vec<String>,
    /// F13 step 2 — the cap for a Task that changes no files and touches
    /// no security boundary.
    no_change_cap: Option<String>,
    /// F13 step 2 — the floor a security boundary raises.
    security_floor: Option<String>,
    /// F13 step 2 — the floor broad file changes raise.
    broad_change_floor: Option<String>,
    /// F21 — the `provider_limited` noul bound; no core default exists.
    provider_limit_threshold: f64,
    /// F13 step 5 — the exploration rate.
    #[serde(default = "default_exploration_rate")]
    exploration_rate: f64,
    /// F21 — pending-obligation lifetime, seconds.
    #[serde(default = "default_recovery_expiry_secs")]
    recovery_expiry_secs: u64,
    /// F21 — provider cooldown length, seconds; no core default exists.
    cooldown_secs: u64,
    /// F22 — `max_age_deadline` window, seconds.
    #[serde(default = "default_max_age_secs")]
    max_age_secs: u64,
    /// F24 — repair window after a first rejection, seconds.
    #[serde(default = "default_repair_window_secs")]
    repair_window_secs: u64,
    /// F24 — the Jev-unavailable bound after freezing, seconds.
    #[serde(default = "default_judgment_window_secs")]
    judgment_window_secs: u64,
    /// F25 — idle window, seconds.
    #[serde(default = "default_idle_window_secs")]
    idle_window_secs: u64,
}

impl RawConfig {
    /// Stamp `version` (sha256 of the file bytes — OQ-7) and map the DTOs
    /// onto the core `Config`; `Config::validate` still owns every value
    /// rule — nothing is checked here.
    pub(super) fn into_config(self, version: ConfigVersion) -> Config {
        Config {
            version,
            catalog: self.catalog.into_catalog(),
            policy: self.policy.into_policy(),
        }
    }
}

impl RawCatalog {
    fn into_catalog(self) -> Catalog {
        Catalog {
            operating_points: self
                .operating_points
                .into_iter()
                .map(RawOperatingPoint::into_point)
                .collect(),
        }
    }
}

impl RawOperatingPoint {
    fn into_point(self) -> OperatingPoint {
        OperatingPoint {
            id: OperatingPointId(self.id),
            harness: AgentKind(self.harness),
            args: self.args,
            tier: Tier(self.tier),
            capabilities: self.capabilities.into_iter().map(Capability).collect(),
            cost_class: CostClass(self.cost_class),
            provider: Provider(self.provider),
        }
    }
}

impl RawPolicy {
    fn into_policy(self) -> Policy {
        Policy {
            tiers: self.tiers.into_iter().map(Tier).collect(),
            no_change_cap: self.no_change_cap.map(Tier),
            security_floor: self.security_floor.map(Tier),
            broad_change_floor: self.broad_change_floor.map(Tier),
            provider_limit_threshold: self.provider_limit_threshold,
            exploration_rate: self.exploration_rate,
            recovery_expiry: Duration::from_secs(self.recovery_expiry_secs),
            cooldown: Duration::from_secs(self.cooldown_secs),
            max_age: Duration::from_secs(self.max_age_secs),
            repair_window: Duration::from_secs(self.repair_window_secs),
            judgment_window: Duration::from_secs(self.judgment_window_secs),
            idle_window: Duration::from_secs(self.idle_window_secs),
        }
    }
}

fn default_exploration_rate() -> f64 {
    DEFAULT_EXPLORATION_RATE
}

fn default_recovery_expiry_secs() -> u64 {
    DEFAULT_RECOVERY_EXPIRY.as_secs()
}

fn default_max_age_secs() -> u64 {
    DEFAULT_MAX_AGE.as_secs()
}

fn default_repair_window_secs() -> u64 {
    DEFAULT_REPAIR_WINDOW.as_secs()
}

fn default_judgment_window_secs() -> u64 {
    DEFAULT_JUDGMENT_WINDOW.as_secs()
}

fn default_idle_window_secs() -> u64 {
    DEFAULT_IDLE_WINDOW.as_secs()
}

#[cfg(test)]
mod tests {
    use governor_core::config::ConfigVersion;
    use governor_core::config::{
        DEFAULT_EXPLORATION_RATE, DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE,
        DEFAULT_RECOVERY_EXPIRY, DEFAULT_REPAIR_WINDOW,
    };

    use super::RawConfig;

    /// The smallest complete file: only the fields that carry no core
    /// `DEFAULT_*` (and the required `operating_points` key) are written.
    const MINIMAL: &str = r#"
[policy]
tiers = ["fast"]
provider_limit_threshold = 0.6
cooldown_secs = 60

[catalog]
operating_points = []
"#;

    /// One operating point with every field present.
    const POINT: &str = r#"
[[catalog.operating_points]]
id = "forge-pro"
harness = "forge"
args = ["--model", "pro"]
tier = "fast"
capabilities = ["start", "prompt_ack"]
cost_class = 3
provider = "vendor-a"
"#;

    /// `MINIMAL` plus one operating point.
    fn with_point() -> String {
        MINIMAL.replace("operating_points = []", "") + POINT
    }

    #[test]
    fn decodes_minimal_and_defaults_apply() {
        let raw: RawConfig = toml::from_str(MINIMAL).expect("minimal catalog decodes");
        let policy = raw.policy;
        assert_eq!(
            policy.tiers,
            Vec::from(["fast".to_string()]),
            "tiers decode in file order"
        );
        assert_eq!(
            policy.provider_limit_threshold.to_bits(),
            0.6f64.to_bits(),
            "required rate decodes bit-exactly"
        );
        assert_eq!(policy.cooldown_secs, 60, "required bound");
        assert_eq!(
            policy.exploration_rate.to_bits(),
            DEFAULT_EXPLORATION_RATE.to_bits(),
            "absent rate decodes to the core default"
        );
        assert_eq!(
            policy.recovery_expiry_secs,
            DEFAULT_RECOVERY_EXPIRY.as_secs(),
            "absent bound decodes to DEFAULT_RECOVERY_EXPIRY"
        );
        assert_eq!(
            policy.max_age_secs,
            DEFAULT_MAX_AGE.as_secs(),
            "absent bound decodes to DEFAULT_MAX_AGE"
        );
        assert_eq!(
            policy.repair_window_secs,
            DEFAULT_REPAIR_WINDOW.as_secs(),
            "absent bound decodes to DEFAULT_REPAIR_WINDOW"
        );
        assert_eq!(
            policy.judgment_window_secs,
            DEFAULT_JUDGMENT_WINDOW.as_secs(),
            "absent bound decodes to DEFAULT_JUDGMENT_WINDOW"
        );
        assert_eq!(
            policy.idle_window_secs,
            DEFAULT_IDLE_WINDOW.as_secs(),
            "absent bound decodes to DEFAULT_IDLE_WINDOW"
        );
        assert_eq!(policy.no_change_cap, None, "absent cap is None");
        assert_eq!(policy.security_floor, None, "absent floor is None");
        assert_eq!(policy.broad_change_floor, None, "absent floor is None");
        assert!(
            raw.catalog.operating_points.is_empty(),
            "the explicit empty catalog decodes empty"
        );
    }

    #[test]
    fn decodes_every_operating_point_field() {
        let raw: RawConfig = toml::from_str(&with_point()).expect("catalog with a point decodes");
        assert_eq!(raw.catalog.operating_points.len(), 1, "one point");
        let point = &raw.catalog.operating_points[0];
        assert_eq!(point.id, "forge-pro");
        assert_eq!(point.harness, "forge");
        assert_eq!(
            point.args,
            Vec::from(["--model".to_string(), "pro".to_string()])
        );
        assert_eq!(point.tier, "fast");
        assert_eq!(
            point.capabilities,
            Vec::from(["start".to_string(), "prompt_ack".to_string()])
        );
        assert_eq!(point.cost_class, 3);
        assert_eq!(point.provider, "vendor-a");
    }

    #[test]
    fn denies_unknown_fields_at_every_level() {
        for (name, doc) in [
            ("root", MINIMAL.replace("[policy]", "stray = 1\n\n[policy]")),
            (
                "policy",
                MINIMAL.replace("cooldown_secs", "mystery = 1\ncooldown_secs"),
            ),
            (
                "point",
                with_point().replace(
                    "provider = \"vendor-a\"",
                    "provider = \"vendor-a\"\nbogus = true",
                ),
            ),
        ] {
            match toml::from_str::<RawConfig>(&doc) {
                Err(error) => assert!(
                    error.to_string().contains("unknown field"),
                    "{name}: the decode error must name the unknown field: {error}"
                ),
                Ok(_) => panic!("{name}: an unknown field must fail the decode"),
            }
        }
    }

    #[test]
    fn missing_required_keys_error() {
        for (name, doc) in [
            (
                "policy table",
                "[catalog]\noperating_points = []\n".to_string(),
            ),
            ("policy.tiers", MINIMAL.replace("tiers = [\"fast\"]\n", "")),
            (
                "policy.provider_limit_threshold",
                MINIMAL.replace("provider_limit_threshold = 0.6\n", ""),
            ),
            (
                "policy.cooldown_secs",
                MINIMAL.replace("cooldown_secs = 60\n", ""),
            ),
            (
                "catalog.operating_points",
                MINIMAL.replace("operating_points = []", ""),
            ),
            ("point.id", with_point().replace("id = \"forge-pro\"\n", "")),
            (
                "point.cost_class",
                with_point().replace("cost_class = 3\n", ""),
            ),
        ] {
            assert!(
                toml::from_str::<RawConfig>(&doc).is_err(),
                "{name}: a missing required key must fail the decode"
            );
        }
    }

    #[test]
    fn wrong_types_error() {
        for (name, doc) in [
            (
                "string duration",
                MINIMAL.replace("cooldown_secs = 60", "cooldown_secs = \"1h\""),
            ),
            (
                "scalar tiers",
                MINIMAL.replace("tiers = [\"fast\"]", "tiers = \"fast\""),
            ),
            (
                "negative cost_class",
                with_point().replace("cost_class = 3", "cost_class = -1"),
            ),
            (
                "cost_class over u32",
                with_point().replace("cost_class = 3", "cost_class = 5000000000"),
            ),
            (
                "string threshold",
                MINIMAL.replace(
                    "provider_limit_threshold = 0.6",
                    "provider_limit_threshold = \"0.6\"",
                ),
            ),
        ] {
            assert!(
                toml::from_str::<RawConfig>(&doc).is_err(),
                "{name}: a mistyped value must fail the decode"
            );
        }
    }

    #[test]
    fn optional_tiers_decode() {
        let raw: RawConfig = toml::from_str(&MINIMAL.replace(
            "provider_limit_threshold",
            "no_change_cap = \"fast\"\nprovider_limit_threshold",
        ))
        .expect("decodes");
        assert_eq!(
            raw.policy.no_change_cap,
            Some("fast".to_string()),
            "a written cap is Some"
        );
    }

    #[test]
    fn into_config_maps_every_field() {
        let raw: RawConfig = toml::from_str(&with_point()).expect("decodes");
        let config = raw.into_config(ConfigVersion("v1".into()));
        assert_eq!(config.version.0, "v1", "the caller-stamped version lands");
        assert_eq!(config.policy.cooldown.as_secs(), 60, "secs map to Duration");
        assert_eq!(config.policy.tiers[0].0, "fast");
        let point = &config.catalog.operating_points[0];
        assert_eq!(point.id.0, "forge-pro");
        assert_eq!(point.harness.0, "forge", "harness lands as AgentKind");
        assert_eq!(point.tier.0, "fast");
        assert_eq!(point.cost_class.0, 3);
        assert_eq!(point.provider.0, "vendor-a");
        assert_eq!(point.capabilities.len(), 2, "capabilities map");
        assert_eq!(point.args.len(), 2, "args map verbatim");
    }
}
