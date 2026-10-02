//! `herdr` — the async Herdr client over `tokio::net::UnixStream` for the
//! confirmed protocol-22 subset (spec §10, the P4.H1 contract): one unary
//! request per connection; a bounded (1 MiB, spec N5) NDJSON codec;
//! `ping`, `session.snapshot`, `pane.{get,read,split,close}`,
//! `tab.create`, `agent.{start,prompt,get,list,read}` and the
//! `events.subscribe` armed stream — each typed, each deadline-bounded by
//! the caller.
//!
//! `types` — the wire DTOs and records; `codec` — framing, the envelope
//! decoder and the typed error vocabulary; `conn` — transport, `Client`,
//! `ConnEpoch` and the armed-stream machinery; `ops` — the typed
//! operations and their params/results. Protocol 22 proves no
//! incarnation (A4 confirmed-negative): `ConnEpoch` is a discontinuity
//! marker the daemon mints `HerdrIncarnation` from, nothing more (OQ-8).

pub mod codec;
pub mod conn;
pub mod ops;
pub mod probe;
pub mod types;

pub use codec::{HerdrError, MAX_FRAME_BYTES};
pub use conn::{Client, ConnEpoch, Subscription};
pub use ops::{
    AgentPromptParams, AgentPrompted, AgentStartParams, AgentStarted, PaneSplitParams, Pong,
    ReadOpts, TabCreateParams, TabCreated,
};
pub use types::{
    AgentInfo, AgentRow, AgentSession, AgentStatus, Observed, OutputMatch, PaneInfo, PaneRead,
    PaneScrollInfo, ReadFormat, ReadSource, SessionKind, SessionSnapshot, SubEvent,
    SubscriptionSpec, TabInfo, WorkspaceInfo,
};

#[cfg(test)]
mod tests;
