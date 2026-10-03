//! `health` — the in-memory Herdr freshness read (§4.7): when the last
//! good snapshot landed, the incarnation it was taken under, and the
//! current verdict. Runtime observation only — never journaled.

use governor_core::identity::{HerdrIncarnation, Timestamp};

use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};

use crate::daemon::identity::incarnation;

/// The in-memory health read A2's `herdr_status` renders: when the last
/// good snapshot landed, the incarnation it was taken under, and the
/// current verdict. Runtime observation only — never journaled.
#[derive(Debug, Default)]
pub(in crate::daemon) struct HerdrHealth {
    last_ok: Option<Timestamp>,
    incarnation: Option<HerdrIncarnation>,
    state: HealthState,
}

/// The health verdict for the status surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::daemon) enum HealthState {
    /// No read has answered yet.
    #[default]
    Unknown,
    /// The latest snapshot answered.
    Fresh,
    /// A non-connect read failed after an earlier success — degraded but
    /// previously reachable.
    Degraded,
    /// A connect-level failure (the socket refused/absent), or no read
    /// ever succeeded — the A2 ruling's "server gone" state.
    Gone,
}

impl HerdrHealth {
    /// Fold one snapshot result into the health state: `Ok` refreshes
    /// `last_ok`/`incarnation`; a `Connect` failure is `gone`; any other
    /// error degrades once a good read exists, else reports `gone`.
    pub(in crate::daemon) fn record(
        &mut self,
        snapshot: &Result<Observed<SessionSnapshot>, HerdrError>,
        now: Timestamp,
    ) {
        match snapshot {
            Ok(observed) => {
                self.last_ok = Some(now);
                self.incarnation = Some(incarnation(&observed.epoch));
                self.state = HealthState::Fresh;
            }
            Err(HerdrError::Connect(_)) => self.state = HealthState::Gone,
            Err(_) => {
                self.state = if self.last_ok.is_some() {
                    HealthState::Degraded
                } else {
                    HealthState::Gone
                };
            }
        }
    }

    /// `freshSecsAgo` — whole seconds since the last successful snapshot;
    /// `None` until one lands. The status page measures the same delta
    /// off `last_ok`; this accessor is the tests' read.
    #[cfg(test)]
    pub(in crate::daemon) fn fresh_secs_ago(&self, now: Timestamp) -> Option<u64> {
        self.last_ok.map(|at| {
            u64::try_from(now.0.saturating_sub(at.0).saturating_div(1_000)).unwrap_or(u64::MAX)
        })
    }

    /// The current verdict — the tests' read (§4.12's page renders only
    /// freshness + incarnation, not the verdict).
    #[cfg(test)]
    pub(in crate::daemon) fn state(&self) -> HealthState {
        self.state
    }

    /// When the last good snapshot landed — the stamp F7's
    /// `herdr.freshSecsAgo` measures from (`None` until one lands).
    pub(in crate::daemon) fn last_ok(&self) -> Option<Timestamp> {
        self.last_ok
    }

    /// The incarnation the last good snapshot was taken under — F7's
    /// `herdr.incarnation` (`None` until one lands).
    pub(in crate::daemon) fn incarnation(&self) -> Option<&HerdrIncarnation> {
        self.incarnation.as_ref()
    }
}
