//! F1 — the caller's identity: the relay-attached envelope, its resolution
//! to a durable caller key over a fresh snapshot, and the persisted
//! `relayInstanceId` binding (ADR-0004).

use crate::task::Refusal;

use super::{AgentKind, AgentRow, NativeSession, PaneId, ProjectRoot, RelayInstanceId};

/// F1/ADR-0004 — the relay-attached envelope on every forwarded request:
/// `{paneId, projectRoot, relayInstanceId}` as relay-to-daemon framing, never
/// a tool argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerEnvelope {
    /// `paneId` — the relay's inherited `HERDR_PANE_ID`.
    pub pane_id: PaneId,
    /// `projectRoot` — realpath of the relay's git root (or cwd outside a
    /// worktree).
    pub project_root: ProjectRoot,
    /// `relayInstanceId` — minted per relay process; binds once, then every
    /// request must re-resolve to the same native session.
    pub relay_instance_id: RelayInstanceId,
}

/// F1 — the durable caller key `(agent kind, native session)`, resolved from a
/// fresh `session.snapshot`: exactly one pane with the envelope's id whose
/// occupant has a native session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallerKey {
    /// The occupying agent's harness kind.
    pub agent_kind: AgentKind,
    /// The occupying agent's native session.
    pub native_session: NativeSession,
}

/// F1/Appendix B — the persisted `relayInstanceId` → caller key binding; kept
/// across daemon restarts, re-verified by a fresh locator check per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerBinding {
    /// The resolved caller.
    pub caller: CallerKey,
    /// The bound relay instance.
    pub relay_instance: RelayInstanceId,
    /// `pane_id_at_bind` — the pane the caller occupied when bound.
    pub pane_at_bind: PaneId,
}

/// F1 — validate the relay-attached envelope before any snapshot work:
/// `paneId` must be present, `projectRoot` absolute, single-line, not
/// `/`, and canonical as given, and `relayInstanceId` the ADR-0004
/// 128-bit id in lowercase hex — it keys `relay_bindings`, so anything
/// else would conflate distinct relays under one binding. An invalid
/// envelope is refused `CALLER_IDENTITY_INVALID`: it can establish no
/// caller, and a relay-derived root that fails validation is refused,
/// never re-anchored (H#3).
pub fn validate_caller_envelope(envelope: &CallerEnvelope) -> Result<(), Refusal> {
    if envelope.pane_id.0.is_empty() {
        return Err(Refusal::CallerIdentityInvalid);
    }
    if !project_root_canonical(&envelope.project_root.0) {
        return Err(Refusal::CallerIdentityInvalid);
    }
    if !relay_instance_id_hex(&envelope.relay_instance_id.0) {
        return Err(Refusal::CallerIdentityInvalid);
    }
    Ok(())
}

/// F1 — the lexical half of the `projectRoot` rules: absolute, not `/`,
/// single-line, and already in the canonical form `realpath` produces —
/// no empty, `.` or `..` components and no NUL. Existence and symlink
/// resolution are the adapter's half; a value that fails here can never be
/// a realpath output.
fn project_root_canonical(root: &str) -> bool {
    root.starts_with('/')
        && root != "/"
        && !root.contains(['\n', '\r', '\0'])
        && root
            .split('/')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// F1/ADR-0004 — the `relayInstanceId` is a random 128-bit id rendered
/// lowercase hex on the wire and in the store: exactly 32 `[0-9a-f]`
/// characters.
fn relay_instance_id_hex(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// F1 — resolve one pane to its caller key over a fresh snapshot value:
/// the pane must appear exactly once and its occupant must report a native
/// session. Used for the envelope's own pane and for `handover`'s
/// `successorPaneId` (F6 — the successor is verified by F1).
pub fn resolve_caller_key(pane: &PaneId, agents: &[AgentRow]) -> Result<CallerKey, Refusal> {
    let mut rows = agents.iter().filter(|row| row.0 == *pane);
    let Some(row) = rows.next() else {
        return Err(Refusal::CallerIdentityMissing);
    };
    if rows.next().is_some() {
        return Err(Refusal::CallerIdentityDuplicate);
    }
    match (&row.2, &row.4) {
        (Some(kind), Some(session)) => Ok(CallerKey {
            agent_kind: kind.clone(),
            native_session: session.clone(),
        }),
        (Some(_), None) => Err(Refusal::CallerIdentitySessionless),
        (None, None | Some(_)) => Err(Refusal::CallerIdentityMissing),
    }
}

/// F1/ADR-0004 — resolve the caller for one forwarded request: validate
/// the envelope, resolve the pane against a fresh snapshot, and enforce
/// the persisted `relayInstanceId` binding — a bound id must re-resolve to
/// the same caller key, and drift is refused `CALLER_IDENTITY_MISMATCH`
/// (a replaced occupant in the same pane, `a4_native_new_replaces_session`).
/// Returns the caller key plus the `CallerBinding` the daemon persists on
/// first use; `None` for the binding when an existing one verified.
pub fn resolve_caller(
    envelope: &CallerEnvelope,
    binding: Option<&CallerBinding>,
    agents: &[AgentRow],
) -> Result<(CallerKey, Option<CallerBinding>), Refusal> {
    validate_caller_envelope(envelope)?;
    let caller = resolve_caller_key(&envelope.pane_id, agents)?;
    match binding {
        Some(bound) => {
            if bound.relay_instance != envelope.relay_instance_id || bound.caller != caller {
                Err(Refusal::CallerIdentityMismatch)
            } else {
                Ok((caller, None))
            }
        }
        None => Ok((
            caller.clone(),
            Some(CallerBinding {
                caller,
                relay_instance: envelope.relay_instance_id.clone(),
                pane_at_bind: envelope.pane_id.clone(),
            }),
        )),
    }
}
