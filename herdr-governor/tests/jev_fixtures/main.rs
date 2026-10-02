//! Jev contract-fixture tests moved out of `src/adapters/jev/tests*`:
//! a member's `src/` must compile against its own tree alone (the
//! guard-selftest mirrors each member's `src/` plus the lockfile), so
//! fixtures under the workspace `tests/fixtures/contract/` are read here
//! at run time. `support.rs` carries the fixture loader and the
//! fake-server harness duplicated from the src test module; `wire.rs`
//! holds the pure-wire tests and `client.rs` the fake-server tests.

#[cfg(test)]
mod client;
#[cfg(test)]
pub mod support;
#[cfg(test)]
mod wire;
