//! `states` — the `state` payloads Jev judges: the F12 task projection
//! and the F23/F24 evidence-bearing states (P5.J2). Every evidence state
//! carries only the fields the spec names: the Task digest
//! (`objective`, `doneWhen`, `constraints`), the bounded transcript tail
//! or its `agent.read` fallback, the git evidence against the pinned
//! base (omitted when the Run pinned none), the frozen handoff at
//! acceptance, and the typed provider-limit record on a blocked ask.
//! `scope` reaches Jev only as the `outside_scope` question's own input
//! on a review ask. By construction there is no field for an operating
//! point, provider, tier request, label, caller or Run identity (F12,
//! F23/F24, H#41): a `State` cannot carry them, so the request cannot
//! either.

use governor_core::task::Task;
use serde::Serialize;

/// The semantic state Jev judges. By construction there is no field for
/// an operating point, provider, tier request or label (F12, H#41): a
/// `State` cannot carry them, so the request cannot either.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// F12 launch evaluation — `{"task": {objective, scope, doneWhen,
    /// constraints}}`.
    Task(TaskState),
    /// F23 periodic review — `{"review": {task, scope, transcript,
    /// terminal?, git?}}`.
    Review(ReviewState),
    /// F23/F21 blocked-episode ask — `{"blocked": {task, transcript,
    /// terminal?, git?, limitRecord?}}`.
    Blocked(BlockedState),
    /// F24 acceptance ask — `{"acceptance": {task, handoff, transcript,
    /// terminal?, git?}}`.
    Acceptance(AcceptanceState),
}

/// The four semantic Task fields and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskState {
    /// `task.objective`.
    pub objective: String,
    /// `task.scope`.
    pub scope: String,
    /// `task.doneWhen`.
    #[serde(rename = "doneWhen")]
    pub done_when: Vec<String>,
    /// `task.constraints`.
    pub constraints: Vec<String>,
}

impl From<&Task> for TaskState {
    /// Projects the Task: `tier`, `recovery_of`, `label` and `cwd` never
    /// reach Jev (F12, H#41).
    fn from(task: &Task) -> Self {
        Self {
            objective: task.objective.clone(),
            scope: task.scope.clone(),
            done_when: task.done_when.clone(),
            constraints: task.constraints.clone(),
        }
    }
}

/// The F23 Task digest every evidence state carries — `objective`,
/// `doneWhen`, `constraints` and nothing else (`scope` is the
/// `outside_scope` question's own input on a review ask, not part of
/// the digest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDigest {
    /// `task.objective`.
    pub objective: String,
    /// `task.doneWhen`.
    #[serde(rename = "doneWhen")]
    pub done_when: Vec<String>,
    /// `task.constraints`.
    pub constraints: Vec<String>,
}

impl From<&Task> for TaskDigest {
    /// Projects the Task's digest: `scope`, `tier`, `recovery_of`,
    /// `label` and `cwd` never reach it (F12, F23, H#41).
    fn from(task: &Task) -> Self {
        Self {
            objective: task.objective.clone(),
            done_when: task.done_when.clone(),
            constraints: task.constraints.clone(),
        }
    }
}

/// One normalized record of the bounded transcript tail — the
/// supervision fields verbatim, with absent fields omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptLine {
    /// The record's own timestamp, verbatim; omitted when the source
    /// carries none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// The record's role, verbatim; omitted when the source carries
    /// none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The evidence class the record carries — the transcript adapter's
    /// `EventKind` spelling, opaque here.
    pub kind: String,
    /// Message or step text, or the error name; omitted when the record
    /// has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// The git evidence against the pinned base, verbatim
/// (`worktree_evidence`): `{head, dirty}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitState {
    /// Full commit id `HEAD` resolved to.
    pub head: String,
    /// Changed and untracked paths, verbatim and in git's order.
    pub dirty: Vec<String>,
}

/// The typed provider-limit record a blocked ask carries as evidence
/// (F31): `{source, observedAt, resetAt?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimitRecordState {
    /// The record source — the transcript adapter's typed source,
    /// rendered by the caller (harness names are catalog data, never
    /// literals here).
    pub source: String,
    /// When the limit was observed, RFC 3339 text.
    #[serde(rename = "observedAt")]
    pub observed_at: String,
    /// When the provider says the limit resets, when stated.
    #[serde(rename = "resetAt", skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}

/// The F23 periodic-review state: the Task digest, the `scope` the
/// `outside_scope` question receives, and the evidence bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewState {
    /// The Task digest.
    pub task: TaskDigest,
    /// `task.scope` — `outside_scope` also receives it (F23).
    pub scope: String,
    /// The bounded transcript tail.
    pub transcript: Vec<TranscriptLine>,
    /// The `agent.read` fallback, present only when the transcript
    /// source failed (F23: "otherwise `agent.read`").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// The worktree evidence, present only when the Run pinned a base
    /// (a `base_commit: None` Run omits git evidence entirely).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitState>,
}

/// The F23/F21 blocked-episode state: the same evidence bundle, plus
/// the optional typed provider-limit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedState {
    /// The Task digest.
    pub task: TaskDigest,
    /// The bounded transcript tail.
    pub transcript: Vec<TranscriptLine>,
    /// The `agent.read` fallback, present only when the transcript
    /// source failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// The worktree evidence, present only when the Run pinned a base.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitState>,
    /// The typed provider-limit record, when one was observed in the
    /// child's native evidence (F31).
    #[serde(rename = "limitRecord", skip_serializing_if = "Option::is_none")]
    pub limit_record: Option<LimitRecordState>,
}

/// The F24 acceptance state: the frozen handoff judged against the same
/// evidence bundle, per `handoff_meets_item_k`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceState {
    /// The Task digest.
    pub task: TaskDigest,
    /// The frozen handoff bytes (F24).
    pub handoff: String,
    /// The bounded transcript tail.
    pub transcript: Vec<TranscriptLine>,
    /// The `agent.read` fallback, present only when the transcript
    /// source failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// The worktree evidence, present only when the Run pinned a base.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitState>,
}
