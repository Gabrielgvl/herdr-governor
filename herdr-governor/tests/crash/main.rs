//! P4.S4 — the store crash suite. A child `store_probe` applies a canned
//! transaction and `abort()`s at every statement boundary of the S3
//! crash-checkpoint seam; each test reopens the database and asserts the
//! transaction either fully committed or left no trace. `boundaries.rs`
//! holds the named scenario × boundary cases, `matrix.rs` pins every
//! scenario's measured boundary count to the declared table.

#[cfg(test)]
mod boundaries;
#[cfg(test)]
mod matrix;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
