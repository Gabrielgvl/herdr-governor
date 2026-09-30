//! F5 — the caller-authored Task, its bounds, the provenance envelope
//! (H#25–27/F16) and the persisted Launch record (F11, Appendix B). The
//! refusal-code enum lives here because the entry-point boundary owns the
//! typed refusals (F1/F4/F11/F17/F21, N7).

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{ConfigVersion, OperatingPointId, Tier};
use crate::identity::{
    CallerKey, DeliveryId, Digest, IdempotencyKey, LaunchId, PaneId, ProjectRoot, RunId,
};
use crate::lifecycle::{CreatedTopology, EffectCertainty};
use crate::routing::Decision;

/// F5 — a `doneWhen` list must carry at least one verifiable item.
pub const DONE_WHEN_MIN_ITEMS: usize = 1;

/// F5 — a `doneWhen` list is bounded at eight items.
pub const DONE_WHEN_MAX_ITEMS: usize = 8;

/// F5 — `constraints` is optional and bounded at eight items.
pub const CONSTRAINTS_MAX_ITEMS: usize = 8;

/// F5/N5 — the rendered Task is bounded at 64 KiB.
pub const RENDERED_TASK_MAX_BYTES: usize = 64 * 1024;

/// F5 — the caller-authored work unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// A bounded non-empty string: the work to do.
    pub objective: String,
    /// Where the work is allowed to happen.
    pub scope: String,
    /// 1–8 verifiable `doneWhen` items (`DONE_WHEN_{MIN,MAX}_ITEMS`).
    pub done_when: Vec<String>,
    /// 0–8 `constraints` items (`CONSTRAINTS_MAX_ITEMS`); the spec's default
    /// is `[]`, never a missing key.
    pub constraints: Vec<String>,
    /// The caller's uplift input to routing (F13 step 3); Jev judges the Task,
    /// not the tier.
    pub tier: Option<Tier>,
    /// F21 — the settled predecessor this Launch continues, when the caller
    /// requests a recovery.
    pub recovery_of: Option<RunId>,
    /// A display label; presentation only, never identity (H#41).
    pub label: Option<String>,
    /// Where the child starts; canonicalized to a real path inside the
    /// caller's `projectRoot`, else the Launch fails (F5). Absent means the
    /// project root.
    pub cwd: Option<String>,
}

/// Appendix B `launches.phase` — where an admitted Launch stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaunchPhase {
    /// Under evaluation — the Jev answer is still outstanding.
    Evaluating,
    /// Judged and a decision persisted; a live Run exists.
    Routed,
    /// Topology or start effects are in flight; a Run may exist.
    Launching,
    /// The Launch answered; `outcome` is terminal for it.
    Done,
}

impl LaunchPhase {
    /// Appendix B — the stored spelling of the phase.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Evaluating => "evaluating",
            Self::Routed => "routed",
            Self::Launching => "launching",
            Self::Done => "done",
        }
    }
}

/// F11/Appendix B — the persisted Launch row: the immutable caller binding,
/// the canonical Task, the routing decision, and the outcome once `done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// `launch_id`.
    pub id: LaunchId,
    /// The resolved caller at admission; immutable.
    pub caller: CallerKey,
    /// The caller's canonical project root; part of the idempotency scope.
    pub project_root: ProjectRoot,
    /// The caller-scoped idempotency key (F11).
    pub idempotency_key: IdempotencyKey,
    /// `digest_version` — which digest scheme `task_digest` used (Appendix B).
    pub digest_version: u32,
    /// Digest of the canonical Task (F15 — equality hashes the rendered Task).
    pub task_digest: Digest,
    /// The canonical Task itself.
    pub task: Task,
    /// Current phase.
    pub phase: LaunchPhase,
    /// The persisted routing decision once made (F13).
    pub decision: Option<Decision>,
    /// The config version the decision was made under (F27); absent while
    /// `evaluating`.
    pub config_version: Option<ConfigVersion>,
    /// The Launch's outcome — present iff `phase` is `done` (Appendix B CHECK).
    pub outcome: Option<LaunchOutcome>,
}

/// F5 — why a Launch abstained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AbstainReason {
    /// `evaluation_failed` — Jev evaluation was unavailable on the first pass
    /// (H#64: the initial evaluation does not cross-provider fail over).
    EvaluationFailed,
    /// `interrupted_before_decision` — restarted between recording the
    /// evaluation and committing the decision (F28).
    InterruptedBeforeDecision,
    /// `no_higher_tier` — a recovery obligation could not find a higher tier
    /// (F21).
    NoHigherTier,
    /// `no_candidates` — no operating point satisfied the requirements.
    NoCandidates,
}

impl AbstainReason {
    /// F5 — the spec spelling of the abstention reason.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EvaluationFailed => "evaluation_failed",
            Self::InterruptedBeforeDecision => "interrupted_before_decision",
            Self::NoHigherTier => "no_higher_tier",
            Self::NoCandidates => "no_candidates",
        }
    }
}

/// F5 — a completed Launch's outcome (`launches.outcome`).
#[expect(
    clippy::large_enum_variant,
    reason = "launched carries the full persisted Decision as tier evidence — boxing would distort the shared vocabulary"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// `launched` — the Task is running.
    Launched {
        /// The launched Run.
        run: RunId,
        /// The operating point actually used (may differ from the requested
        /// one after fallback — F5 `requestedOperatingPointId`).
        operating_point: OperatingPointId,
        /// What the caller requested, when it differs or was named.
        requested_operating_point: Option<OperatingPointId>,
        /// `tier` + `tier_evidence` — the persisted decision is the evidence.
        tier_evidence: Decision,
    },
    /// `abstained` — the governor declined; `reason` distinguishes
    /// "Jev never answered" from "no candidate exists" (H#10, F5).
    Abstained {
        /// Why the Launch abstained.
        reason: AbstainReason,
    },
    /// `rejected` — evaluation completed and the Task was rejected
    /// (`doneWhen` not verifiable — F5/F12).
    Rejected,
    /// `failed` — the governor broke after the Launch was admitted; the
    /// caller must not assume nothing ran unless `certainty` says so.
    Failed {
        /// The F20 certainty contract: `absent` (provably nothing ran) or
        /// `unknown`.
        certainty: EffectCertainty,
        /// The Run if one exists (e.g. `agent.start` timed out).
        run: Option<RunId>,
        /// Every new tab/pane created before the failure (F5
        /// `createdTopology`; empty when nothing was created).
        created_topology: CreatedTopology,
    },
}

/// F5 — the `herdr_launch` result: `pending` means the Launch was recorded
/// and continues through the daemon.
#[expect(
    clippy::large_enum_variant,
    reason = "Outcome carries LaunchOutcome unboxed — see the enum's expect"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchResponse {
    /// `pending` — recorded; the caller is notified on `launched` or
    /// `abstained`.
    Pending {
        /// The recorded Launch.
        launch: LaunchId,
        /// The Run once the runner thread has made one (rare at reply time).
        run: Option<RunId>,
    },
    /// `launched` / `abstained` / `rejected` / `failed` as one outcome.
    Outcome(LaunchOutcome),
}

/// F16 — the provenance envelope: every message sent to a running child,
/// however it arrives, is wrapped before send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// F9 delivery id — transcript evidence resolves an `unconfirmed` prompt.
    pub delivery_id: DeliveryId,
    /// The verified sender (H#23 — caller identity is relay-derived).
    pub sender: CallerKey,
    /// The sender's pane — always included in the body (H#23).
    pub pane: PaneId,
    /// The message body the envelope wraps.
    pub payload: String,
}

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
    /// F4 — the requester is not the Run's owner.
    NotOwner,
    /// F11 — the idempotency key exists with a different body digest.
    IdempotencyKeyConflict,
    /// F17 — `messageKey` is already used for this Run.
    MessageKeyConflict,
    /// F17 — a follow-up addressed to a settled Run.
    RunSettled,
    /// F21 — a recovery obligation already exists for the predecessor.
    RecoveryExists,
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
            Self::NotOwner => "NOT_OWNER",
            Self::IdempotencyKeyConflict => "IDEMPOTENCY_KEY_CONFLICT",
            Self::MessageKeyConflict => "MESSAGE_KEY_CONFLICT",
            Self::RunSettled => "RUN_SETTLED",
            Self::RecoveryExists => "RECOVERY_EXISTS",
            Self::DaemonUnavailable => "DAEMON_UNAVAILABLE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Refusal;

    #[test]
    fn refusal_codes_are_the_spec_spellings() {
        let cases = [
            (Refusal::CallerIdentityMissing, "CALLER_IDENTITY_MISSING"),
            (
                Refusal::CallerIdentityDuplicate,
                "CALLER_IDENTITY_DUPLICATE",
            ),
            (
                Refusal::CallerIdentitySessionless,
                "CALLER_IDENTITY_SESSIONLESS",
            ),
            (Refusal::CallerIdentityMismatch, "CALLER_IDENTITY_MISMATCH"),
            (Refusal::NotOwner, "NOT_OWNER"),
            (Refusal::IdempotencyKeyConflict, "IDEMPOTENCY_KEY_CONFLICT"),
            (Refusal::MessageKeyConflict, "MESSAGE_KEY_CONFLICT"),
            (Refusal::RunSettled, "RUN_SETTLED"),
            (Refusal::RecoveryExists, "RECOVERY_EXISTS"),
            (Refusal::DaemonUnavailable, "DAEMON_UNAVAILABLE"),
        ];
        for (refusal, code) in cases {
            assert_eq!(
                refusal.code(),
                code,
                "refusal code must match spec spelling"
            );
        }
    }
}
