//! F21 — recovery obligations and provider cooldowns (ADR-0003): a
//! `provider_limited` settlement creates exactly one obligation; dispatch
//! waits for fresh proof the predecessor's identity is absent. Panes are
//! never closed automatically.

use alloc::string::String;

use crate::config::Provider;
use crate::identity::{LaunchId, RunId, Timestamp};

/// F21 — the successor Launch's idempotency key is
/// `"recovery:" ++ predecessorRunId`, which makes one recovery per
/// predecessor and never collides with a caller key.
pub const RECOVERY_KEY_PREFIX: &str = "recovery:";

/// F21/Appendix B `recoveries.origin` — where the obligation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryOrigin {
    /// `provider_limit` — created by a `provider_limited` settlement.
    ProviderLimit,
    /// `caller` — requested with `recoveryOf` on a successor Launch (F21).
    Caller,
}

impl RecoveryOrigin {
    /// Appendix B — the stored spelling of the origin.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProviderLimit => "provider_limit",
            Self::Caller => "caller",
        }
    }
}

/// F21/Appendix B `recoveries.state` — the obligation's lifecycle:
/// `pending` → `dispatched` | `blocked` | `failed`; a still-pending
/// obligation past `expires_at` fails `expired` (recorded in `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryStatus {
    /// `pending` — waiting for fresh proof the predecessor is absent.
    Pending,
    /// `blocked` — the successor Launch abstained, no candidates (F21).
    Blocked,
    /// `dispatched` — the successor Launch was admitted (Appendix B CHECK:
    /// `successor_launch_id` is then required).
    Dispatched,
    /// `failed` — the obligation could not be satisfied (e.g. `expired`).
    Failed,
}

impl RecoveryStatus {
    /// Appendix B — the stored spelling of the state.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Dispatched => "dispatched",
            Self::Failed => "failed",
        }
    }
}

/// F21/Appendix B `recoveries` — one obligation per settled predecessor
/// (`predecessor_run_id` is the primary key — `RECOVERY_EXISTS` refuses a
/// second).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryObligation {
    /// The settled Run it continues.
    pub predecessor: RunId,
    /// `origin`.
    pub origin: RecoveryOrigin,
    /// `state`.
    pub status: RecoveryStatus,
    /// `reason` — e.g. `expired` for a still-pending obligation past
    /// `expires_at` (F21).
    pub reason: Option<String>,
    /// `successor_launch_id` — the Launch the dispatch admitted; required
    /// iff `status` is `dispatched` (Appendix B CHECK).
    pub successor_launch: Option<LaunchId>,
    /// `expires_at` — the policy expiry (default `DEFAULT_RECOVERY_EXPIRY`).
    pub expires_at: Timestamp,
}

/// F21/Appendix B `cooldowns` — one provider's exclusion period. Upserts keep
/// the later `until`: cooldowns only ever lengthen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cooldown {
    /// `provider` — the excluded provider (primary key).
    pub provider: Provider,
    /// `until` — the absolute expiry of the exclusion.
    pub until: Timestamp,
    /// `reason` — why the provider was limited.
    pub reason: String,
    /// `source_run_id` — the Run whose `provider_limited` settlement caused
    /// it, when known.
    pub source_run: Option<RunId>,
}
