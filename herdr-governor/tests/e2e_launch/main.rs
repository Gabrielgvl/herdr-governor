//! `e2e_launch` — the P5.B1 §4.4 dispatch pipeline end to end: a real
//! `daemon::run` in-process against `FakeHerdr`, effects seeded `planned`
//! through a second `Store` connection on the same `governor.db`. Where a
//! test needs the startup hand-out (the one `hand_out` pass that runs
//! before the first tick), the seed lands before `daemon::run` spawns and
//! `reconcile_secs` sits at 3600 so no observation interposes; interactive
//! cases keep the 1s tick and seed post-bind. Every wait is a bounded
//! poll; nothing sleeps on the wall clock.

#[cfg(test)]
mod dispatch;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
