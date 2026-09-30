//! F21 — the `cooldowns` record (Appendix B): one provider's exclusion
//! period. Upserts keep the later `until`: cooldowns only ever lengthen.

use alloc::string::String;
use core::time::Duration;

use crate::config::Provider;
use crate::identity::{RunId, Timestamp};
use crate::lifecycle::Settlement;

use super::obligation::after;

/// F21/Appendix B `cooldowns` — one provider's exclusion period.
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

impl Cooldown {
    /// F21 — the exclusion a `provider_limited` settlement records: `until`
    /// is `now + duration` (the policy cooldown window, `Policy::cooldown`).
    #[must_use]
    pub fn limited(
        provider: Provider,
        source_run: RunId,
        now: Timestamp,
        duration: Duration,
    ) -> Self {
        Self {
            provider,
            until: after(now, duration),
            reason: Settlement::ProviderLimited.as_str().into(),
            source_run: Some(source_run),
        }
    }

    /// F21 — merge a newly observed limit into this provider's cooldown:
    /// `until` only ever lengthens (Appendix B upsert keeps `max(existing,
    /// new)`), and the winning exclusion keeps its own `reason`/`source_run`.
    /// Both inputs share the provider — it is the row's key.
    #[must_use]
    pub fn merged(&self, candidate: Cooldown) -> Self {
        if candidate.until > self.until {
            candidate
        } else {
            self.clone()
        }
    }
}
