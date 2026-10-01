//! `store` — the SQLite store (spec §9). Its public API is
//! `apply(Transition)` plus read queries; lifecycle writes live only under
//! `transitions` (I10).
//!
//! `error` — the typed store errors; `migrate` — the Appendix-B DDL and the
//! expand-only `user_version` migration; `rows` — row ↔ core-type codecs;
//! `reads` — the typed read queries; `transitions` — the per-transaction
//! writers `apply` dispatches to.

mod error;
mod migrate;
mod reads;
mod rows;
mod transitions;
