//! `jev` — the Jev judgment client (spec §10, the P4.J1 contract): typed
//! questions in, calibrated probabilities and typed error classes out, no
//! retries. `wire` — the `POST /v1/systemone` body as the fixtures pin it
//! and the bounded decode; `client` — the redacted credential, the
//! no-socket-write size gate and the async call; `error` — the failure
//! taxonomy and its one map onto governor-core's `JudgmentOutcome`.
//!
//! Jev never sees operating points (F12): the request `State` type has
//! no field for them, so the constraint holds by construction.

pub mod client;
pub mod error;
pub mod wire;

pub use client::{ApiKey, Client, JudgeParams, Judged, QuestionSpec};
pub use error::JevError;
pub use wire::{JEV_RESPONSE_MAX_BYTES, Kind, State, TaskState};

#[cfg(test)]
mod tests;
