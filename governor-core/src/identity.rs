//! F1–F4 and F19 — caller identity, child identity, observation classes,
//! ownership and adoption, plus the opaque id/digest/time primitives the
//! other modules share. Every identifier the spec or Appendix B names is a
//! newtype here so call sites cannot swap one kind of id for another.

use alloc::string::String;
use core::time::Duration;

mod caller;
mod child;
mod observation;
mod ownership;

pub use caller::{
    CallerBinding, CallerEnvelope, CallerKey, resolve_caller, resolve_caller_key,
    validate_caller_envelope,
};
pub use child::{ChildIdentity, mint_agent_name};
pub use observation::{ChildStatus, Observation, ObservationClass, classify};
pub use ownership::{plan_adoption, plan_handover, require_owner};

/// A sha-256 content digest (`task_digest`, `body_digest`, handoff `digest`,
/// `payload_digest`); the hasher lives behind the digest-admission lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

/// §9 — an absolute time passed in as a value: milliseconds since the unix
/// epoch. The store renders Appendix B's `TEXT` form; the core never reads a
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The absolute deadline `window` after `self` (F22 — deadlines are
    /// stored absolute and never reset). The saturation is a backstop
    /// only: `Config::validate` bounds every policy window to
    /// [`MAX_POLICY_WINDOW`](crate::config::MAX_POLICY_WINDOW), so for a
    /// validated policy and a clock before year 9989 the sum never
    /// saturates and always encodes. An unvalidated `Policy` — a test or
    /// probe builds the struct directly — can still reach it: a
    /// pathological window pins to `i64::MAX` rather than wrapping into
    /// the past.
    #[must_use]
    pub(crate) fn after(self, window: Duration) -> Self {
        let millis = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
        Self(self.0.saturating_add(millis))
    }
}

/// Appendix B `runs.run_id` — one Run of a Launch's Task (uuid v7 text).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunId(pub String);

/// Appendix B `launches.launch_id` — one caller request to start a Task.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LaunchId(pub String);

/// Appendix B `mailbox.event_id` — one actionable event for a caller.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventId(pub String);

/// Appendix B `effects.effect_id` — one journaled mutation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EffectId(pub String);

/// Appendix B `effects.effect_key` — the unique dispatch key, e.g.
/// `run:<id>:prompt:task`, `run:<id>:outbox:<seq>`, `event:<id>:hint` (N1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EffectKey(pub String);

/// Appendix B `judgment_sets.set_id` — one bounded Jev request/response set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JudgmentSetId(pub String);

/// Herdr `pane_id` — a pane locator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(pub String);

/// Herdr `tab_id` — a tab locator (F14 placement, F12 `related_tab`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(pub String);

/// Herdr `terminal_id` — the stable terminal under a pane (F2, A4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TerminalId(pub String);

/// The Herdr server incarnation marker a Run's identity is bound to (A4, F28).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HerdrIncarnation(pub String);

/// The harness's own session identity — Herdr's `agent_session.value` (F1, F2,
/// F28 re-proof).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NativeSession(pub String);

/// A harness kind, carried as opaque catalog data — harness names never appear
/// as literals in this crate (N8, ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentKind(pub String);

/// A Run's minted agent name: `gov-<runId[0..8]>` (F2, H#52).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentName(pub String);

/// F1/ADR-0004 — the immutable random 128-bit id a relay mints once per
/// process; lowercase hex on the wire (Appendix B `relay_bindings`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelayInstanceId(pub String);

/// F1 — the caller's project root: absolute, single-line, not `/`, existing,
/// realpath-canonical. A relay-derived root that violates this is refused.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectRoot(pub String);

/// F11 — the caller-supplied idempotency key, unique within
/// `(caller key, projectRoot)`; keys are retained forever.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IdempotencyKey(pub String);

/// F17 — a follow-up key, unique within its Run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageKey(pub String);

/// F18 — a mailbox event's stable deduplication key, e.g. `run:<id>:settled`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DedupKey(pub String);

/// F9/F16 — the delivery id the provenance envelope carries so transcript
/// evidence can resolve an `unconfirmed` prompt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeliveryId(pub String);

/// A snapshot's agent rows — the per-pane occupant view the adapter builds
/// from one fresh `session.snapshot` (`agents` joined to `panes`): locator,
/// terminal, harness kind, agent name, native session and reported status.
/// Every field is a distinct newtype, so positions cannot be transposed.
/// A pane with no agent has no row: it resolves no caller and matches no
/// child identity. The `Option` fields are `None` when Herdr reports no
/// value (a `null` field, or an `agent_status` of `unknown` for status).
type AgentRow = (
    PaneId,
    TerminalId,
    Option<AgentKind>,
    Option<AgentName>,
    Option<NativeSession>,
    Option<ChildStatus>,
);

#[cfg(test)]
mod tests;
