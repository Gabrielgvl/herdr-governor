//! F11/Appendix B — the persisted Launch record, its phase and outcome
//! vocabulary (`launched`/`abstained`/`rejected`/`failed`), the
//! `herdr_launch` response shape, and the idempotency admission decision.

use crate::config::{ConfigVersion, OperatingPointId};
use crate::identity::{CallerKey, Digest, IdempotencyKey, LaunchId, ProjectRoot, RunId};
use crate::lifecycle::{CreatedTopology, EffectCertainty};
use crate::routing::Decision;

use super::{Refusal, Task};

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

/// F11 — the idempotency decision for `herdr_launch`, scoped to `(caller,
/// projectRoot, idempotencyKey)`.
///
/// `existing` is the row the store found under the key (with the Run it
/// already made, when one exists); `digest` is the incoming Task's
/// `digest()`. The result:
/// - `Ok(None)` — no Launch holds the key in this scope: record the new
///   Launch and continue admission. A row returned with a different caller,
///   root or key is outside the scope and counts as absent.
/// - `Ok(Some(_))` — the same digest already launched: the stored outcome
///   once `done`, `pending` while the Launch still stands.
/// - `Err(Refusal::IdempotencyKeyConflict)` — the key was used with a
///   different task digest.
///
/// Retention is the store's (keys are never garbage-collected — F11); this
/// function only decides.
pub fn admission_decision(
    caller: &CallerKey,
    project_root: &ProjectRoot,
    key: &IdempotencyKey,
    digest: &Digest,
    existing: Option<&Launch>,
    run: Option<RunId>,
) -> Result<Option<LaunchResponse>, Refusal> {
    let Some(launch) = existing else {
        return Ok(None);
    };
    if launch.caller != *caller
        || launch.project_root != *project_root
        || launch.idempotency_key != *key
    {
        return Ok(None);
    }
    if launch.task_digest != *digest {
        return Err(Refusal::IdempotencyKeyConflict);
    }
    let pending = LaunchResponse::Pending {
        launch: launch.id.clone(),
        run,
    };
    let response = match launch.phase {
        LaunchPhase::Done => match &launch.outcome {
            Some(outcome) => LaunchResponse::Outcome(outcome.clone()),
            // `done` without an outcome cannot exist through the store
            // (Appendix B CHECK) — read the impossible row as still pending
            // rather than invent an outcome.
            None => pending,
        },
        LaunchPhase::Evaluating | LaunchPhase::Routed | LaunchPhase::Launching => pending,
    };
    Ok(Some(response))
}
