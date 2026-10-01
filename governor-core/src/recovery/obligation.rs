//! F21 — the `recoveries` record (Appendix B): one obligation per settled
//! predecessor (`predecessor_run_id` is the primary key — `RECOVERY_EXISTS`
//! refuses a second) and its `pending` → `dispatched` | `blocked` |
//! `failed` state machine.

use alloc::string::String;
use core::time::Duration;

use crate::identity::{LaunchId, RunId, Timestamp};
use crate::task::AbstainReason;

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

/// F21/Appendix B `recoveries` — one obligation per settled predecessor.
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

impl RecoveryObligation {
    /// F21 — a freshly recorded obligation: `pending`, unclaimed, expiring
    /// `expiry` after `now` (the policy expiry, `Policy::recovery_expiry`;
    /// default `DEFAULT_RECOVERY_EXPIRY` = 24 h).
    #[must_use]
    pub fn pending(
        predecessor: RunId,
        origin: RecoveryOrigin,
        now: Timestamp,
        expiry: Duration,
    ) -> Self {
        Self {
            predecessor,
            origin,
            status: RecoveryStatus::Pending,
            reason: None,
            successor_launch: None,
            expires_at: now.after(expiry),
        }
    }

    /// F21 — `pending` → `dispatched`: the successor Launch was admitted
    /// (Appendix B CHECK — `successor_launch_id` is then required). The
    /// terminal states absorb: a non-pending obligation returns `None`.
    #[must_use]
    pub fn dispatched(&self, successor: LaunchId) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Dispatched,
                successor_launch: Some(successor),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — `pending` → `blocked`: the successor Launch abstained; `reason`
    /// records the abstention (`no_candidates`, `no_higher_tier`, ...).
    #[must_use]
    pub fn blocked(&self, reason: AbstainReason) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Blocked,
                reason: Some(reason.as_str().into()),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — `pending` → `failed` with `reason` (e.g. `expired`).
    #[must_use]
    pub fn failed(&self, reason: String) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Failed,
                reason: Some(reason),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — a still-`pending` obligation at or past `expires_at` fails
    /// `expired`; every other input leaves it untouched.
    #[must_use]
    pub fn expired(&self, now: Timestamp) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending if now.0 >= self.expires_at.0 => {
                self.failed(String::from("expired"))
            }
            RecoveryStatus::Pending
            | RecoveryStatus::Blocked
            | RecoveryStatus::Dispatched
            | RecoveryStatus::Failed => None,
        }
    }
}
