//! Appendix C — the lifecycle transition vocabulary: `State`, `Event`,
//! `Settlement`, the version triple and deadline kinds, the F8 effect-journal
//! types, and the `Transition` value the transition function returns
//! (§9/F22). This module declares the vocabulary only — the total function
//! lives in a module lane.

use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::{FrozenHandoff, HandoffReading};
use crate::config::{OperatingPointId, Provider, Tier};
use crate::delivery::{ExpiryReason, MailboxEvent, OutboxMessage};
use crate::identity::{
    CallerBinding, CallerKey, ChildIdentity, ChildStatus, Digest, EffectId, EffectKey, EventId,
    LaunchId, Observation, PaneId, RunId, TabId, Timestamp,
};
use crate::recovery::{Cooldown, RecoveryObligation};
use crate::routing::{JudgmentRecord, PlacementPlan};
use crate::task::Launch;

/// Appendix C — the lifecycle states: `reserved` → `starting` → `prompting` →
/// `active` → `judging` ⇄ `repair` → `settled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// `reserved` — decision persisted, Run row created, no effect yet.
    Reserved,
    /// `starting` — topology planned; `agent.start` in flight or falling
    /// back across candidates.
    Starting,
    /// `prompting` — started; the Task prompt effect is in flight.
    Prompting,
    /// `active` — the Run is supervised (F23/F25).
    Active,
    /// `judging` — a frozen handoff is being assessed (F24).
    Judging,
    /// `repair` — a rejected work generation; `repair_deadline` runs (F24).
    Repair,
    /// `settled` — terminal; immutable (F20).
    Settled,
}

impl State {
    /// Appendix B — the stored spelling of the state (`runs.state` CHECK).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Starting => "starting",
            Self::Prompting => "prompting",
            Self::Active => "active",
            Self::Judging => "judging",
            Self::Repair => "repair",
            Self::Settled => "settled",
        }
    }
}

/// Appendix B `runs.prompt_certainty` — what the prompt's acknowledgement
/// proved (F16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PromptCertainty {
    /// `acknowledged` — the ack matched the captured identity (H#30).
    Acknowledged,
    /// `unconfirmed` — possibly consumed: no resubmission, relaunch or
    /// cleanup; supervision continues (H#29, H#31).
    Unconfirmed,
}

impl PromptCertainty {
    /// Appendix B — the stored spelling of the certainty.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// Appendix C `deadline(...)` / F22 — the deadline kinds, stored as absolute
/// times and never reset by observations, restarts or paused reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineKind {
    /// `idle` — the F25 idle episode bound (`idle_deadline`).
    Idle,
    /// `repair` — the F24 repair window (`repair_deadline`).
    Repair,
    /// `judgment` — the Jev-unavailable bound (`judgment_deadline`).
    Judgment,
    /// `max_age` — the per-Run lifetime bound (`max_age_deadline`); in any
    /// unsettled state it settles `unresolved(max_age)`.
    MaxAge,
}

impl DeadlineKind {
    /// Appendix C — the spec spelling of the deadline.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Repair => "repair",
            Self::Judgment => "judgment",
            Self::MaxAge => "max_age",
        }
    }
}

/// F20 — the specific reasons a Run can settle `unresolved`
/// (`runs.settlement_reason`; Appendix B requires it for `unresolved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedReason {
    /// `launch_not_started` — the Launch abstained or failed before any
    /// effect (Appendix C `reserved`).
    LaunchNotStarted,
    /// `launch_failed` — start failed with no candidate, or went
    /// `unconfirmed`, and the pane was observed `absent` (Appendix C
    /// `starting`).
    LaunchFailed,
    /// `judgment_unavailable` — Jev stayed unavailable past
    /// `judgment_deadline` (F24).
    JudgmentUnavailable,
    /// `identity_unprovable` — a Herdr incarnation change left a Run without
    /// `native_session` unable to re-prove its identity (A4/F28).
    IdentityUnprovable,
    /// `max_age` — `max_age_deadline` passed (F22/F25).
    MaxAge,
}

impl UnresolvedReason {
    /// F20 — the stored spelling of the reason (`runs.settlement_reason`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LaunchNotStarted => "launch_not_started",
            Self::LaunchFailed => "launch_failed",
            Self::JudgmentUnavailable => "judgment_unavailable",
            Self::IdentityUnprovable => "identity_unprovable",
            Self::MaxAge => "max_age",
        }
    }
}

/// F20 — the settlements: first-commit-wins and immutable (`settlement IS
/// NULL AND version = :v` guards the write; a trigger aborts any rewrite).
/// The governor never invents an `accepted`/`rejected` verdict (F25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Settlement {
    /// `accepted` — every doneWhen item met (F24).
    Accepted,
    /// `rejected` — `repair_deadline` passed with no qualifying repair (F24).
    Rejected,
    /// `no_handoff` — `idle_deadline` passed on an idle episode (F25).
    NoHandoff,
    /// `pane_lost` — the child's identity went `absent` with no frozen
    /// handoff (F25).
    PaneLost,
    /// `cancelled` — `cancel` on an unsettled Run (F20).
    Cancelled,
    /// `provider_limited` — the provider-limit judgment cleared the policy
    /// threshold (F21).
    ProviderLimited,
    /// `unresolved(reason)` — the governor could not prove an outcome
    /// (F20/F25).
    Unresolved {
        /// The specific reason — never absent on `unresolved` (Appendix B
        /// CHECK).
        reason: UnresolvedReason,
    },
}

impl Settlement {
    /// Appendix B — the stored spelling of the settlement (`runs.settlement`
    /// CHECK); the reason rides `runs.settlement_reason`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::NoHandoff => "no_handoff",
            Self::PaneLost => "pane_lost",
            Self::Cancelled => "cancelled",
            Self::ProviderLimited => "provider_limited",
            Self::Unresolved { reason: _ } => "unresolved",
        }
    }
}

/// F20 — the `(version, work_generation, evidence_generation)` triple every
/// Jev result, observation and deadline carries; it applies only while all
/// three still hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VersionTriple {
    /// `runs.version` — the row version.
    pub version: u64,
    /// `runs.work_generation` — the generation of accepted work (repair
    /// dispatches advance it, F24).
    pub work_generation: u64,
    /// `runs.evidence_generation` — the generation of judged evidence
    /// (freezes advance it, F24).
    pub evidence_generation: u64,
}

/// F20 — an async result stamped with the versions it was requested against;
/// the transition checks the stamp before applying.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    /// The versions the result was requested against.
    pub requested_against: VersionTriple,
    /// The result payload.
    pub value: T,
}

/// Appendix C `judgment(...)` — how a completed acceptance assessment rules
/// on the frozen handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JudgmentVerdict {
    /// `accept` — every doneWhen item met → `accepted`.
    Accept,
    /// `reject` — some item unmet → `repair` (F24).
    Reject,
    /// `unavailable` — the request could not complete; the Run waits on
    /// `judgment_deadline` (F24).
    Unavailable,
}

/// Appendix B `effects.kind` — every journaled mutation: the Herdr operations
/// plus the launch-time Jev evaluation (F8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectKind {
    /// `jev_evaluate` — one `systemOne` request (F12).
    JevEvaluate,
    /// `tab_create` — open a tab (F14).
    TabCreate,
    /// `pane_split` — right split, without focus (F14/H#53).
    PaneSplit,
    /// `agent_start` — start the harness with the persisted arguments (F15).
    AgentStart,
    /// `prompt` — send text to a pane: Task prompts, follow-ups, nudges,
    /// hints (F9/F16–F18/F23).
    Prompt,
    /// `close` — close a pane; only ever on an explicit `cancel
    /// {closePane}` (F20 — panes are never closed automatically).
    Close,
}

impl EffectKind {
    /// Appendix B — the stored spelling of the kind (`effects.kind` CHECK).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JevEvaluate => "jev_evaluate",
            Self::TabCreate => "tab_create",
            Self::PaneSplit => "pane_split",
            Self::AgentStart => "agent_start",
            Self::Prompt => "prompt",
            Self::Close => "close",
        }
    }
}

/// F8/Appendix B `effects.state` — the journal lifecycle:
/// `planned` → `dispatching` → `acknowledged` | `failed`; a `dispatching`
/// effect without a receipt becomes `unconfirmed` on restart and is never
/// dispatched again; a `planned` one may still be dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectState {
    /// `planned` — journaled, not yet dispatched.
    Planned,
    /// `dispatching` — committed to the wire, awaiting receipt.
    Dispatching,
    /// `acknowledged` — the receipt is committed.
    Acknowledged,
    /// `failed` — it failed; `certainty` is required (Appendix B CHECK).
    Failed,
    /// `unconfirmed` — crash-interrupted dispatch; never retried (F8/N1).
    Unconfirmed,
}

impl EffectState {
    /// Appendix B — the stored spelling of the state (`effects.state` CHECK).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Dispatching => "dispatching",
            Self::Acknowledged => "acknowledged",
            Self::Failed => "failed",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// F8/Appendix B `effects.certainty` — what a failed dispatch can prove:
/// `absent` means the mutation provably never ran, `unknown` means it might
/// have (F20's `effectCertainty` contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectCertainty {
    /// `absent` — provably never ran; the caller may assume nothing happened.
    Absent,
    /// `unknown` — possibly ran; nothing may be assumed.
    Unknown,
}

impl EffectCertainty {
    /// Appendix B — the stored spelling of the certainty
    /// (`effects.certainty` CHECK).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }
}

/// F5 `createdTopology` — every new tab and pane the launch created before a
/// failure, so the caller can inspect what exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedTopology {
    /// The tab the launch created, when it made one.
    pub tab: Option<TabId>,
    /// The panes the launch created.
    pub panes: Vec<PaneId>,
}

/// The typed payload an effect's result carries back (`effects.result_json`)
/// — the data a transition needs, not just a receipt.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectReceipt {
    /// `agent_start` acknowledged — the F2 identity parts captured at start
    /// (F15).
    AgentStarted {
        /// The captured child identity.
        identity: ChildIdentity,
    },
    /// `jev_evaluate` resolved — the judgment set and its answers (F12/F23/
    /// F24); persisted with the result commit.
    Judgments(JudgmentRecord),
    /// `tab_create` — the tab that was created.
    TabCreated {
        /// The created tab.
        tab: TabId,
    },
    /// `pane_split` — the pane that was created.
    PaneCreated {
        /// The created pane.
        pane: PaneId,
    },
}

/// How an in-flight effect resolved — the payload of an `effect_result`
/// event (F8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectOutcome {
    /// The mutation ran to completion; the receipt carries what it produced.
    Acknowledged,
    /// A typed pre-interactive failure (the busy-pane class): provably never
    /// ran — journals as `failed`/`absent`; F15 moves to the next candidate.
    PreInteractiveFailed,
    /// The effect failed; `certainty` says whether the mutation provably
    /// never ran (`absent`) or might have (`unknown`).
    Failed {
        /// The F8/F20 certainty of the failure.
        certainty: EffectCertainty,
    },
    /// `dispatching` without a receipt across a restart — never dispatched
    /// again (F8).
    Unconfirmed,
}

/// F8 — an effect's resolution, delivered to the transition as an
/// `effect_result` event: which journaled effect, how it resolved and what it
/// produced.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectResult {
    /// `effect_key` — the journaled effect this result resolves.
    pub key: EffectKey,
    /// Its kind (the dispatch context the lane needs).
    pub kind: EffectKind,
    /// How it resolved.
    pub outcome: EffectOutcome,
    /// What it produced, when `outcome` is `Acknowledged`.
    pub receipt: Option<EffectReceipt>,
}

/// F8/Appendix B `effects.target_json` — the captured target identity an
/// effect addresses, persisted with the journal row so a `planned` effect
/// stays dispatchable after restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectTarget {
    /// F14 — `pane_split` targets a tab: `ExistingTab` names it now; `NewTab`
    /// resolves at dispatch to the tab the sibling `tab_create` produced
    /// (`EffectReceipt::TabCreated`).
    Placement(PlacementPlan),
    /// F14 — `tab_create` targets the caller context the new tab opens in:
    /// the caller's pane, whose workspace receives it (resolved fresh at
    /// dispatch).
    CallerContext {
        /// The caller's pane — the workspace locator.
        pane: PaneId,
    },
    /// F2/F10 — `prompt` and `close` target the captured child identity,
    /// re-verified fresh before every dispatch.
    Child(ChildIdentity),
}

/// F8/Appendix B `effects` — one journaled mutation. Every kind's dispatch
/// payload is rebuilt from persisted state (`agent_start` from the Decision's
/// candidate, `prompt` from the Task or the outbox message, `jev_evaluate`
/// from the Task, `tab_create`/`pane_split`/`close` from the persisted
/// `target`); `payload_digest` identifies the rendered form.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    /// `effect_id`.
    pub id: EffectId,
    /// `effect_key` — unique; e.g. `run:<id>:prompt:task`,
    /// `run:<id>:outbox:<seq>`, `run:<id>:nudge:<episode>`, `event:<id>:hint`.
    pub key: EffectKey,
    /// `kind`.
    pub kind: EffectKind,
    /// `subject_launch_id` — the Launch it serves (exactly one subject is
    /// required — Appendix B CHECK).
    pub subject_launch: Option<LaunchId>,
    /// `subject_run_id` — the Run it serves.
    pub subject_run: Option<RunId>,
    /// `target_json` — the captured identity the mutation addresses (F8):
    /// `Placement` for `pane_split`, `CallerContext` for `tab_create`,
    /// `Child` for `prompt`/`close`; `None` for `jev_evaluate` (no external
    /// target) and `agent_start` (its pane is the topology effect's product,
    /// resolved from that receipt at dispatch).
    pub target: Option<EffectTarget>,
    /// `payload_digest` — digest of the rendered operation.
    pub payload_digest: Option<Digest>,
    /// `state`.
    pub state: EffectState,
    /// `certainty` — required when `state` is `failed` (Appendix B CHECK).
    pub certainty: Option<EffectCertainty>,
    /// `result_json` — the typed receipt once the result commits.
    pub receipt: Option<EffectReceipt>,
}

/// F8 — one journal-row write a transition requests: the effect's state,
/// certainty and result. The dispatch commit is `planned` → `dispatching`; the
/// result commit writes `acknowledged`/`failed` (+`certainty`) or
/// `unconfirmed` with its receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectWrite {
    /// `effect_key` — which effect this write updates.
    pub key: EffectKey,
    /// The state to commit.
    pub state: EffectState,
    /// The certainty to record (required when `state` is `failed`).
    pub certainty: Option<EffectCertainty>,
    /// The result to record — a `Judgments` receipt also writes the
    /// `judgment_sets`/`judgments` rows (Appendix B "Effect result").
    pub receipt: Option<EffectReceipt>,
}

/// Appendix B `runs` — the Run record the transitions read and write. The
/// CHECK invariants (`settled` ⇔ `settlement`, `settlement` ⇔ `settled_at`,
/// `unresolved` ⇒ reason) are the record's contract: `identity` is `None`
/// until `agent.start` captures the F2 parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// `run_id`.
    pub id: RunId,
    /// `launch_id` — one Run per Launch (UNIQUE).
    pub launch: LaunchId,
    /// `owner_caller_id` — the current owner (F4).
    pub owner: CallerKey,
    /// `owner_generation` — bumped by every handover/adopt (F4).
    pub owner_generation: u64,
    /// `version` — the row version; the post-write value on updates.
    pub version: u64,
    /// `state`.
    pub state: State,
    /// `prompt_certainty`.
    pub prompt_certainty: Option<PromptCertainty>,
    /// `child_name` — the unique display name.
    pub child_name: String,
    /// The captured F2 identity (`herdr_incarnation`, `terminal_id`,
    /// `agent_kind`, `agent_name`, `native_session`, `pane_id`); `None` until
    /// `agent.start` acknowledges.
    pub identity: Option<ChildIdentity>,
    /// `operating_point_id` — the point that started, when it has.
    pub operating_point: Option<OperatingPointId>,
    /// `provider` — that point's provider.
    pub provider: Option<Provider>,
    /// `tier_start` — the tier the Run started at.
    pub tier_start: Option<Tier>,
    /// `cwd` — the canonical working directory.
    pub cwd: String,
    /// `base_commit` — the git base pinned before any effect (F23/H#82).
    pub base_commit: Option<String>,
    /// `work_generation`.
    pub work_generation: u64,
    /// `evidence_generation`.
    pub evidence_generation: u64,
    /// `child_status` — the last observed status.
    pub child_status: Option<ChildStatus>,
    /// `idle_since` — when the current idle episode began.
    pub idle_since: Option<Timestamp>,
    /// `idle_deadline` — the episode's `no_handoff` bound (F25).
    pub idle_deadline: Option<Timestamp>,
    /// `repair_deadline` — set on the first rejection in a work generation
    /// and never extended (F24).
    pub repair_deadline: Option<Timestamp>,
    /// `judgment_deadline` — the Jev-unavailable bound (F24).
    pub judgment_deadline: Option<Timestamp>,
    /// `max_age_deadline` — fixed at reserve, never reset (F22).
    pub max_age_deadline: Timestamp,
    /// `nudge_episode` — the current stall/idle episode (F23).
    pub nudge_episode: u64,
    /// `nudged_episode` — which episode already got its one nudge (F23).
    pub nudged_episode: Option<u64>,
    /// `settlement` + `settlement_reason` — immutable once set (F20).
    pub settlement: Option<Settlement>,
    /// `settled_at` — set iff `settlement` is (Appendix B CHECK).
    pub settled_at: Option<Timestamp>,
}

/// Appendix B — a conditional Run-row write: apply asserts `version =
/// expected_version` (plus `settlement IS NULL` when `record.settlement` is
/// `Some` — F20 first-commit-wins) and stores `record` with `version`
/// bumped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunUpdate {
    /// The version the write was computed against (`version = :v`, F20).
    pub expected_version: u64,
    /// The post-write record.
    pub record: Run,
}

/// F4/Appendix B — a `handover` or `adopt`: the owner write is conditional on
/// `expected_owner` still owning the Run, and bumps `owner_generation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerChange {
    /// The Run being re-owned.
    pub run: RunId,
    /// The owner the write was computed against (the compare half of the
    /// conditional).
    pub expected_owner: CallerKey,
    /// The verified successor owner.
    pub owner: CallerKey,
}

/// §9/Appendix B — one row-group write inside a `Transition`'s transaction;
/// each variant names the write set its transaction performs.
#[expect(
    clippy::large_enum_variant,
    reason = "variants carry whole spec-shaped records (Launch, Run); boxing would distort the shared vocabulary"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum StateChange {
    /// F1 — persist the caller binding: the `callers` row when the key is new
    /// plus the `relay_bindings` row, in one transaction.
    BindCaller(CallerBinding),
    /// F11 — write the Launch row: admission, the persisted routing decision,
    /// or the outcome (`routed`/`done` phase writes upsert the same row).
    RecordLaunch(Launch),
    /// F13 — create the reserved Run with its `max_age_deadline`, in the
    /// routing transaction.
    ReserveRun(Run),
    /// Appendix B — the conditional run write behind "Effect result" and
    /// "Settle" (and every other row write).
    UpdateRun(RunUpdate),
    /// F4 — a `handover`/`adopt` owner change, conditional on the expected
    /// owner.
    ChangeOwner(OwnerChange),
    /// F8 — a journal-row write: the dispatch commit or the result commit.
    WriteEffect(EffectWrite),
    /// F17 — write an outbox row (enqueue, dispatch bookkeeping,
    /// resolution).
    RecordFollowUp(OutboxMessage),
    /// F20 — expire the Run's still-`queued` follow-ups in the settle
    /// transaction; dispatched ones are untouched (they stay visible in their
    /// last state).
    ExpireFollowUps {
        /// The Run whose queue expires.
        run: RunId,
        /// The recorded expiry reason.
        reason: ExpiryReason,
    },
    /// F21 — write the recovery obligation (record, dispatch, block, fail).
    RecordRecovery(RecoveryObligation),
    /// F21 — upsert the provider cooldown; it only ever lengthens.
    SetCooldown(Cooldown),
    /// F24 — write the frozen handoff row.
    FreezeHandoff(FrozenHandoff),
    /// F6/F18 — mark a mailbox event acknowledged; `ack` is idempotent.
    AckEvent(EventId),
}

/// §9 — everything a transition returns: the pure function's whole output,
/// committed as one `store::apply` transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// The row writes — the Appendix B transaction's write set, in apply
    /// order.
    pub state_changes: Vec<StateChange>,
    /// The mailbox events to emit (F18; dedup keys make repeats no-ops).
    pub events: Vec<MailboxEvent>,
    /// The effects to plan — journaled `planned`, dispatched under the F8
    /// protocol.
    pub effects: Vec<Effect>,
}

/// Appendix C — the events the total transition function matches: every
/// state against every one of these, no wildcards (F22).
#[expect(
    clippy::large_enum_variant,
    reason = "EffectResult carries its typed receipt unboxed — the lanes match on the payload directly"
)]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `obs(unique|absent|invalid)` — the child status rides inside
    /// `Observation::Unique`. `handoff_reading` carries the one-shot
    /// marked-file read Appendix C performs on `obs(absent)` in `active`
    /// (`None` in every other state and class).
    Obs {
        /// The target-local read of a fresh snapshot (F3).
        observation: Observation,
        /// The one-shot handoff read, when the transition asked for it.
        handoff_reading: Option<HandoffReading>,
    },
    /// `handoff` — a valid marked file observed while `active`; the digest
    /// names the bytes to freeze (F24).
    Handoff {
        /// The marked file's digest.
        digest: Digest,
    },
    /// `judgment(accept|reject|unavailable)` — the verdict of a completed
    /// acceptance assessment (F24); a stale judgment is ignored (Appendix C).
    Judgment(JudgmentVerdict),
    /// `deadline(idle|repair|judgment|max_age)` — the stored absolute
    /// deadline fired (F22).
    Deadline(DeadlineKind),
    /// `cancel` — `cancel {runId, closePane?}` (F20); on a settled Run it
    /// only closes the pane.
    Cancel {
        /// Whether to dispatch a verified close effect (F10).
        close_pane: bool,
    },
    /// `provider_limited` — the provider-limit judgment cleared the policy
    /// threshold (F21/F23).
    ProviderLimited,
    /// `effect_result` — a journaled effect resolved (F8).
    EffectResult(EffectResult),
    /// `restart` — the daemon restarted and re-derived the Run (F28);
    /// `dispatching` effects become `unconfirmed`, deadlines unchanged.
    Restart,
}

#[cfg(test)]
mod tests {
    use super::{
        DeadlineKind, EffectCertainty, EffectKind, EffectState, EffectTarget, PromptCertainty,
        Settlement, State, UnresolvedReason,
    };
    use crate::identity::{
        AgentKind, AgentName, ChildIdentity, HerdrIncarnation, NativeSession, PaneId, TabId,
        TerminalId,
    };
    use crate::routing::PlacementPlan;

    #[test]
    fn appendix_c_state_spellings() {
        assert_eq!(State::Reserved.as_str(), "reserved");
        assert_eq!(State::Starting.as_str(), "starting");
        assert_eq!(State::Prompting.as_str(), "prompting");
        assert_eq!(State::Active.as_str(), "active");
        assert_eq!(State::Judging.as_str(), "judging");
        assert_eq!(State::Repair.as_str(), "repair");
        assert_eq!(State::Settled.as_str(), "settled");
    }

    #[test]
    fn f16_prompt_certainty_spellings() {
        assert_eq!(PromptCertainty::Acknowledged.as_str(), "acknowledged");
        assert_eq!(PromptCertainty::Unconfirmed.as_str(), "unconfirmed");
    }

    #[test]
    fn appendix_c_deadline_kind_spellings() {
        assert_eq!(DeadlineKind::Idle.as_str(), "idle");
        assert_eq!(DeadlineKind::Repair.as_str(), "repair");
        assert_eq!(DeadlineKind::Judgment.as_str(), "judgment");
        assert_eq!(DeadlineKind::MaxAge.as_str(), "max_age");
    }

    #[test]
    fn f20_unresolved_reason_spellings() {
        assert_eq!(
            UnresolvedReason::LaunchNotStarted.as_str(),
            "launch_not_started"
        );
        assert_eq!(UnresolvedReason::LaunchFailed.as_str(), "launch_failed");
        assert_eq!(
            UnresolvedReason::JudgmentUnavailable.as_str(),
            "judgment_unavailable"
        );
        assert_eq!(
            UnresolvedReason::IdentityUnprovable.as_str(),
            "identity_unprovable"
        );
        assert_eq!(UnresolvedReason::MaxAge.as_str(), "max_age");
    }

    #[test]
    fn f20_settlement_spellings() {
        assert_eq!(Settlement::Accepted.as_str(), "accepted");
        assert_eq!(Settlement::Rejected.as_str(), "rejected");
        assert_eq!(Settlement::NoHandoff.as_str(), "no_handoff");
        assert_eq!(Settlement::PaneLost.as_str(), "pane_lost");
        assert_eq!(Settlement::Cancelled.as_str(), "cancelled");
        assert_eq!(Settlement::ProviderLimited.as_str(), "provider_limited");
        // `unresolved` never carries its reason in the settlement spelling —
        // the reason rides `runs.settlement_reason` (Appendix B).
        for reason in [
            UnresolvedReason::LaunchNotStarted,
            UnresolvedReason::LaunchFailed,
            UnresolvedReason::JudgmentUnavailable,
            UnresolvedReason::IdentityUnprovable,
            UnresolvedReason::MaxAge,
        ] {
            assert_eq!(Settlement::Unresolved { reason }.as_str(), "unresolved");
        }
    }

    #[test]
    fn f8_effect_kind_spellings() {
        assert_eq!(EffectKind::JevEvaluate.as_str(), "jev_evaluate");
        assert_eq!(EffectKind::TabCreate.as_str(), "tab_create");
        assert_eq!(EffectKind::PaneSplit.as_str(), "pane_split");
        assert_eq!(EffectKind::AgentStart.as_str(), "agent_start");
        assert_eq!(EffectKind::Prompt.as_str(), "prompt");
        assert_eq!(EffectKind::Close.as_str(), "close");
    }

    #[test]
    fn f8_effect_state_spellings() {
        assert_eq!(EffectState::Planned.as_str(), "planned");
        assert_eq!(EffectState::Dispatching.as_str(), "dispatching");
        assert_eq!(EffectState::Acknowledged.as_str(), "acknowledged");
        assert_eq!(EffectState::Failed.as_str(), "failed");
        assert_eq!(EffectState::Unconfirmed.as_str(), "unconfirmed");
    }

    #[test]
    fn f8_effect_certainty_spellings() {
        assert_eq!(EffectCertainty::Absent.as_str(), "absent");
        assert_eq!(EffectCertainty::Unknown.as_str(), "unknown");
    }

    #[test]
    fn f8_f14_effect_target_matches_kind() {
        let identity = ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: AgentKind("kind-1".into()),
            agent_name: AgentName("gov-deadbeef".into()),
            native_session: Some(NativeSession("sess-1".into())),
            pane_id: PaneId("w6:p2".into()),
        };
        let split = EffectTarget::Placement(PlacementPlan::ExistingTab {
            tab: TabId("t1".into()),
        });
        let new_tab = EffectTarget::Placement(PlacementPlan::NewTab);
        let caller = EffectTarget::CallerContext {
            pane: PaneId("w6:p1".into()),
        };
        let child = EffectTarget::Child(identity);
        // Every variant pairs with the spec kind that addresses it — the
        // exhaustive match is the shape pin (F8/F14/F10).
        let kind_of = |target: &EffectTarget| match target {
            EffectTarget::Placement(_) => EffectKind::PaneSplit,
            EffectTarget::CallerContext { pane: _ } => EffectKind::TabCreate,
            EffectTarget::Child(_) => EffectKind::Prompt,
        };
        assert_eq!(kind_of(&split), EffectKind::PaneSplit);
        assert_eq!(
            kind_of(&new_tab),
            EffectKind::PaneSplit,
            "NewTab resolves at dispatch to the sibling tab_create's TabCreated receipt (F14)"
        );
        assert_eq!(kind_of(&caller), EffectKind::TabCreate);
        assert_eq!(
            kind_of(&child),
            EffectKind::Prompt,
            "prompt and close share the captured-identity target (F10)"
        );
    }
}
