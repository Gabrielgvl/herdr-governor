//! F1–F3 — caller identity, child identity and observation classes, plus the
//! opaque id/digest/time primitives the other modules share. Every identifier
//! the spec or Appendix B names is a newtype here so call sites cannot swap
//! one kind of id for another.

use alloc::string::String;

/// A sha-256 content digest (`task_digest`, `body_digest`, handoff `digest`,
/// `payload_digest`); the hasher lives behind the digest-admission lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

/// §9 — an absolute time passed in as a value: milliseconds since the unix
/// epoch. The store renders Appendix B's `TEXT` form; the core never reads a
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub i64);

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

/// F1/ADR-0004 — the relay-attached envelope on every forwarded request:
/// `{paneId, projectRoot, relayInstanceId}` as relay-to-daemon framing, never
/// a tool argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerEnvelope {
    /// `paneId` — the relay's inherited `HERDR_PANE_ID`.
    pub pane_id: PaneId,
    /// `projectRoot` — realpath of the relay's git root (or cwd outside a
    /// worktree).
    pub project_root: ProjectRoot,
    /// `relayInstanceId` — minted per relay process; binds once, then every
    /// request must re-resolve to the same native session.
    pub relay_instance_id: RelayInstanceId,
}

/// F1 — the durable caller key `(agent kind, native session)`, resolved from a
/// fresh `session.snapshot`: exactly one pane with the envelope's id whose
/// occupant has a native session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallerKey {
    /// The occupying agent's harness kind.
    pub agent_kind: AgentKind,
    /// The occupying agent's native session.
    pub native_session: NativeSession,
}

/// F1/Appendix B — the persisted `relayInstanceId` → caller key binding; kept
/// across daemon restarts, re-verified by a fresh locator check per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerBinding {
    /// The resolved caller.
    pub caller: CallerKey,
    /// The bound relay instance.
    pub relay_instance: RelayInstanceId,
    /// `pane_id_at_bind` — the pane the caller occupied when bound.
    pub pane_at_bind: PaneId,
}

/// F2 — a child's captured identity parts: the first four are captured at the
/// acknowledged `agent.start` and suffice before the first prompt; the pane id
/// is only a current locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildIdentity {
    /// The Herdr incarnation the child was observed under.
    pub herdr_incarnation: HerdrIncarnation,
    /// The stable terminal identity.
    pub terminal_id: TerminalId,
    /// The child's harness kind (catalog data, never a literal).
    pub agent_kind: AgentKind,
    /// The minted `gov-<runId[0..8]>` name.
    pub agent_name: AgentName,
    /// Once Herdr reports it (F2); carries the session re-proof (F28).
    pub native_session: Option<NativeSession>,
    /// Current locator only — a move is followed, never a loss (H#75).
    pub pane_id: PaneId,
}

/// F3 — the observation classes: how a target-local read of a fresh snapshot
/// resolves a captured identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObservationClass {
    /// Exactly one pane matches the identity, wherever it is now.
    Unique,
    /// The snapshot is valid and no pane matches.
    Absent,
    /// The snapshot is malformed, duplicated, unavailable, or ambiguous about
    /// the incarnation — never counts as absence, never settles a Run (H#74).
    Invalid,
}

impl ObservationClass {
    /// F3 — the spec spelling of the class.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unique => "unique",
            Self::Absent => "absent",
            Self::Invalid => "invalid",
        }
    }
}

/// Appendix C — the child status an `obs` event carries; Herdr's reported
/// `agent_status` reduced to the four spec states (a wire `unknown` is
/// `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChildStatus {
    /// The child is working.
    Working,
    /// The child is idle — starts an idle episode in `active` (F25).
    Idle,
    /// The child finished its turn — treated like `idle` (Appendix C).
    Done,
    /// The child is blocked — never prompt it (F17); asks the F23 questions.
    Blocked,
}

impl ChildStatus {
    /// Appendix C — the spec spelling of the status.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

/// F3 — one target-local read of a fresh snapshot: the class plus what the
/// located pane reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// `obs(unique)` — the identity resolves to exactly one pane.
    Unique {
        /// The child's reported status; `None` when Herdr reports none or the
        /// wire's `unknown`.
        status: Option<ChildStatus>,
        /// Where the identity is now — a move is followed (H#75).
        pane: PaneId,
        /// The native session the pane's occupant reports now (F2).
        native_session: Option<NativeSession>,
    },
    /// `obs(absent)` — the snapshot is valid and no pane matches.
    Absent,
    /// `obs(invalid)` — changes nothing in any state, except health reporting;
    /// deadlines still run (Appendix C).
    Invalid,
}

impl Observation {
    /// F3 — the class of this observation.
    #[must_use]
    pub fn class(&self) -> ObservationClass {
        match self {
            Self::Unique {
                status: _,
                pane: _,
                native_session: _,
            } => ObservationClass::Unique,
            Self::Absent => ObservationClass::Absent,
            Self::Invalid => ObservationClass::Invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChildStatus, NativeSession, Observation, ObservationClass, PaneId};

    #[test]
    fn f3_observation_class_spellings() {
        let cases = [
            (ObservationClass::Unique, "unique"),
            (ObservationClass::Absent, "absent"),
            (ObservationClass::Invalid, "invalid"),
        ];
        for (class, name) in cases {
            assert_eq!(
                class.as_str(),
                name,
                "observation class spelling must match F3"
            );
        }
    }

    #[test]
    fn appendix_c_child_status_spellings() {
        let cases = [
            (ChildStatus::Working, "working"),
            (ChildStatus::Idle, "idle"),
            (ChildStatus::Done, "done"),
            (ChildStatus::Blocked, "blocked"),
        ];
        for (status, name) in cases {
            assert_eq!(
                status.as_str(),
                name,
                "child status spelling must match Appendix C"
            );
        }
    }

    #[test]
    fn f3_observation_maps_to_its_class() {
        let unique = Observation::Unique {
            status: Some(ChildStatus::Working),
            pane: PaneId("w6:p1".into()),
            native_session: Some(NativeSession("sess".into())),
        };
        assert_eq!(unique.class(), ObservationClass::Unique);
        assert_eq!(Observation::Absent.class(), ObservationClass::Absent);
        assert_eq!(Observation::Invalid.class(), ObservationClass::Invalid);
    }
}
