//! Appendix B — the records the lifecycle stores: the F5 created topology a
//! Launch reports, the `runs` row itself, and the conditional writes
//! against it (`RunUpdate`'s version guard, `OwnerChange`'s expected
//! owner).

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{OperatingPointId, Provider, Tier};
use crate::identity::{
    CallerKey, ChildIdentity, ChildStatus, Digest, LaunchId, PaneId, RunId, TabId, Timestamp,
};

use super::{PromptCertainty, Settlement, State};

/// F5 `createdTopology` — every new tab and pane the launch created before a
/// failure, so the caller can inspect what exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedTopology {
    /// The tab the launch created, when it made one.
    pub tab: Option<TabId>,
    /// The panes the launch created.
    pub panes: Vec<PaneId>,
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
    /// `evidence_digest` — the last recorded transcript/git evidence digest
    /// (F23): an `evidence` event carrying a different digest records it and
    /// bumps `evidence_generation`.
    pub evidence_digest: Option<Digest>,
    /// `child_status` — the last observed status.
    pub child_status: Option<ChildStatus>,
    /// `idle_since` — when the current idle episode began.
    pub idle_since: Option<Timestamp>,
    /// `idle_deadline` — the episode's `no_handoff` bound (F25).
    pub idle_deadline: Option<Timestamp>,
    /// `repair_deadline` — set on the first rejection in a work generation
    /// and never extended (F24).
    pub repair_deadline: Option<Timestamp>,
    /// `rejected_at` — that first rejection's time, persisted so the repair
    /// window's lower bound survives a policy reload (it is never derived
    /// from `repair_deadline`). Armed together with `repair_deadline` and
    /// cleared with it when the generation advances (F24, Appendix B
    /// `runs.rejected_at`).
    pub rejected_at: Option<Timestamp>,
    /// `judgment_deadline` — the Jev-unavailable bound (F24).
    pub judgment_deadline: Option<Timestamp>,
    /// `judging_digest` — the handoff digest the current acceptance ask
    /// assesses (F24, Appendix B `runs.judging_digest`): the ask's key
    /// (`accept:<work>:<evidence>`) does not name a digest, so the row
    /// records which frozen handoff it is about. Set on every judging
    /// entry, cleared when the work generation advances.
    pub judging_digest: Option<Digest>,
    /// `max_age_deadline` — fixed at reserve, never reset (F22).
    pub max_age_deadline: Timestamp,
    /// `nudge_episode` — the current stall/idle episode (F23).
    pub nudge_episode: u64,
    /// `nudged_episode` — which episode already got its one nudge (F23).
    pub nudged_episode: Option<u64>,
    /// `blocked_episode` — the current blocked episode (F23): a `blocked`
    /// observation after a non-blocked one opens the next number, and the
    /// episode's `provider_limited` ask names it (`blocked:<episode>`).
    /// A `working`, `idle` or `done` observation ends the episode.
    pub blocked_episode: u64,
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
