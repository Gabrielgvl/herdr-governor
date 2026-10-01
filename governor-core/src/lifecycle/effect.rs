//! F8/Appendix B — the effect-journal vocabulary: the kinds, states and
//! certainties of a journaled mutation, the typed receipts and captured
//! targets, the `Effect` row itself and the `EffectWrite` a transition
//! requests.

use crate::identity::{
    ChildIdentity, Digest, EffectId, EffectKey, LaunchId, PaneId, RunId, TabId, Timestamp,
};
use crate::routing::{JudgmentRecord, PlacementPlan};

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
    /// `dispatched_at` — the dispatch commit's time (Appendix B
    /// `effects.dispatched_at`), present once the row leaves `planned`;
    /// F24 reads it as the repair-window evidence, never the result's
    /// arrival time.
    pub dispatched_at: Option<Timestamp>,
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
