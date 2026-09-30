//! F27 — the typed values of `catalog.toml`: the operating-point catalog and
//! the routing policy, plus the qualification evidence that gates capability
//! claims (F26). Harness kinds are catalog data here — never literals (N8,
//! ADR-0002).

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::identity::{AgentKind, Digest};

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

/// F26 — a qualified property an operating point offers; the catalog claims
/// it, `herdr-governor qualify` proves it, routing and delivery use only
/// current passes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Capability(pub String);

impl Capability {
    /// F26 — the canned Task starts the harness.
    pub const START: &'static str = "start";
    /// F26 — the harness acknowledges a prompt.
    pub const PROMPT_ACK: &'static str = "prompt_ack";
    /// F26 — `handoff_write`: the harness can write the handoff path.
    pub const HANDOFF_WRITE: &'static str = "handoff_write";
    /// F26 — `followup_read`: the harness reads a follow-up.
    pub const FOLLOWUP_READ: &'static str = "followup_read";
    /// F17 — `mid_turn_input`: follow-ups may be sent while the child works.
    pub const MID_TURN_INPUT: &'static str = "mid_turn_input";
    /// F18 — `hint_consumption`: the owner's pane consumes hint prompts.
    pub const HINT_CONSUMPTION: &'static str = "hint_consumption";

    /// The capability name as written in the catalog and qualifications.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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

/// F26/Appendix B `qualifications` — one capability's pass or fail for an
/// operating point, keyed by `(operating point, args digest)`; an args change
/// re-keys the row and invalidates the old pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qualification {
    /// The operating point that was qualified.
    pub operating_point: OperatingPointId,
    /// Digest of the exact arguments the qualification ran against.
    pub args_digest: Digest,
    /// The capability exercised.
    pub capability: Capability,
    /// Whether the point passed.
    pub passed: bool,
    /// `evidence_json` — the qualification's evidence payload.
    pub evidence: String,
}
