//! `e2e_supervision` — the P5.B3 reconcile e2e (F3/F22/N2/F28/F8 + the
//! status-event path): a real `daemon::run` in-process against
//! `FakeHerdr`, with runs seeded through a second `Store` connection.
//! Socket-transport-level variants land after P5.M2.

#[cfg(test)]
mod reconcile;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
