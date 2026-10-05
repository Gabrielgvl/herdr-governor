//! `e2e_run` — the `herdr_run` surface end to end (P5.C4): `observe`'s
//! F6 page and opaque cursor (`observe`), `ack`'s idempotent mailbox
//! stamp (`ack`), `handover`/`adopt`'s F4/F19 ownership moves
//! (`ownership`), `cancel`'s F20 settlement plus its parked verified
//! close and the literal `herdr_run{cancel}` S31b (`cancel`), and the
//! §4.10 recovery sweep end to end — S21, S21b, S22's expiry and the
//! in-flight-successor expiry skip — with §17's settled-mid-start
//! orphan close (`recovery`). A real `daemon::run` in-process against
//! `FakeHerdr` + `FakeJev`; Runs, obligations and outbox rows are seeded
//! through a second `Store` connection. `world` is the fixture, `seed`
//! the rows/reads. `reconcile_secs = 1` keeps the tick on a one-second
//! cadence; every wait is a bounded poll, nothing sleeps on the wall
//! clock.

#[cfg(test)]
mod ack;
#[cfg(test)]
mod cancel;
#[cfg(test)]
mod observe;
#[cfg(test)]
mod ownership;
#[cfg(test)]
mod recovery;
#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
mod seed;
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

/// Every wait's bound — the 1s tick plus wire round-trips land an
/// effect or a sweep move in a few seconds; this is the backstop.
#[cfg(test)]
const DEADLINE: Duration = Duration::from_secs(20);
/// `reconcile_secs = 1` — the fastest cadence the catalog accepts.
#[cfg(test)]
const TICK_SECS: u64 = 1;
/// The caller envelope's `relayInstanceId` (32 lowercase hex; opaque).
/// `relay_bindings` is UNIQUE on it — every other dialled-in caller
/// carries its own.
#[cfg(test)]
const RELAY: &str = "abababababababababababababababab";
/// `w1:p3`'s relay — the handover successor's client.
#[cfg(test)]
const SUCC_RELAY: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
/// `w1:p4`'s relay — the foreign caller's seeded binding and client.
#[cfg(test)]
const FOREIGN_RELAY: &str = "efefefefefefefefefefefefefefefef";
/// `w1:p2`'s relay — the child's client (`CALLER_IS_RUN` lanes).
#[cfg(test)]
const CHILD_RELAY: &str = "12121212121212121212121212121212";
/// The caller's pane — `kind-a`/`caller-session` on `w1:p1`.
#[cfg(test)]
const CALLER_PANE: &str = "w1:p1";
/// The supervised child's pane — `gov-r1` on `w1:p2`.
#[cfg(test)]
const CHILD_PANE: &str = "w1:p2";
/// The handover successor's pane — `kind-b`/`sess-succ` on `w1:p3`.
#[cfg(test)]
const SUCC_PANE: &str = "w1:p3";
/// The foreign caller's pane — `kind-b`/`sess-foreign` on `w1:p4`.
#[cfg(test)]
const FOREIGN_PANE: &str = "w1:p4";
/// A timestamp inside the store's RFC3339 range, before `now`.
#[cfg(test)]
const NOW: Timestamp = Timestamp(1_790_812_800_000);
/// A deadline-bearing timestamp far past every window.
#[cfg(test)]
const FAR: Timestamp = Timestamp(4_102_444_800_000);
