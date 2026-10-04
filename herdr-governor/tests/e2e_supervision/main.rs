//! `e2e_supervision` — the supervision e2es: P5.B3 `reconcile` (F3/F22/N2/
//! F28/F8 + the status-event path), P5.C2 `delivery` (F17/F18 + S12/S25)
//! and P5.C3 `evidence`/`acceptance`/`idle` (F23–F25, S17–S20, S33/S34)
//! — a real `daemon::run` in-process against `FakeHerdr`, with runs seeded
//! through a second `Store` connection. `delivery`'s restart leg spawns the
//! shipped binary as a child for the seam kill. Socket-transport-level
//! variants land after P5.M2.

#[cfg(test)]
mod delivery;
#[cfg(test)]
mod evidence;
#[cfg(test)]
mod reconcile;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
