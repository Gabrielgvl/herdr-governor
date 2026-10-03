//! `herdr-governor` — the daemon crate (spec §9). `main.rs` holds the
//! hand-rolled subcommand entry; this library target carries the modules the
//! subcommands and the member-level tests drive.

pub mod adapters;
pub mod daemon;
pub mod store;
