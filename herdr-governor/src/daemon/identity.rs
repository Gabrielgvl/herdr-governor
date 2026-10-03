//! `identity` — the daemon's half of F1 (spec F1, ADR-0004, §4.6): the
//! relay-attached `CallerEnvelope` validates lexically, its `projectRoot`
//! must equal the connection task's realpath read, and its pane resolves
//! to exactly one caller over the request-time snapshot. The persisted
//! `relayInstanceId` binding is then enforced — a drift refuses
//! `CALLER_IDENTITY_MISMATCH` before the tool call runs.
//!
//! The I/O halves stay outside: `mcp`'s connection task takes the
//! `session.snapshot` and the `tokio::fs::canonicalize` read and posts
//! them in `Msg::Tool`; this module maps and judges values only.

use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerEnvelope, CallerKey, ChildStatus, HerdrIncarnation,
    NativeSession, PaneId, TerminalId, resolve_caller, validate_caller_envelope,
};
use governor_core::task::Refusal;

use crate::adapters::herdr::{AgentStatus, ConnEpoch, SessionSnapshot};
use crate::store::Store;

use super::api::ToolError;

/// The core's `AgentRow` tuple, re-aliased — the core keeps the name
/// private but the tuple itself is the public contract: one snapshot
/// occupant row `(pane, terminal, kind, name, session, status)`.
pub type AgentRow = (
    PaneId,
    TerminalId,
    Option<AgentKind>,
    Option<AgentName>,
    Option<NativeSession>,
    Option<ChildStatus>,
);

/// §4.6 — the wire `AgentStatus` to `ChildStatus`: the four real states
/// map 1:1 and `unknown` is `None` (already dropped by
/// `SessionSnapshot::agent_rows`, kept total here anyway).
fn child_status(status: AgentStatus) -> Option<ChildStatus> {
    match status {
        AgentStatus::Idle => Some(ChildStatus::Idle),
        AgentStatus::Working => Some(ChildStatus::Working),
        AgentStatus::Blocked => Some(ChildStatus::Blocked),
        AgentStatus::Done => Some(ChildStatus::Done),
        AgentStatus::Unknown => None,
    }
}

/// §4.6 — a snapshot's `agents` rows as core `AgentRow` tuples for
/// `resolve_caller`/`classify` (F1/F3's shared input shape).
#[must_use]
pub fn agent_rows(snapshot: &SessionSnapshot) -> Vec<AgentRow> {
    snapshot
        .agent_rows()
        .iter()
        .map(|row| {
            (
                PaneId(row.pane_id.clone()),
                TerminalId(row.terminal_id.clone()),
                row.agent.clone().map(AgentKind),
                row.name.clone().map(AgentName),
                row.native_session.clone().map(NativeSession),
                row.status.and_then(child_status),
            )
        })
        .collect()
}

/// F2/OQ-8 — protocol 22 proves no incarnation, so the daemon derives
/// `HerdrIncarnation` from the `ConnEpoch`: the socket's inode and mtime
/// identify the serving Herdr process.
#[must_use]
pub fn incarnation(epoch: &ConnEpoch) -> HerdrIncarnation {
    HerdrIncarnation(format!(
        "{}:{}.{:09}",
        epoch.socket_inode, epoch.socket_mtime_secs, epoch.socket_mtime_nsecs
    ))
}

/// F1/§4.6 — resolve the caller for one forwarded request:
///
/// 1. `validate_caller_envelope` — the lexical envelope rules;
/// 2. `projectRoot` must equal `resolved_root`, the connection task's
///    `tokio::fs::canonicalize` read (`None` when the path does not
///    resolve) — a mismatch is `CALLER_IDENTITY_INVALID`, never
///    re-anchored (H#3);
/// 3. the persisted `relayInstanceId` binding is read and
///    `resolve_caller` enforces it — a bound id re-resolving to a
///    different caller is `CALLER_IDENTITY_MISMATCH`.
///
/// `Ok((caller, Some(binding)))` means a first use: the coordinator
/// journals `StateChange::BindCaller` before the tool runs. A failed
/// store read is `DAEMON_UNAVAILABLE` — an internal fault, not an
/// identity verdict.
pub fn resolve(
    store: &Store,
    envelope: &CallerEnvelope,
    resolved_root: Option<&str>,
    agents: &[AgentRow],
) -> Result<(CallerKey, Option<CallerBinding>), ToolError> {
    if let Err(refusal) = validate_caller_envelope(envelope) {
        return Err(ToolError::refusal(refusal, "malformed caller envelope"));
    }
    if resolved_root != Some(envelope.project_root.0.as_str()) {
        return Err(ToolError::refusal(
            Refusal::CallerIdentityInvalid,
            "projectRoot is not the canonical realpath",
        ));
    }
    let binding = store
        .relay_binding(&envelope.relay_instance_id)
        .map_err(|_err| {
            ToolError::new(ToolError::DAEMON_UNAVAILABLE, "caller binding read failed")
        })?;
    resolve_caller(envelope, binding.as_ref(), agents)
        .map_err(|refusal| ToolError::refusal(refusal, "caller identity refused"))
}
