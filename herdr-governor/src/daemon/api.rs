//! `api` — the tool-call value types (spec §9, the P5.M1/M2 contract).
//! `mcp::tools` maps `tools/call` into a `ToolRequest` and a `ToolResponse`
//! back onto the wire; the strict JSON DTOs and schemas live in `mcp::`
//! (OQ-A: hand-rolled, no `rmcp`/`schemars`). These types are the stable
//! typed boundary between the transport and the coordinator: `caller` is
//! the relay-attached envelope (F1), framing, never a tool argument.

use std::path::Path;

use governor_core::identity::{CallerEnvelope, EventId, IdempotencyKey, MessageKey, PaneId, RunId};
use governor_core::task::{Refusal, Task};

use crate::adapters::git::{self, GitError};

/// One forwarded tool call: the relay-attached caller envelope (F1) plus
/// the validated call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    /// The caller envelope the relay attached — daemon-side F1 resolves it.
    pub caller: CallerEnvelope,
    /// The validated call.
    pub call: ToolCall,
}

/// Which task-facing tool was called and its typed arguments (F5–F7).
/// Field-set strictness (unknown keys, types) is the `mcp::` schema's job —
/// by the time a call is a `ToolCall` its arguments are typed domain values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolCall {
    /// `herdr_launch` (F5) — start a Task.
    Launch {
        /// The caller-scoped idempotency key.
        key: IdempotencyKey,
        /// The validated Task.
        task: Task,
    },
    /// `herdr_run` (F6) — a run-scoped action.
    Run(RunAction),
    /// `herdr_status` (F7) — the caller's health/status page.
    Status {
        /// Wait for this mailbox event before answering, when named.
        event: Option<EventId>,
        /// The opaque resume cursor from a previous page.
        cursor: Option<String>,
    },
}

/// F6 — the `herdr_run` actions. `observe` is run-scoped; `ack` is
/// event-scoped; `handover`/`adopt` name run sets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunAction {
    /// `observe {runId, cursor?}` — a page of the Run's transcript/evidence.
    Observe {
        /// The Run to observe.
        run: RunId,
        /// The opaque resume cursor.
        cursor: Option<String>,
    },
    /// `message {runId, messageKey, text}` — a follow-up for the Run's
    /// outbox (F17).
    Message {
        /// The Run the message addresses.
        run: RunId,
        /// The caller-scoped message key (F17 dedup).
        key: MessageKey,
        /// The message text — validated, never logged.
        text: String,
    },
    /// `ack {eventId}` — acknowledge a mailbox event (F18).
    Ack {
        /// The mailbox event.
        event: EventId,
    },
    /// `handover {runIds, successorPaneId}` — transfer ownership to the
    /// caller occupying `successorPaneId` (F4/F19).
    Handover {
        /// The Runs being handed over.
        runs: Vec<RunId>,
        /// The successor's pane — verified by F1.
        successor_pane: PaneId,
    },
    /// `adopt {runIds}` — claim Runs whose owner session is gone (F19).
    Adopt {
        /// The Runs being adopted.
        runs: Vec<RunId>,
    },
    /// `cancel {runId, closePane?}` (F20).
    Cancel {
        /// The Run being cancelled.
        run: RunId,
        /// Whether the child's pane is closed (F10-verified).
        close_pane: bool,
    },
}

/// What a tool call resolves to: a JSON result body on success, a typed
/// `ToolError` on refusal. The body shape is per tool (F5–F7) and owned by
/// `mcp::`; the coordinator returns either.
pub type ToolResponse = Result<serde_json::Value, ToolError>;

/// §4.2's `ToolPrepared` — the request-time evidence `mcp::serve`
/// gathers alongside the snapshot so the coordinator performs no I/O.
/// The launch fields are `Ok(None)` for every other tool.
#[derive(Debug)]
pub struct ToolPrepared {
    /// The connection task's `canonicalize` of the envelope's
    /// `projectRoot` — `None` when the path does not resolve (H#3).
    pub resolved_root: Option<String>,
    /// Launch only: the realpath of the Task's `cwd` — the project root
    /// when the Task names none. `Err` carries the `TASK_INVALID`
    /// refusal a noncanonical/nonresolving `cwd` earns before any
    /// recording; `violations` re-checks the field against the root.
    pub resolved_cwd: Result<Option<String>, ToolError>,
    /// Launch only: `base_commit` of the resolved `cwd`, taken before
    /// admission (F6): `Ok(head)` → `Some`, `NotARepo`/`UnbornHead` →
    /// `None` — a legal plain-directory Run; every other git failure is
    /// the typed `GIT_EVIDENCE_UNAVAILABLE` refusal, also before any
    /// recording.
    pub base_commit: Result<Option<String>, ToolError>,
}

impl ToolPrepared {
    /// The non-launch shape — every field carries its neutral value.
    #[must_use]
    pub fn root_only(resolved_root: Option<String>) -> Self {
        Self {
            resolved_root,
            resolved_cwd: Ok(None),
            base_commit: Ok(None),
        }
    }
}

/// F6's base-commit verdict for `cwd` — the one mapping the
/// pre-admission probe and the restart re-probe share: `Ok(head)` pins
/// the base, `NotARepo`/`UnbornHead` are the legal plain-directory
/// `None` (git evidence is then omitted for the Run).
///
/// # Errors
///
/// Every `GitError` outside those two legal-`None` classes — the
/// admission path refuses `GIT_EVIDENCE_UNAVAILABLE` before recording.
pub async fn probe_base_commit(cwd: &Path) -> Result<Option<String>, GitError> {
    match git::base_commit(cwd).await {
        Ok(head) => Ok(Some(head)),
        Err(GitError::NotARepo | GitError::UnbornHead) => Ok(None),
        Err(error) => Err(error),
    }
}

/// A tool-call refusal: a stable `code` the caller branches on plus a
/// caller-facing message. `code` is a spec refusal spelling
/// (`Refusal::code()`) or one of the named transport/schema codes below.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ToolError {
    /// The refusal code — `SCREAMING_SNAKE`, stable on the wire.
    pub code: &'static str,
    /// The caller-facing detail (never sensitive content).
    pub message: String,
}

impl ToolError {
    /// `TASK_INVALID` — the Task failed `violations` or the schema's value
    /// checks; the message names the violation codes.
    pub const TASK_INVALID: &'static str = "TASK_INVALID";
    /// `GIT_EVIDENCE_UNAVAILABLE` — `base_commit` evidence could not be
    /// taken for a `herdr_launch` (§4.2 `ToolPrepared`, → F6).
    pub const GIT_EVIDENCE_UNAVAILABLE: &'static str = "GIT_EVIDENCE_UNAVAILABLE";
    /// `RESULT_TOO_LARGE` — a result would exceed the response bound (N5).
    pub const RESULT_TOO_LARGE: &'static str = "RESULT_TOO_LARGE";
    /// `FOLLOWUP_PUBLISH_FAILED` — the body file could not be written
    /// verified (F17/H#62–64); nothing was enqueued.
    pub const FOLLOWUP_PUBLISH_FAILED: &'static str = "FOLLOWUP_PUBLISH_FAILED";
    /// `TOOL_UNKNOWN` — `tools/call` named a tool the daemon does not serve.
    pub const TOOL_UNKNOWN: &'static str = "TOOL_UNKNOWN";
    /// `REQUEST_INVALID` — the JSON-RPC call could not be decoded or its
    /// arguments did not satisfy the strict schema.
    pub const REQUEST_INVALID: &'static str = "REQUEST_INVALID";
    /// `DAEMON_UNAVAILABLE` — the caller-facing tool surface is not serving
    /// (N7; `Refusal::DaemonUnavailable`'s spelling).
    pub const DAEMON_UNAVAILABLE: &'static str = "DAEMON_UNAVAILABLE";

    /// An arbitrary refusal with a named code.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// A spec `Refusal` with a caller-facing message.
    #[must_use]
    pub fn refusal(refusal: Refusal, message: impl Into<String>) -> Self {
        Self::new(refusal.code(), message)
    }
}
