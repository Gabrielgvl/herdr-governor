//! Spec §10 Phase 3 — property proofs over the pure lifecycle transition
//! function, driven only through governor-core's public API:
//!
//! - `f22_safety_over_event_prefixes` — at most one settlement, never two
//!   prompts per effect key, no transition out of a settlement;
//! - `f22_settles_past_every_deadline` — every armed, overdue deadline
//!   settles the Run with its Appendix C settlement;
//! - `f22_restart_never_extends_a_deadline` — a restart write never moves a
//!   deadline;
//! - `f20_stale_async_result_never_applies` — a version stamp that no longer
//!   holds produces nothing; a stale Jev receipt journals `stale` and
//!   applies nothing else.

pub mod strategies;

#[cfg(test)]
mod staleness;
#[cfg(test)]
pub mod tests;
