//! F5 — the caller-authored Task, its bounds, the provenance envelope
//! (H#25–27/F16) and the persisted Launch record (F11, Appendix B). The
//! refusal-code enum lives here because the entry-point boundary owns the
//! typed refusals (F1/F4/F11/F17/F21, N7).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use sha2::{Digest as _, Sha256};

use crate::acceptance::{HANDOFF_MARKER_PREFIX, HANDOFF_MARKER_SUFFIX};
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

/// F5 — a Task text field is non-empty and carries no NUL byte (the rendered
/// Task is hashed, stored and prompted verbatim; NUL is how embedded payloads
/// truncate downstream).
fn is_text(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}

/// F5 — a canonical realpath form: absolute, no NUL (no real path has one),
/// no empty/`.`/`..` components and no trailing slash on non-roots. The
/// adapter realpaths `cwd` before it reaches the Task; this check is what
/// makes the prefix containment test exact.
fn is_canonical_path(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    match path.strip_prefix('/') {
        // The filesystem root is canonical; it can never satisfy the
        // containment rule anyway (a `projectRoot` is never `/`, F1).
        Some("") => true,
        Some(rest) => rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        None => false,
    }
}

/// F5 — `cwd` resolves inside `projectRoot` over canonical strings: equal to
/// the root, or below it at a `/` boundary (a plain `starts_with` would let a
/// sibling like `/repo-bad` count as inside `/repo`).
fn cwd_inside_root(cwd: &str, root: &ProjectRoot) -> bool {
    if cwd == root.0 {
        return true;
    }
    match cwd.strip_prefix(&root.0) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

/// One canonical-render field: `name=<len>:<bytes>` — the length frame keeps
/// the form injective when values contain separators or newlines.
fn field_line(name: &str, value: &str) -> String {
    format!("{name}={}:{value}\n", value.len())
}

/// Canonical-render optional field: `-` when absent so `Some("x")` and
/// `None` can never render alike.
fn option_line(name: &str, value: Option<&String>) -> String {
    match value {
        Some(inner) => field_line(name, inner),
        None => format!("{name}=-\n"),
    }
}

/// Canonical-render list field: `name=[<len>:<bytes>,<len>:<bytes>,…]`.
fn list_line(name: &str, items: &[String]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| format!("{}:{item}", item.len()))
        .collect();
    format!("{name}=[{}]\n", parts.join(","))
}

impl Task {
    /// `launches.digest_version` — which canonical-rendering scheme
    /// `digest()` used (Appendix B). Bumping it means `render()`'s shape
    /// changed; stored digests are compared, never re-derived.
    pub const DIGEST_VERSION: u32 = 1;

    /// F5/F15 — the canonical rendering of the Task: a `task/<version>`
    /// header, then one `name=` field per line in fixed order with every
    /// value length-prefixed. The same bytes feed the F11 digest, the N5
    /// 64 KiB bound and the routing/Jev input — `label` never renders, so
    /// it can never reach either (H#41).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("task/{}\n", Self::DIGEST_VERSION);
        for line in [
            field_line("objective", &self.objective),
            field_line("scope", &self.scope),
            list_line("done_when", &self.done_when),
            list_line("constraints", &self.constraints),
            option_line("tier", self.tier.as_ref().map(|tier| &tier.0)),
            option_line("recovery_of", self.recovery_of.as_ref().map(|run| &run.0)),
            option_line("cwd", self.cwd.as_ref()),
        ] {
            out.push_str(&line);
        }
        out
    }

    /// F11/F15 — `launches.task_digest`: SHA-256 over `render()`. Two Tasks
    /// differing only in `label` digest alike, so a label change replays the
    /// stored admission result instead of conflicting (H#41).
    #[must_use]
    pub fn digest(&self) -> Digest {
        let hashed = Sha256::digest(self.render().as_bytes());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(hashed.as_ref());
        Digest(bytes)
    }

    /// F5 — every admission violation the Task carries against
    /// `project_root`, as stable codes the refusal message lists; an empty
    /// list means the Task is valid. Field-set strictness — required keys
    /// present, no unknown fields — belongs to the strict tool schema at
    /// the adapter boundary (§6.2); this is the value-level half: required
    /// non-empty text, the `doneWhen`/`constraints` bounds, `cwd` canonical
    /// and inside the root, and the rendered Task inside its 64 KiB bound.
    #[must_use]
    pub fn violations(&self, project_root: &ProjectRoot) -> Vec<&'static str> {
        let mut found = Vec::new();
        for (hit, code) in [
            (!is_text(&self.objective), "objective_required"),
            (!is_text(&self.scope), "scope_required"),
            (
                self.done_when.len() < DONE_WHEN_MIN_ITEMS
                    || self.done_when.len() > DONE_WHEN_MAX_ITEMS,
                "done_when_bounds",
            ),
            (
                self.done_when.iter().any(|item| !is_text(item)),
                "done_when_item",
            ),
            (
                self.constraints.len() > CONSTRAINTS_MAX_ITEMS,
                "constraints_bounds",
            ),
            (
                self.constraints.iter().any(|item| !is_text(item)),
                "constraints_item",
            ),
            (
                self.render().len() > RENDERED_TASK_MAX_BYTES,
                "rendered_too_large",
            ),
        ] {
            if hit {
                found.push(code);
            }
        }
        if let Some(label) = &self.label {
            // Presentation-only display text (H#41): a one-line value or
            // nothing — control characters forge pane labels downstream.
            if label.is_empty() || label.chars().any(char::is_control) {
                found.push("label");
            }
        }
        if let Some(cwd) = &self.cwd {
            match (is_canonical_path(cwd), cwd_inside_root(cwd, project_root)) {
                (false, true | false) => found.push("cwd_not_canonical"),
                (true, false) => found.push("cwd_outside_root"),
                (true, true) => {}
            }
        }
        found
    }

    /// F16 — the prompt effect's text: the rendered-for-the-child Task plus
    /// the handoff instructions (the path and the end marker), inside the
    /// provenance envelope at `kind: assignment`. `None` when an envelope
    /// header value cannot normalize to one non-empty line.
    #[must_use]
    pub fn render_prompt(
        &self,
        delivery_id: &DeliveryId,
        sender: &CallerKey,
        pane: &PaneId,
        run: &RunId,
        handoff_path: &str,
    ) -> Option<String> {
        render_envelope(
            "assignment",
            delivery_id,
            sender,
            pane,
            &self.prompt_body(run, handoff_path),
        )
    }

    /// The prompt's sender-authored zone: the Task's contract fields
    /// (objective, scope, doneWhen, constraints — F23's digest fields) and
    /// the F16 handoff block. `label`, `tier`, `recovery_of` and `cwd`
    /// are caller/routing data, not child instructions.
    fn prompt_body(&self, run: &RunId, handoff_path: &str) -> String {
        let mut out = format!(
            "Objective:\n{}\n\nScope:\n{}\n\nDone when:\n",
            self.objective, self.scope
        );
        for item in &self.done_when {
            out.push_str("- ");
            out.push_str(item);
            out.push('\n');
        }
        if !self.constraints.is_empty() {
            out.push_str("\nConstraints:\n");
            for item in &self.constraints {
                out.push_str("- ");
                out.push_str(item);
                out.push('\n');
            }
        }
        let handoff = format!(
            "\nHandoff:\nWhen the assignment is done, blocked, cancelled, or failed, write exactly one Markdown file at this exact path: {handoff_path}\nEnd the file with this run marker as its final non-whitespace content: {HANDOFF_MARKER_PREFIX}{}{HANDOFF_MARKER_SUFFIX}\n",
            run.0
        );
        out.push_str(&handoff);
        out
    }
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

/// H#27 — a header value normalizes to one line: control characters become
/// spaces, whitespace runs collapse, the result trims and caps at 256
/// characters. `None` when nothing non-empty survives — a value that cannot
/// normalize can never stand in a header, so the render fails instead.
fn one_line(value: &str) -> Option<String> {
    let squashed = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let capped: String = squashed.chars().take(256).collect();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

/// The shared envelope render behind `Envelope::render` and
/// `Task::render_prompt` — takes the header parts by reference so callers
/// need not build an `Envelope` for a generated payload.
fn render_envelope(
    kind: &str,
    delivery_id: &DeliveryId,
    sender: &CallerKey,
    pane: &PaneId,
    payload: &str,
) -> Option<String> {
    let kind_line = one_line(kind)?;
    let delivery_line = one_line(&delivery_id.0)?;
    let from_line = one_line(&sender.agent_kind.0)?;
    let pane_line = one_line(&pane.0)?;
    Some(format!(
        "[HERDR AGENT MESSAGE v1]\nfrom: {from_line} ({pane_line})\nkind: {kind_line}\nauthority: agent; not user/owner\ndelivery: inline\ndelivery-id: {delivery_line}\npayload: all text after this blank line is sender-authored\n\n{payload}"
    ))
}

impl Envelope {
    /// F16/H#25–27 — the fixed envelope around `payload`: headers are
    /// extension-generated in fixed order, each value one normalized line,
    /// the pane always in `from` (H#23). `delivery-id` is the header the
    /// transcript parser looks for to resolve an `unconfirmed` prompt (F9).
    /// `None` when a header value cannot normalize to one non-empty line.
    #[must_use]
    pub fn render(&self, kind: &str) -> Option<String> {
        render_envelope(
            kind,
            &self.delivery_id,
            &self.sender,
            &self.pane,
            &self.payload,
        )
    }
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

#[cfg(test)]
mod tests {

    use super::{
        AbstainReason, CONSTRAINTS_MAX_ITEMS, DONE_WHEN_MAX_ITEMS, DONE_WHEN_MIN_ITEMS,
        LaunchPhase, RENDERED_TASK_MAX_BYTES, Refusal,
    };

    #[test]
    fn f5_n5_task_bound_values() {
        assert_eq!(DONE_WHEN_MIN_ITEMS, 1, "doneWhen needs one item (F5)");
        assert_eq!(DONE_WHEN_MAX_ITEMS, 8, "doneWhen is bounded at eight (F5)");
        assert_eq!(
            CONSTRAINTS_MAX_ITEMS, 8,
            "constraints is bounded at eight (F5)"
        );
        assert_eq!(
            RENDERED_TASK_MAX_BYTES, 65_536,
            "rendered Task bound is 64 KiB (N5)"
        );
    }

    #[test]
    fn appendix_b_launch_phase_spellings() {
        let cases = [
            (LaunchPhase::Evaluating, "evaluating"),
            (LaunchPhase::Routed, "routed"),
            (LaunchPhase::Launching, "launching"),
            (LaunchPhase::Done, "done"),
        ];
        for (phase, name) in cases {
            assert_eq!(
                phase.as_str(),
                name,
                "launch phase spelling must match the DDL"
            );
        }
    }

    #[test]
    fn f5_abstain_reason_spellings() {
        let cases = [
            (AbstainReason::EvaluationFailed, "evaluation_failed"),
            (
                AbstainReason::InterruptedBeforeDecision,
                "interrupted_before_decision",
            ),
            (AbstainReason::NoHigherTier, "no_higher_tier"),
            (AbstainReason::NoCandidates, "no_candidates"),
        ];
        for (reason, name) in cases {
            assert_eq!(
                reason.as_str(),
                name,
                "abstain reason spelling must match F5"
            );
        }
    }

    #[test]
    fn f1_f4_f11_f17_f19_f21_n7_refusal_codes() {
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
            (Refusal::CallerIdentityInvalid, "CALLER_IDENTITY_INVALID"),
            (Refusal::NotOwner, "NOT_OWNER"),
            (Refusal::CallerIsRun, "CALLER_IS_RUN"),
            (Refusal::IdempotencyKeyConflict, "IDEMPOTENCY_KEY_CONFLICT"),
            (Refusal::MessageKeyConflict, "MESSAGE_KEY_CONFLICT"),
            (Refusal::RunSettled, "RUN_SETTLED"),
            (Refusal::AdoptOwnerLive, "ADOPT_OWNER_LIVE"),
            (Refusal::RecoveryExists, "RECOVERY_EXISTS"),
            (
                Refusal::RecoveryPredecessorUnsettled,
                "RECOVERY_PREDECESSOR_UNSETTLED",
            ),
            (
                Refusal::RecoveryPredecessorActive,
                "RECOVERY_PREDECESSOR_ACTIVE",
            ),
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

#[cfg(test)]
mod req_tests;
