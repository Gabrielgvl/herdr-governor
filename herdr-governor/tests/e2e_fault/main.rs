//! `e2e_fault` — the PR B fault suite's lane A: the §5 S1 child-daemon
//! kill matrix (`matrix` — four effect families across the four seam
//! boundaries) and the restart scenarios (`restart` — S27's startup
//! deadline sweep, S27b's killed close, S27d's stale-socket successor
//! and F28's pre-bind global scan), every case e2e against `FakeHerdr`
//! and `FakeJev` with the daemon a real `herdr-governor daemon` child
//! killed through `GOV_DAEMON_SEAM`'s `abort` action. `world`/`seed`
//! are the launch suite's fixture `#[path]`-included — the harness is
//! shared, never duplicated; `fixture` adds only the seeded-row builders
//! the restart scenarios need.

#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
mod fixture;
#[cfg(test)]
mod matrix;
#[cfg(test)]
mod restart;
#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
#[expect(
    dead_code,
    reason = "the launch suite's shared fixture — e2e_fault uses only its kill/restart-relevant helpers"
)]
#[path = "../e2e_launch/seed.rs"]
mod seed;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
#[expect(
    dead_code,
    reason = "the launch suite's shared fixture — e2e_fault uses only its kill/restart-relevant helpers"
)]
#[path = "../e2e_launch/world.rs"]
mod world;

#[cfg(test)]
use fixture::*;
#[cfg(test)]
use seed::*;
#[cfg(test)]
use world::*;

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use governor_core::identity::Timestamp;

/// Every wait's bound — the 1s tick plus wire round-trips land a launch
/// in a few seconds; this is the broken-pipeline backstop.
#[cfg(test)]
const DEADLINE: Duration = Duration::from_secs(20);
/// `reconcile_secs = 1` — the fastest cadence the catalog accepts.
#[cfg(test)]
const TICK_SECS: u64 = 1;
/// The envelope's `relayInstanceId` (32 lowercase hex; opaque).
#[cfg(test)]
const RELAY: &str = "abababababababababababababababab";
/// The caller's pane — `kind-a`/`caller-session` on `w1:p1`.
#[cfg(test)]
const CALLER_PANE: &str = "w1:p1";
/// A timestamp inside the store's RFC3339 range, before `now`.
#[cfg(test)]
const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// A deadline-bearing timestamp far past every window.
#[cfg(test)]
const FAR: Timestamp = Timestamp(4_102_444_800_000);
/// A deadline already elapsed at bring-up — the S27 sweep fires it
/// during startup, before the socket binds.
#[cfg(test)]
const PAST: Timestamp = Timestamp(1);
