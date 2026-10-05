//! `jev` — the Jev judgment client (spec §10, the P4.J1 contract): typed
//! questions in, calibrated probabilities and typed error classes out, no
//! retries. `wire` — the `POST /v1/systemone` body as the fixtures pin it
//! and the bounded decode; `client` — the redacted credential, the
//! no-socket-write size gate and the async call; `error` — the failure
//! taxonomy and its one map onto governor-core's `JudgmentOutcome`.
//!
//! Jev never sees operating points (F12): the request `State` type has
//! no field for them, so the constraint holds by construction. The F23/
//! F24 evidence-bearing states (review, blocked, acceptance) likewise
//! carry only the fields the spec names — the Task digest, the bounded
//! transcript tail or its `agent.read` fallback, the pinned-base git
//! evidence, the frozen handoff, and the blocked ask's typed
//! provider-limit record.

pub mod client;
pub mod error;
pub mod wire;

pub use client::{ApiKey, Client, JudgeParams, Judged, QuestionSpec};
pub use error::JevError;
pub use wire::{
    AcceptanceState, BlockedState, GitState, JEV_RESPONSE_MAX_BYTES, Kind, LimitRecordState,
    ReviewState, State, TaskDigest, TaskState, TranscriptLine,
};

#[cfg(test)]
mod tests;
