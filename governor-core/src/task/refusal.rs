//! The spec's named typed refusals (F1/F4/F11/F17/F19/F21, N7) — the
//! task-facing tools and admission refuse with these codes, which is why
//! the enum lives at the entry-point boundary.

/// The spec's named typed refusals — `PascalCase` variants, SCREAMING_SNAKE
/// wire spellings (`code`). The task-facing tools and admission refuse with
/// these codes; `code()` is the only behaviour the enum carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refusal {
    /// F1 — a `session.snapshot` read finds no pane for the envelope, the pane
    /// is empty, or its occupant reports no native session.
    CallerIdentityMissing,
    /// F1 — the read resolves the envelope's pane id to more than one pane.
    CallerIdentityDuplicate,
    /// F1 — the pane is occupied but the occupant lacks a native session —
    /// treated the same as a missing caller (a shell driving a relay is not a
    /// caller).
    CallerIdentitySessionless,
    /// F1 — a bound `relayInstanceId` re-resolves to a different caller
    /// (`CALLER_IDENTITY_MISMATCH`, Appendix B/F1).
    CallerIdentityMismatch,
    /// F1 — the caller envelope is malformed, or its `projectRoot` is set but
    /// invalid (never re-anchored — H#3).
    CallerIdentityInvalid,
    /// F4 — the requester is not the Run's owner.
    NotOwner,
    /// F4 — a Run acts as its own caller (H#24).
    CallerIsRun,
    /// F11 — the idempotency key exists with a different body digest.
    IdempotencyKeyConflict,
    /// F17 — `messageKey` is already used for this Run.
    MessageKeyConflict,
    /// F17 — a follow-up addressed to a settled Run.
    RunSettled,
    /// F19 — `adopt` while the previous owner's native session is still
    /// present.
    AdoptOwnerLive,
    /// F21 — a recovery obligation already exists for the predecessor.
    RecoveryExists,
    /// F21 — `recoveryOf` names a predecessor that has not settled.
    RecoveryPredecessorUnsettled,
    /// F21 — `recoveryOf` while the predecessor's observation gate is unmet;
    /// retryable once the gate is met.
    RecoveryPredecessorActive,
    /// N7 — the relay could not reach the daemon at all.
    DaemonUnavailable,
}

impl Refusal {
    /// The spec spelling of the refusal code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::CallerIdentityMissing => "CALLER_IDENTITY_MISSING",
            Self::CallerIdentityDuplicate => "CALLER_IDENTITY_DUPLICATE",
            Self::CallerIdentitySessionless => "CALLER_IDENTITY_SESSIONLESS",
            Self::CallerIdentityMismatch => "CALLER_IDENTITY_MISMATCH",
            Self::CallerIdentityInvalid => "CALLER_IDENTITY_INVALID",
            Self::NotOwner => "NOT_OWNER",
            Self::CallerIsRun => "CALLER_IS_RUN",
            Self::IdempotencyKeyConflict => "IDEMPOTENCY_KEY_CONFLICT",
            Self::MessageKeyConflict => "MESSAGE_KEY_CONFLICT",
            Self::RunSettled => "RUN_SETTLED",
            Self::AdoptOwnerLive => "ADOPT_OWNER_LIVE",
            Self::RecoveryExists => "RECOVERY_EXISTS",
            Self::RecoveryPredecessorUnsettled => "RECOVERY_PREDECESSOR_UNSETTLED",
            Self::RecoveryPredecessorActive => "RECOVERY_PREDECESSOR_ACTIVE",
            Self::DaemonUnavailable => "DAEMON_UNAVAILABLE",
        }
    }
}
