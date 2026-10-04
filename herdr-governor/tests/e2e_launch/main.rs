//! `e2e_launch` — the launch pipeline end to end. P5.B1's §4.4 dispatch
//! cases (`dispatch*`) seed `planned` effects directly; P5.B2's cases
//! drive `herdr_launch` through `McpClient` against `FakeHerdr` +
//! `FakeJev`: admission and the F6 base (`pipeline`), F11 idempotency
//! (`idempotency`), F12 evaluation (`evaluation`), F13/F14 routing and
//! placement (`routing`), F15 start and fallback (`start`), F16 prompt
//! (`prompt`), the F21 recovery hooks (`recovery`) and the §4.5
//! convergence table (`convergence`). `world` is the fixture, `seed` the
//! store rows/reads. `reconcile_secs = 1` keeps the tick on a
//! one-second cadence; every wait is a bounded poll, nothing sleeps on
//! the wall clock.

#[cfg(test)]
mod convergence;
#[cfg(test)]
mod dispatch;
#[cfg(test)]
mod evaluation;
#[cfg(test)]
mod idempotency;
#[cfg(test)]
mod pipeline;
#[cfg(test)]
mod prompt;
#[cfg(test)]
mod recovery;
#[cfg(test)]
mod routing;
#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
mod seed;
#[cfg(test)]
mod start;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
mod world;

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
