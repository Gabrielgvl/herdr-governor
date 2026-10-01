//! N8 — the metamorphic proof (spec §7): renaming opaque harness and
//! operating-point ids while keeping declared capabilities changes no
//! non-transcript behaviour. One generated `World` drives both the F13
//! routing decision and the Appendix C lifecycle transition; a random
//! bijective renaming of the catalog's harness kinds and operating-point
//! ids produces the renamed world; every output must be identical modulo
//! the renaming. Transcript behaviour is outside the pure core by
//! construction (ADR-0002), so any difference at all is a
//! literal-dependence bug, not an intended distinction.
//!
//! The generators live in `strategies/`; the `Rename` application
//! methods live in `rename.rs` — the mapping is the relation,
//! not a generator.

mod rename;
pub mod strategies;
#[cfg(test)]
mod tests;
