//! Appendix C — the lifecycle transition vocabulary: `State`, `Event`,
//! `Settlement`, the version triple and deadline kinds, the F8 effect-journal
//! types, and the `Transition` value the transition function returns
//! (§9/F22), plus the total function itself: `transition`, `settle` and
//! `periodic_review` implement F20, F22, F23 and F25.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::acceptance::{FrozenHandoff, HandoffReading};
use crate::config::{OperatingPointId, Policy, Provider, Tier};
use crate::delivery::{
    ExpiryReason, MailboxEvent, MailboxEventKind, MailboxSubject, OutboxMessage,
};
use crate::identity::{
    CallerBinding, CallerKey, ChildIdentity, ChildStatus, DedupKey, Digest, EffectId, EffectKey,
    EventId, LaunchId, NativeSession, Observation, PaneId, RunId, TabId, Timestamp,
};
use crate::recovery::{Cooldown, RecoveryObligation, RecoveryOrigin, RecoveryStatus};
use crate::routing::{
    Decision, Judgment, JudgmentOutcome, JudgmentRecord, PlacementPlan, Question,
};
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
    /// `tab_create` — the created tab and its initial pane (H#102: it hosts
    /// the child, never orphaned).
    TabCreated {
        /// The created tab.
        tab: TabId,
        /// The initial pane — a `NewTab` placement's `agent_start` target.
        pane: PaneId,
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
/// effect addresses, persisted with the row so a `planned` effect stays
/// dispatchable after restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectTarget {
    /// F14 — `pane_split`'s target: the tab an `ExistingTab` placement named
    /// (right split, no focus). `NewTab` never splits — `tab.create` already
    /// yields an initial pane (H#102).
    ExistingTab(TabId),
    /// F14 — `tab_create`'s caller context: the caller's pane locates the
    /// workspace the new tab opens in (resolved fresh at dispatch).
    CallerContext(PaneId),
    /// F14/F15 — `agent_start`'s pane: `PaneCreated` for `ExistingTab`,
    /// `TabCreated`'s initial pane for `NewTab`.
    AgentPane(PlacementPlan),
    /// F2/F10 — `prompt`/`close`'s captured child identity (re-verified
    /// fresh before dispatch).
    Child(ChildIdentity),
}

/// F8/Appendix B `effects` — one journaled mutation. Every kind's dispatch
/// payload is rebuilt from persisted state: `agent_start` from the Decision's
/// candidate plus the resolved `AgentPane`, `prompt` from the Task or outbox
/// message, `jev_evaluate` from the Task, `tab_create`/`pane_split`/`close`
/// from the persisted `target`; `payload_digest` names the rendered form.
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
    /// `ExistingTab` for `pane_split`, `CallerContext` for `tab_create`,
    /// `AgentPane` for `agent_start`, `Child` for `prompt`/`close`; `None`
    /// for `jev_evaluate` (no external target). F10 re-verifies `Child`.
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
/// state against every one of these, no wildcards (F22). Stamped through
/// `Versioned`: the F20 triple they were requested against.
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

// ===========================================================================
// The transition function — F20 settlement, F22 the total Appendix C
// function, F23 supervision mapping, F25 idle and loss. Pure and total:
// every input arrives as a value; the output is one `Transition` committed
// as a single `store::apply` transaction (§9).
// ===========================================================================

/// F22 — the Appendix C transition rules as data `(state, event, outcome)`,
/// in the spec's spellings, so the table is rendered from the code and never
/// copied (Phase 3 DoD). `*` reads "any state"; `unsettled` reads "any state
/// but `settled`".
pub const TRANSITION_RULES: &[(&str, &str, &str)] = &[
    ("settled", "cancel(closePane)", "close the pane only"),
    (
        "settled",
        "any other event",
        "ignored — settlement is immutable",
    ),
    ("*", "obs(invalid)", "no change; deadlines still run"),
    (
        "unsettled",
        "deadline(max_age)",
        "settle unresolved(max_age)",
    ),
    (
        "unsettled",
        "cancel",
        "settle cancelled; closePane also closes",
    ),
    (
        "*",
        "restart",
        "dispatching effects become unconfirmed; deadlines unchanged",
    ),
    (
        "unsettled",
        "provider_limited",
        "settle provider_limited (F21)",
    ),
    (
        "reserved",
        "obs(absent)",
        "settle unresolved(launch_not_started)",
    ),
    (
        "reserved",
        "topology effect planned",
        "starting (the launch plan write moves it)",
    ),
    (
        "reserved",
        "launch abstains or fails before any effect",
        "unresolved(launch_not_started) via settle",
    ),
    (
        "starting",
        "start acknowledged",
        "prompting; task prompt planned",
    ),
    (
        "starting",
        "typed pre-interactive failure with another candidate",
        "stays starting; next candidate planned in the same pane",
    ),
    (
        "starting",
        "failure with no candidate, or unconfirmed",
        "stays starting until obs(absent) or max_age",
    ),
    (
        "starting",
        "obs(absent)",
        "settle unresolved(launch_failed)",
    ),
    (
        "prompting",
        "prompt acknowledged",
        "active; prompt_certainty acknowledged",
    ),
    (
        "prompting",
        "prompt unconfirmed or failed",
        "active; prompt_certainty unconfirmed + prompt_unconfirmed event",
    ),
    ("prompting", "obs(absent)", "settle pane_lost"),
    (
        "active",
        "obs(working)",
        "clear idle_since; the episode ends",
    ),
    (
        "active",
        "obs(idle|done) with no handoff",
        "open the idle episode; one nudge; idle_deadline set",
    ),
    (
        "active",
        "obs(blocked)",
        "ask blocked_on_input and provider_limited",
    ),
    ("active", "deadline(idle)", "settle no_handoff"),
    ("active", "handoff(valid)", "freeze; judging"),
    (
        "active",
        "obs(absent)",
        "one-shot handoff read: valid → freeze + judging; otherwise pane_lost",
    ),
    ("judging", "judgment(accept)", "settle accepted"),
    (
        "judging",
        "judgment(reject)",
        "repair; repair_deadline armed once per work generation",
    ),
    (
        "judging",
        "judgment(unavailable)",
        "stays judging until judgment_deadline",
    ),
    (
        "judging",
        "deadline(judgment)",
        "settle unresolved(judgment_unavailable)",
    ),
    (
        "judging",
        "deadline(repair) armed and passed",
        "settle rejected",
    ),
    (
        "judging",
        "obs(absent)",
        "stays judging; the frozen handoff is judged",
    ),
    ("judging", "handoff(new digest)", "re-freeze; stays judging"),
    (
        "repair",
        "repair follow-up dispatched before repair_deadline",
        "work_generation+1; active",
    ),
    (
        "repair",
        "handoff(digest not yet judged)",
        "freeze; judging (repair_deadline kept)",
    ),
    ("repair", "deadline(repair)", "settle rejected"),
    ("repair", "obs(absent)", "stays repair until the deadline"),
];

/// An absolute deadline `window` after `at` (F22 — deadlines are stored
/// absolute and never reset). Saturating: a pathological window means
/// "effectively never" rather than wrapping into the past.
fn deadline_after(at: Timestamp, window: Duration) -> Timestamp {
    let millis = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
    Timestamp(at.0.saturating_add(millis))
}

/// F20 — the version stamp a Run reads as "still holding".
fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// The empty transition — losing transitions and no-op events commit nothing
/// (F20).
fn nothing() -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// Effect keys are `run:<id>:<suffix>` (Appendix B `effect_key` examples).
fn effect_key(run: &Run, suffix: &str) -> EffectKey {
    EffectKey(format!("run:{}:{}", run.id.0, suffix))
}

/// `true` when the journal already holds an effect under `key` — the unique
/// key is the dedup (N1): re-planning the same work names the same row.
fn journaled(journal: &[Effect], key: &EffectKey) -> bool {
    journal.iter().any(|effect| effect.key == *key)
}

/// Ids are derived deterministically from the unique key they belong to, so
/// a re-planned effect or re-emitted event names the same row.
fn planned_effect(
    run: &Run,
    kind: EffectKind,
    key: EffectKey,
    target: Option<EffectTarget>,
) -> Effect {
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind,
        subject_launch: Some(run.launch.clone()),
        subject_run: Some(run.id.clone()),
        target,
        payload_digest: None,
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
    }
}

/// One mailbox event for this Run: `dedup_key` is `run:<id>:<suffix>` (F18 —
/// stable keys make repeats no-ops).
fn mailbox_event(run: &Run, kind: MailboxEventKind, suffix: &str, body: String) -> MailboxEvent {
    let dedup_key = DedupKey(format!("run:{}:{}", run.id.0, suffix));
    MailboxEvent {
        id: EventId(format!("evt:{}", dedup_key.0)),
        dedup_key,
        subject: MailboxSubject::Run(run.id.clone()),
        kind,
        body,
    }
}

/// The `settled` event's body — the settlement plus the reason for
/// `unresolved` (Appendix B `settlement_reason`).
fn settled_body(settlement: Settlement) -> String {
    match settlement {
        Settlement::Unresolved { reason } => format!(
            "{{\"settlement\":\"unresolved\",\"reason\":\"{}\"}}",
            reason.as_str()
        ),
        Settlement::Accepted
        | Settlement::Rejected
        | Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::ProviderLimited => {
            format!("{{\"settlement\":\"{}\"}}", settlement.as_str())
        }
    }
}

/// The conditional run write (Appendix B): `apply` asserts
/// `version = expected_version` — and `settlement IS NULL` when the record
/// settles — then stores `record`.
fn write_run(run: &Run, record: Run) -> StateChange {
    StateChange::UpdateRun(RunUpdate {
        expected_version: run.version,
        record,
    })
}

/// `run` with `edit` applied and `version` bumped — every row write bumps it
/// (Appendix B `runs.version`).
fn edited(run: &Run, edit: impl FnOnce(&mut Run)) -> Run {
    let mut next = run.clone();
    edit(&mut next);
    next.version = run.version.saturating_add(1);
    next
}

/// An `UpdateRun` transition when `edit` changed the row, nothing when it
/// did not — a no-change observation must not bump `version`, or every
/// reconcile read would invalidate in-flight stamped results (F20).
fn update_if_changed(run: &Run, edit: impl FnOnce(&mut Run)) -> Transition {
    let mut record = run.clone();
    edit(&mut record);
    if record == *run {
        return nothing();
    }
    record.version = run.version.saturating_add(1);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// The fields a `unique` observation refreshes: the last seen status, the
/// current locator (a move is followed — H#75) and the native session once
/// Herdr reports it (F2).
fn observe_fields(
    record: &mut Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) {
    record.child_status = status;
    if let Some(identity) = &mut record.identity {
        identity.pane_id = pane.clone();
        if let Some(session) = native_session {
            identity.native_session = Some(session.clone());
        }
    }
}

/// F23/F21 — a noul judgment clears `threshold` when its affirmative
/// probability reaches it. `Judgment.probabilities` carries the calibrated
/// distribution over the question's answer space; for a noul the wire's
/// `noul` scalar is P(yes) and lands under the `"yes"` key.
fn noul_cleared(judgment: &Judgment, threshold: f64) -> bool {
    judgment
        .probabilities
        .get("yes")
        .is_some_and(|p| p.0 >= threshold)
}

/// The noul verdict boundary when the judgment recorded no policy threshold:
/// a calibrated P(yes) resolves at the majority.
const NOUL_MAJORITY: f64 = 0.5;

/// F20 — the "Settle" transaction as a pure value: the conditional run write
/// (`settlement IS NULL AND version = :v`), the terminal event, the expiry of
/// queued follow-ups that were never dispatched, and for `provider_limited`
/// the recovery obligation plus the provider cooldown (F21).
///
/// First-commit-wins: on an already-settled Run this produces nothing — the
/// losing transition commits nothing and causes no effect.
#[must_use]
pub fn settle(run: &Run, settlement: Settlement, now: Timestamp, policy: &Policy) -> Transition {
    if run.settlement.is_some() {
        return nothing();
    }
    let record = edited(run, |next| {
        next.state = State::Settled;
        next.settlement = Some(settlement);
        next.settled_at = Some(now);
    });
    let mut state_changes = Vec::from([
        write_run(run, record),
        StateChange::ExpireFollowUps {
            run: run.id.clone(),
            reason: ExpiryReason::RunSettled,
        },
    ]);
    let mut events = Vec::new();
    match settlement {
        Settlement::Accepted => events.push(mailbox_event(
            run,
            MailboxEventKind::HandoffAccepted,
            "handoff_accepted",
            String::from("{\"handoff\":\"accepted\"}"),
        )),
        Settlement::Rejected => events.push(mailbox_event(
            run,
            MailboxEventKind::HandoffRejected,
            "handoff_rejected",
            String::from("{\"handoff\":\"rejected\"}"),
        )),
        Settlement::ProviderLimited => {
            // F21 — same transaction: the unique obligation, the cooldown
            // (only ever lengthens), and the event telling the owner that
            // closing the pane triggers recovery.
            state_changes.push(StateChange::RecordRecovery(RecoveryObligation {
                predecessor: run.id.clone(),
                origin: RecoveryOrigin::ProviderLimit,
                status: RecoveryStatus::Pending,
                reason: None,
                successor_launch: None,
                expires_at: deadline_after(now, policy.recovery_expiry),
            }));
            if let Some(provider) = &run.provider {
                state_changes.push(StateChange::SetCooldown(Cooldown {
                    provider: provider.clone(),
                    until: deadline_after(now, policy.cooldown),
                    reason: String::from("provider_limited"),
                    source_run: Some(run.id.clone()),
                }));
                events.push(mailbox_event(
                    run,
                    MailboxEventKind::CooldownHit,
                    "cooldown_hit",
                    format!("{{\"provider\":\"{}\"}}", provider.0),
                ));
            }
            events.push(mailbox_event(
                run,
                MailboxEventKind::RecoveryPending,
                "recovery_pending",
                String::from("{\"recovery\":\"pending\"}"),
            ));
        }
        Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::Unresolved { reason: _ } => {}
    }
    events.push(mailbox_event(
        run,
        MailboxEventKind::Settled,
        "settled",
        settled_body(settlement),
    ));
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}

/// F22 — the total Appendix C transition function: every `State` against
/// every `Event`, matched exhaustively. `read` is the persisted context the
/// transition consults beyond the Run row — `(decision, journal, handoffs)`:
/// the Launch's routing decision, the Run's effect journal, and its frozen
/// handoffs. `freeze_path` is the coordinator-supplied destination a new
/// freeze writes (F24).
///
/// Async results — `obs`, `handoff`, `judgment`, `deadline` and the
/// `provider_limited` judgment — apply only while the
/// `(version, work_generation, evidence_generation)` they were requested
/// against still hold (F20); a stale stamp produces nothing. `cancel`,
/// `restart` and `effect_result` are synchronous or journal-bound and apply
/// unconditionally (the journal write is durable fact).
#[must_use]
pub fn transition(
    run: &Run,
    event: &Versioned<Event>,
    now: Timestamp,
    policy: &Policy,
    read: (Option<&Decision>, &[Effect], &[FrozenHandoff]),
    freeze_path: &str,
) -> Transition {
    let (decision, journal, handoffs) = read;
    if carries_versions(&event.value) && event.requested_against != triple_of(run) {
        return nothing();
    }
    match &event.value {
        Event::Obs {
            observation,
            handoff_reading,
        } => on_observation(
            run,
            observation,
            handoff_reading.as_ref(),
            (now, policy),
            journal,
            (handoffs, freeze_path),
        ),
        Event::Handoff { digest } => {
            on_handoff(run, *digest, (now, policy), (handoffs, freeze_path))
        }
        Event::Judgment(verdict) => on_judgment(run, *verdict, (now, policy)),
        Event::Deadline(kind) => on_deadline(run, *kind, (now, policy)),
        Event::Cancel { close_pane } => on_cancel(run, *close_pane, (now, policy), journal),
        Event::ProviderLimited => settle(run, Settlement::ProviderLimited, now, policy),
        Event::EffectResult(result) => {
            on_effect_result(run, result, (now, policy), decision, journal)
        }
        Event::Restart => on_restart(run, journal),
    }
}

/// F20 — the event kinds that carry the version stamp: Jev results,
/// observations and deadlines. `cancel`, `restart` and `effect_result` are
/// not async results — the conditional writes guard them at apply time.
fn carries_versions(event: &Event) -> bool {
    match event {
        Event::Obs { .. }
        | Event::Handoff { .. }
        | Event::Judgment(..)
        | Event::Deadline(..)
        | Event::ProviderLimited => true,
        Event::Cancel { .. } | Event::EffectResult(..) | Event::Restart => false,
    }
}

/// F23 — the periodic review ask for a Run: one `jev_evaluate` per
/// `evidence_generation`, and only while the Run is `active`. Periodic
/// progress reviews pause while the owner's session is absent; acceptance
/// judgments and deadlines never pause (those run through `transition`,
/// which takes no owner-presence input). `Some` is the effect to plan.
#[must_use]
pub fn periodic_review(run: &Run, owner_absent: bool, journal: &[Effect]) -> Option<Effect> {
    if owner_absent || run.state != State::Active {
        return None;
    }
    // H#79 — unchanged evidence is never re-asked: the key carries the
    // generation, so an earlier ask (whatever its outcome) suppresses this
    // one.
    let key = effect_key(run, &format!("review:{}", run.evidence_generation));
    if journaled(journal, &key) {
        return None;
    }
    Some(planned_effect(run, EffectKind::JevEvaluate, key, None))
}

fn on_observation(
    run: &Run,
    observation: &Observation,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    journal: &[Effect],
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    match observation {
        // obs(invalid) changes nothing in any state (F3/H#74).
        Observation::Invalid => nothing(),
        Observation::Absent => on_absent(run, reading, env, handoffs),
        Observation::Unique {
            status,
            pane,
            native_session,
        } => on_unique(run, *status, pane, native_session.as_ref(), env, journal),
    }
}

fn on_absent(
    run: &Run,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let (now, policy) = env;
    match run.state {
        // The pane that would host the child is gone before it existed.
        State::Reserved => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchNotStarted,
            },
            now,
            policy,
        ),
        State::Starting => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchFailed,
            },
            now,
            policy,
        ),
        State::Prompting => settle(run, Settlement::PaneLost, now, policy),
        State::Active => {
            // F25 — a frozen handoff that has not been judged goes to
            // judgment first; otherwise the marked file is read once.
            if handoffs
                .0
                .iter()
                .any(|h| h.work_generation == run.work_generation)
            {
                enter_judging(run, env)
            } else {
                match reading {
                    Some(HandoffReading::Valid { digest }) => {
                        freeze_and_judge(run, *digest, env, handoffs.1)
                    }
                    Some(HandoffReading::NotWritten) | None => {
                        settle(run, Settlement::PaneLost, now, policy)
                    }
                }
            }
        }
        // `judging` still judges the frozen handoff; `repair` waits out its
        // deadline; `settled` ignores everything but the close rule.
        State::Judging | State::Repair | State::Settled => nothing(),
    }
}

fn on_unique(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    match run.state {
        // No captured identity can produce a unique observation yet (F2/F3),
        // and `settled` answers nothing.
        State::Reserved | State::Starting | State::Settled => nothing(),
        State::Prompting | State::Judging | State::Repair => {
            locate(run, status, pane, native_session)
        }
        State::Active => match status {
            Some(ChildStatus::Working) => work_resumed(run, status, pane, native_session),
            Some(ChildStatus::Idle | ChildStatus::Done) => {
                idle_observed(run, status, pane, native_session, env.0, env.1)
            }
            Some(ChildStatus::Blocked) => {
                blocked_observed(run, status, pane, native_session, journal)
            }
            None => locate(run, status, pane, native_session),
        },
    }
}

/// A `unique` observation's field refresh — child status, locator, native
/// session — with no other consequence.
fn locate(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) -> Transition {
    update_if_changed(run, |next| {
        observe_fields(next, status, pane, native_session);
    })
}

/// `active` + `obs(working)` — the episode ends when the child works again
/// (F23); the next stall or idle opens a fresh one.
fn work_resumed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) -> Transition {
    update_if_changed(run, |next| {
        observe_fields(next, status, pane, native_session);
        if next.idle_since.is_some() || next.nudged_episode == Some(next.nudge_episode) {
            next.nudge_episode = next.nudge_episode.saturating_add(1);
            next.idle_since = None;
            next.idle_deadline = None;
        }
    })
}

/// `active` + `obs(idle|done)` with no handoff — F25: the idle episode opens
/// at the observation (`idle_deadline` is the episode start + the policy
/// window) and the child gets the episode's one nudge (F23).
fn idle_observed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    let mut record = run.clone();
    observe_fields(&mut record, status, pane, native_session);
    if record.idle_since.is_none() {
        record.idle_since = Some(now);
        record.idle_deadline = Some(deadline_after(now, policy.idle_window));
    }
    let mut effects = Vec::new();
    if record.nudged_episode != Some(record.nudge_episode)
        && let Some(identity) = record.identity.clone()
    {
        effects.push(planned_effect(
            run,
            EffectKind::Prompt,
            effect_key(run, &format!("nudge:{}", run.nudge_episode)),
            Some(EffectTarget::Child(identity)),
        ));
        record.nudged_episode = Some(record.nudge_episode);
    }
    if record == *run {
        return nothing();
    }
    record.version = run.version.saturating_add(1);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects,
    }
}

/// `active` + `obs(blocked)` — F23: ask `blocked_on_input` and
/// `provider_limited` (the review set carries both). Never prompt a blocked
/// child — no `prompt` effect is ever planned here (F17).
fn blocked_observed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    journal: &[Effect],
) -> Transition {
    let mut record = run.clone();
    observe_fields(&mut record, status, pane, native_session);
    let review_key = effect_key(run, &format!("review:{}", run.evidence_generation));
    let mut effects = Vec::new();
    if !journaled(journal, &review_key) {
        effects.push(planned_effect(
            run,
            EffectKind::JevEvaluate,
            review_key,
            None,
        ));
    }
    let mut state_changes = Vec::new();
    if record != *run {
        record.version = run.version.saturating_add(1);
        state_changes.push(write_run(run, record));
    }
    Transition {
        state_changes,
        events: Vec::new(),
        effects,
    }
}

fn on_handoff(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let frozen_for_generation = handoffs
        .0
        .iter()
        .any(|h| h.work_generation == run.work_generation && h.digest == digest);
    match run.state {
        State::Active => {
            if frozen_for_generation {
                // already frozen for this generation — judging, no re-freeze
                enter_judging(run, env)
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        // a digest not yet judged freezes and judges; one already frozen was
        // already judged (F24 — unchanged digests are never re-judged).
        State::Judging | State::Repair => {
            if frozen_for_generation {
                nothing()
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        State::Reserved | State::Starting | State::Prompting | State::Settled => nothing(),
    }
}

/// Move to `judging` when the handoff was already frozen (`obs(absent)` in
/// `active`, or a repeated `handoff` event for a known digest). The freeze
/// transaction already planned the assessment, so nothing is re-planned
/// here; `judgment_deadline` is armed defensively if unset.
fn enter_judging(run: &Run, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    update_if_changed(run, |next| {
        next.state = State::Judging;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
    })
}

/// Freeze a new handoff digest and move to `judging` — the Appendix B freeze
/// transaction: the `handoffs` row, `evidence_generation+1`, and
/// `judgment_deadline` if not already set (it is never re-armed).
fn freeze_and_judge(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    freeze_path: &str,
) -> Transition {
    let (now, policy) = env;
    let generation = run.evidence_generation.saturating_add(1);
    let record = edited(run, |next| {
        next.state = State::Judging;
        next.evidence_generation = generation;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
        next.idle_since = None;
        next.idle_deadline = None;
    });
    Transition {
        state_changes: Vec::from([
            StateChange::FreezeHandoff(FrozenHandoff {
                run: run.id.clone(),
                work_generation: run.work_generation,
                digest,
                frozen_path: String::from(freeze_path),
                frozen_at: now,
            }),
            write_run(run, record),
        ]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::JevEvaluate,
            effect_key(
                run,
                &format!("accept:{}:{}", run.work_generation, generation),
            ),
            None,
        )]),
    }
}

fn on_judgment(run: &Run, verdict: JudgmentVerdict, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    match run.state {
        State::Judging => match verdict {
            JudgmentVerdict::Accept => settle(run, Settlement::Accepted, now, policy),
            JudgmentVerdict::Reject => {
                // repair_deadline arms on the first rejection of a work
                // generation and is never extended (F24).
                let deadline = run
                    .repair_deadline
                    .unwrap_or_else(|| deadline_after(now, policy.repair_window));
                let record = edited(run, |next| {
                    next.state = State::Repair;
                    next.repair_deadline = Some(deadline);
                    next.idle_since = None;
                    next.idle_deadline = None;
                });
                Transition {
                    state_changes: Vec::from([write_run(run, record)]),
                    events: Vec::from([mailbox_event(
                        run,
                        MailboxEventKind::HandoffRejected,
                        &format!(
                            "handoff_rejected:{}:{}",
                            run.work_generation, run.evidence_generation
                        ),
                        String::from("{\"verdict\":\"reject\"}"),
                    )]),
                    effects: Vec::new(),
                }
            }
            // Jev could not complete: the Run waits on judgment_deadline
            // (armed at the freeze; defensive re-arm here).
            JudgmentVerdict::Unavailable => update_if_changed(run, |next| {
                if next.judgment_deadline.is_none() {
                    next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
                }
            }),
        },
        // A fresh verdict is meaningful only while a frozen handoff is being
        // judged; everywhere else it is ignored.
        State::Reserved
        | State::Starting
        | State::Prompting
        | State::Active
        | State::Repair
        | State::Settled => nothing(),
    }
}

fn on_deadline(run: &Run, kind: DeadlineKind, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    if run.state == State::Settled {
        return nothing();
    }
    let overdue = |deadline: Option<Timestamp>| deadline.is_some_and(|d| now >= d);
    match kind {
        DeadlineKind::MaxAge => {
            if now >= run.max_age_deadline {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::MaxAge,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
        DeadlineKind::Idle => {
            if run.state == State::Active && overdue(run.idle_deadline) {
                settle(run, Settlement::NoHandoff, now, policy)
            } else {
                nothing()
            }
        }
        // The repair deadline keeps running through `judging` — a re-frozen
        // handoff does not extend it (F24).
        DeadlineKind::Repair => {
            if (run.state == State::Repair || run.state == State::Judging)
                && overdue(run.repair_deadline)
            {
                settle(run, Settlement::Rejected, now, policy)
            } else {
                nothing()
            }
        }
        DeadlineKind::Judgment => {
            if run.state == State::Judging && overdue(run.judgment_deadline) {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::JudgmentUnavailable,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
    }
}

fn on_cancel(
    run: &Run,
    close_pane: bool,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    // `settled` accepts only `cancel` with `closePane` — and only the pane
    // close is planned (F20).
    if run.state == State::Settled {
        if close_pane && let Some(effect) = close_effect(run, journal) {
            return Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: Vec::from([effect]),
            };
        }
        return nothing();
    }
    let mut transition = settle(run, Settlement::Cancelled, now, policy);
    if close_pane && let Some(effect) = close_effect(run, journal) {
        transition.effects.push(effect);
    }
    transition
}

/// The verified close effect (F10) — planned once (`run:<id>:close` is the
/// dedup) and only while a captured identity exists to verify against.
fn close_effect(run: &Run, journal: &[Effect]) -> Option<Effect> {
    let key = effect_key(run, "close");
    if journaled(journal, &key) {
        return None;
    }
    run.identity.clone().map(|identity| {
        planned_effect(
            run,
            EffectKind::Close,
            key,
            Some(EffectTarget::Child(identity)),
        )
    })
}

/// F8 — how a resolution journals.
fn journal_state(outcome: EffectOutcome) -> EffectState {
    match outcome {
        EffectOutcome::Acknowledged => EffectState::Acknowledged,
        // pre-interactive failures provably never ran → failed/absent (F15)
        EffectOutcome::PreInteractiveFailed
        | EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        } => EffectState::Failed,
        EffectOutcome::Unconfirmed => EffectState::Unconfirmed,
    }
}

/// F8 — the certainty a result commit records (required on `failed`).
fn result_certainty(outcome: EffectOutcome) -> Option<EffectCertainty> {
    match outcome {
        EffectOutcome::PreInteractiveFailed => Some(EffectCertainty::Absent),
        EffectOutcome::Failed { certainty } => Some(certainty),
        EffectOutcome::Acknowledged | EffectOutcome::Unconfirmed => None,
    }
}

fn on_restart(run: &Run, journal: &[Effect]) -> Transition {
    // F8 — dispatching without a receipt becomes unconfirmed and is never
    // dispatched again; `planned` effects still may. No deadline changes.
    let mut state_changes = Vec::new();
    for effect in journal {
        if effect.state == EffectState::Dispatching {
            state_changes.push(StateChange::WriteEffect(EffectWrite {
                key: effect.key.clone(),
                state: EffectState::Unconfirmed,
                certainty: None,
                receipt: None,
            }));
        }
    }
    let mut events = Vec::new();
    // F16 — a task prompt that was dispatching becomes unconfirmed: possibly
    // consumed, no resubmission, the Run goes `active` with certainty
    // recorded and the caller is notified.
    if run.state == State::Prompting {
        let prompt_key = effect_key(run, "prompt:task");
        let interrupted = journal
            .iter()
            .any(|e| e.key == prompt_key && e.state == EffectState::Dispatching);
        if interrupted {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Unconfirmed);
            });
            state_changes.push(write_run(run, record));
            events.push(mailbox_event(
                run,
                MailboxEventKind::PromptUnconfirmed,
                "prompt_unconfirmed",
                String::from("{\"prompt\":\"unconfirmed\"}"),
            ));
        }
    }
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}

fn on_effect_result(
    run: &Run,
    result: &EffectResult,
    env: (Timestamp, &Policy),
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    // F20 — a Jev result applies only while its versions still hold; a stale
    // one journals its set with outcome `stale` and does nothing else.
    let stale = match &result.receipt {
        Some(EffectReceipt::Judgments(record)) => match record.set.versions {
            Some(versions) => versions != triple_of(run),
            None => false,
        },
        Some(
            EffectReceipt::AgentStarted { .. }
            | EffectReceipt::TabCreated { .. }
            | EffectReceipt::PaneCreated { .. },
        )
        | None => false,
    };
    let receipt = if stale {
        match &result.receipt {
            Some(EffectReceipt::Judgments(record)) => {
                let mut marked = record.clone();
                marked.set.outcome = JudgmentOutcome::Stale;
                Some(EffectReceipt::Judgments(marked))
            }
            Some(
                EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => result.receipt.clone(),
        }
    } else {
        result.receipt.clone()
    };
    let mut transition = Transition {
        state_changes: Vec::from([StateChange::WriteEffect(EffectWrite {
            key: result.key.clone(),
            state: journal_state(result.outcome),
            certainty: result_certainty(result.outcome),
            receipt,
        })]),
        events: Vec::new(),
        effects: Vec::new(),
    };
    if stale {
        return transition;
    }
    let consequences = match run.state {
        State::Reserved | State::Starting => launch_result(run, result, decision, journal),
        State::Prompting => prompt_result(run, result),
        State::Repair => repair_result(run, result, env),
        State::Active | State::Judging => review_result(run, result, now, policy),
        State::Settled => nothing(),
    };
    transition.state_changes.extend(consequences.state_changes);
    transition.events.extend(consequences.events);
    transition.effects.extend(consequences.effects);
    transition
}

/// `reserved`/`starting` — the launch pipeline's effect results (F14/F15):
/// topology acknowledgements plan the first `agent_start`; a start result
/// captures identity or walks the persisted candidates.
fn launch_result(
    run: &Run,
    result: &EffectResult,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    match result.kind {
        EffectKind::TabCreate => match result.outcome {
            // a new tab's initial pane hosts the child — never split (H#102)
            EffectOutcome::Acknowledged => plan_agent_start(run, PlacementPlan::NewTab, 0),
            EffectOutcome::PreInteractiveFailed
            | EffectOutcome::Failed {
                certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
            }
            | EffectOutcome::Unconfirmed => nothing(),
        },
        EffectKind::PaneSplit => match result.outcome {
            EffectOutcome::Acknowledged => {
                let plan = journal
                    .iter()
                    .find(|e| e.key == result.key)
                    .and_then(|e| e.target.clone())
                    .and_then(|target| match target {
                        EffectTarget::ExistingTab(tab) => Some(PlacementPlan::ExistingTab { tab }),
                        EffectTarget::CallerContext(_)
                        | EffectTarget::AgentPane(_)
                        | EffectTarget::Child(_) => None,
                    });
                match plan {
                    Some(placement) => plan_agent_start(run, placement, 0),
                    None => nothing(),
                }
            }
            EffectOutcome::PreInteractiveFailed
            | EffectOutcome::Failed {
                certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
            }
            | EffectOutcome::Unconfirmed => nothing(),
        },
        EffectKind::AgentStart => agent_start_result(run, result, decision, journal),
        // launch evaluation, stray prompts, closes: the journal write stands
        // on its own — other lanes consume the rows.
        EffectKind::JevEvaluate | EffectKind::Prompt | EffectKind::Close => nothing(),
    }
}

/// The `agent_start` effect for candidate `index` into `plan`'s pane.
fn plan_agent_start(run: &Run, plan: PlacementPlan, index: usize) -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::AgentStart,
            effect_key(run, &format!("start:{index}")),
            Some(EffectTarget::AgentPane(plan)),
        )]),
    }
}

fn agent_start_result(
    run: &Run,
    result: &EffectResult,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    match result.outcome {
        EffectOutcome::Acknowledged => match &result.receipt {
            Some(EffectReceipt::AgentStarted { identity }) => {
                started(run, result, identity, decision, journal)
            }
            // an acknowledgement without the captured identity is not a
            // start — the Run waits on obs(absent) or max_age
            Some(
                EffectReceipt::Judgments(_)
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => nothing(),
        },
        // F15 — the pane is provably back at its shell: try the next
        // candidate in the same pane; with none left, wait for obs(absent)
        // or max_age.
        EffectOutcome::PreInteractiveFailed => {
            let tried = journal
                .iter()
                .filter(|e| e.kind == EffectKind::AgentStart)
                .count();
            let target = journal
                .iter()
                .find(|e| e.key == result.key)
                .and_then(|e| e.target.clone());
            let has_next = decision.is_some_and(|d| tried < d.candidates.len());
            if has_next {
                match target {
                    Some(EffectTarget::AgentPane(plan)) => {
                        return plan_agent_start(run, plan, tried);
                    }
                    Some(
                        EffectTarget::ExistingTab(_)
                        | EffectTarget::CallerContext(_)
                        | EffectTarget::Child(_),
                    )
                    | None => {}
                }
            }
            nothing()
        }
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        }
        | EffectOutcome::Unconfirmed => nothing(),
    }
}

/// `agent_start` acknowledged — capture the F2 identity, record the started
/// candidate's point/provider/tier, move to `prompting` and plan the Task
/// prompt (F15/F16). The candidate is identified by the effect's position
/// among the journal's `agent_start` entries (keys are `start:<index>`).
fn started(
    run: &Run,
    result: &EffectResult,
    identity: &ChildIdentity,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    let index = journal
        .iter()
        .filter(|e| e.kind == EffectKind::AgentStart)
        .position(|e| e.key == result.key);
    let candidate = index.and_then(|i| decision.and_then(|d| d.candidates.get(i)));
    let record = edited(run, |next| {
        next.state = State::Prompting;
        next.identity = Some(identity.clone());
        if let Some(c) = candidate {
            next.operating_point = Some(c.operating_point.clone());
            next.provider = Some(c.provider.clone());
            next.tier_start = Some(c.tier.clone());
        }
    });
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::Prompt,
            effect_key(run, "prompt:task"),
            Some(EffectTarget::Child(identity.clone())),
        )]),
    }
}

/// `prompting` — the Task prompt's result (F16): acknowledged means the ack
/// matched the captured identity; anything else means possibly consumed —
/// `prompt_certainty = unconfirmed`, `active`, and the caller is notified.
fn prompt_result(run: &Run, result: &EffectResult) -> Transition {
    if result.kind != EffectKind::Prompt || result.key != effect_key(run, "prompt:task") {
        return nothing();
    }
    match result.outcome {
        EffectOutcome::Acknowledged => {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Acknowledged);
            });
            Transition {
                state_changes: Vec::from([write_run(run, record)]),
                events: Vec::new(),
                effects: Vec::new(),
            }
        }
        EffectOutcome::PreInteractiveFailed
        | EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        }
        | EffectOutcome::Unconfirmed => {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Unconfirmed);
            });
            Transition {
                state_changes: Vec::from([write_run(run, record)]),
                events: Vec::from([mailbox_event(
                    run,
                    MailboxEventKind::PromptUnconfirmed,
                    "prompt_unconfirmed",
                    String::from("{\"prompt\":\"unconfirmed\"}"),
                )]),
                effects: Vec::new(),
            }
        }
    }
}

/// `repair` — a repair follow-up (`outbox:<seq>`) dispatched before
/// `repair_deadline` opens a new work generation and returns the Run to
/// `active` (F24); anything else is supervision.
fn repair_result(run: &Run, result: &EffectResult, env: (Timestamp, &Policy)) -> Transition {
    if result.kind == EffectKind::Prompt
        && result.outcome == EffectOutcome::Acknowledged
        && result
            .key
            .0
            .starts_with(&format!("run:{}:outbox:", run.id.0))
        && run.repair_deadline.is_none_or(|d| env.0 < d)
    {
        let record = edited(run, |next| {
            next.state = State::Active;
            next.work_generation = next.work_generation.saturating_add(1);
            next.repair_deadline = None;
            next.idle_since = None;
            next.idle_deadline = None;
        });
        return Transition {
            state_changes: Vec::from([write_run(run, record)]),
            events: Vec::new(),
            effects: Vec::new(),
        };
    }
    review_result(run, result, env.0, env.1)
}

/// An answered judgment set → the F23 supervision mapping; anything else is
/// journal-only here (acceptance verdicts arrive as `judgment` events).
fn review_result(run: &Run, result: &EffectResult, now: Timestamp, policy: &Policy) -> Transition {
    match (&result.receipt, result.outcome) {
        (Some(EffectReceipt::Judgments(record)), EffectOutcome::Acknowledged)
            if record.set.outcome == JudgmentOutcome::Answered =>
        {
            apply_review(run, record, now, policy)
        }
        (
            Some(
                EffectReceipt::Judgments(_)
                | EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None,
            _,
        ) => nothing(),
    }
}

/// F23 — the supervision answers of an answered review/provider-limit set,
/// applied while their versions still hold. `provider_limited` is terminal
/// and is checked before the advisory answers; `no_recent_progress` nudges
/// once per episode (the second stall in one episode reports `stalled`).
fn apply_review(run: &Run, record: &JudgmentRecord, now: Timestamp, policy: &Policy) -> Transition {
    for judgment in &record.judgments {
        if judgment.question == Question::ProviderLimited
            && noul_cleared(judgment, policy.provider_limit_threshold)
        {
            return settle(run, Settlement::ProviderLimited, now, policy);
        }
    }
    let mut next = run.clone();
    let mut events = Vec::new();
    let mut effects = Vec::new();
    for judgment in &record.judgments {
        match judgment.question {
            Question::BlockedOnInput => {
                if noul_cleared(judgment, judgment.threshold.unwrap_or(NOUL_MAJORITY)) {
                    events.push(mailbox_event(
                        run,
                        MailboxEventKind::BlockedOnInput,
                        &format!("blocked_on_input:{}", run.evidence_generation),
                        String::from("{\"question\":\"blocked_on_input\"}"),
                    ));
                }
            }
            Question::OutsideScope => {
                if noul_cleared(judgment, judgment.threshold.unwrap_or(NOUL_MAJORITY)) {
                    events.push(mailbox_event(
                        run,
                        MailboxEventKind::OutsideScope,
                        &format!("outside_scope:{}", run.evidence_generation),
                        String::from("{\"question\":\"outside_scope\"}"),
                    ));
                }
            }
            Question::NoRecentProgress => {
                if noul_cleared(judgment, judgment.threshold.unwrap_or(NOUL_MAJORITY)) {
                    if next.nudged_episode == Some(next.nudge_episode) {
                        // stalled after the nudge — actionable (F18)
                        events.push(mailbox_event(
                            run,
                            MailboxEventKind::Stalled,
                            &format!("stalled:{}", run.nudge_episode),
                            String::from("{\"stalled\":true}"),
                        ));
                    } else if let Some(identity) = next.identity.clone() {
                        effects.push(planned_effect(
                            run,
                            EffectKind::Prompt,
                            effect_key(run, &format!("nudge:{}", run.nudge_episode)),
                            Some(EffectTarget::Child(identity)),
                        ));
                        next.nudged_episode = Some(next.nudge_episode);
                    } else {
                        // no captured identity: the episode's nudge stays
                        // unconsumed — there is no pane to send it to.
                    }
                }
            }
            Question::DoneWhenVerifiable
            | Question::WeakestSufficientTier
            | Question::ChangesFiles
            | Question::SecurityBoundary
            | Question::NeedsExternal
            | Question::LongRunning
            | Question::RelatedTab
            | Question::ProviderLimited
            | Question::HandoffMeetsItem { item: _ } => {}
        }
    }
    let mut state_changes = Vec::new();
    if next != *run {
        next.version = run.version.saturating_add(1);
        state_changes.push(write_run(run, next));
    }
    Transition {
        state_changes,
        events,
        effects,
    }
}

#[cfg(test)]
mod tests;
