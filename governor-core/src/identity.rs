//! F1–F4 and F19 — caller identity, child identity, observation classes,
//! ownership and adoption, plus the opaque id/digest/time primitives the
//! other modules share. Every identifier the spec or Appendix B names is a
//! newtype here so call sites cannot swap one kind of id for another.

use alloc::string::String;
use alloc::vec::Vec;

use crate::lifecycle::{OwnerChange, Run, StateChange, Transition};
use crate::task::Refusal;

/// A sha-256 content digest (`task_digest`, `body_digest`, handoff `digest`,
/// `payload_digest`); the hasher lives behind the digest-admission lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

/// §9 — an absolute time passed in as a value: milliseconds since the unix
/// epoch. The store renders Appendix B's `TEXT` form; the core never reads a
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub i64);

/// Appendix B `runs.run_id` — one Run of a Launch's Task (uuid v7 text).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunId(pub String);

/// Appendix B `launches.launch_id` — one caller request to start a Task.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LaunchId(pub String);

/// Appendix B `mailbox.event_id` — one actionable event for a caller.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventId(pub String);

/// Appendix B `effects.effect_id` — one journaled mutation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EffectId(pub String);

/// Appendix B `effects.effect_key` — the unique dispatch key, e.g.
/// `run:<id>:prompt:task`, `run:<id>:outbox:<seq>`, `event:<id>:hint` (N1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EffectKey(pub String);

/// Appendix B `judgment_sets.set_id` — one bounded Jev request/response set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JudgmentSetId(pub String);

/// Herdr `pane_id` — a pane locator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(pub String);

/// Herdr `tab_id` — a tab locator (F14 placement, F12 `related_tab`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(pub String);

/// Herdr `terminal_id` — the stable terminal under a pane (F2, A4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TerminalId(pub String);

/// The Herdr server incarnation marker a Run's identity is bound to (A4, F28).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HerdrIncarnation(pub String);

/// The harness's own session identity — Herdr's `agent_session.value` (F1, F2,
/// F28 re-proof).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NativeSession(pub String);

/// A harness kind, carried as opaque catalog data — harness names never appear
/// as literals in this crate (N8, ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentKind(pub String);

/// A Run's minted agent name: `gov-<runId[0..8]>` (F2, H#52).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentName(pub String);

/// F1/ADR-0004 — the immutable random 128-bit id a relay mints once per
/// process; lowercase hex on the wire (Appendix B `relay_bindings`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelayInstanceId(pub String);

/// F1 — the caller's project root: absolute, single-line, not `/`, existing,
/// realpath-canonical. A relay-derived root that violates this is refused.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectRoot(pub String);

/// F11 — the caller-supplied idempotency key, unique within
/// `(caller key, projectRoot)`; keys are retained forever.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IdempotencyKey(pub String);

/// F17 — a follow-up key, unique within its Run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageKey(pub String);

/// F18 — a mailbox event's stable deduplication key, e.g. `run:<id>:settled`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DedupKey(pub String);

/// F9/F16 — the delivery id the provenance envelope carries so transcript
/// evidence can resolve an `unconfirmed` prompt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeliveryId(pub String);

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

/// F2 — a child's captured identity parts: the first four are captured at the
/// acknowledged `agent.start` and suffice before the first prompt; the pane id
/// is only a current locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildIdentity {
    /// The Herdr incarnation the child was observed under.
    pub herdr_incarnation: HerdrIncarnation,
    /// The stable terminal identity.
    pub terminal_id: TerminalId,
    /// The child's harness kind (catalog data, never a literal).
    pub agent_kind: AgentKind,
    /// The minted `gov-<runId[0..8]>` name.
    pub agent_name: AgentName,
    /// Once Herdr reports it (F2); carries the session re-proof (F28).
    pub native_session: Option<NativeSession>,
    /// Current locator only — a move is followed, never a loss (H#75).
    pub pane_id: PaneId,
}

/// F3 — the observation classes: how a target-local read of a fresh snapshot
/// resolves a captured identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObservationClass {
    /// Exactly one pane matches the identity, wherever it is now.
    Unique,
    /// The snapshot is valid and no pane matches.
    Absent,
    /// The snapshot is malformed, duplicated, unavailable, or ambiguous about
    /// the incarnation — never counts as absence, never settles a Run (H#74).
    Invalid,
}

impl ObservationClass {
    /// F3 — the spec spelling of the class.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unique => "unique",
            Self::Absent => "absent",
            Self::Invalid => "invalid",
        }
    }
}

/// Appendix C — the child status an `obs` event carries; Herdr's reported
/// `agent_status` reduced to the four spec states (a wire `unknown` is
/// `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChildStatus {
    /// The child is working.
    Working,
    /// The child is idle — starts an idle episode in `active` (F25).
    Idle,
    /// The child finished its turn — treated like `idle` (Appendix C).
    Done,
    /// The child is blocked — never prompt it (F17); asks the F23 questions.
    Blocked,
}

impl ChildStatus {
    /// Appendix C — the spec spelling of the status.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

/// F3 — one target-local read of a fresh snapshot: the class plus what the
/// located pane reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// `obs(unique)` — the identity resolves to exactly one pane.
    Unique {
        /// The child's reported status; `None` when Herdr reports none or the
        /// wire's `unknown`.
        status: Option<ChildStatus>,
        /// Where the identity is now — a move is followed (H#75).
        pane: PaneId,
        /// The native session the pane's occupant reports now (F2).
        native_session: Option<NativeSession>,
    },
    /// `obs(absent)` — the snapshot is valid and no pane matches.
    Absent,
    /// `obs(invalid)` — changes nothing in any state, except health reporting;
    /// deadlines still run (Appendix C).
    Invalid,
}

impl Observation {
    /// F3 — the class of this observation.
    #[must_use]
    pub fn class(&self) -> ObservationClass {
        match self {
            Self::Unique {
                status: _,
                pane: _,
                native_session: _,
            } => ObservationClass::Unique,
            Self::Absent => ObservationClass::Absent,
            Self::Invalid => ObservationClass::Invalid,
        }
    }
}

/// A snapshot's agent rows — the per-pane occupant view the adapter builds
/// from one fresh `session.snapshot` (`agents` joined to `panes`): locator,
/// terminal, harness kind, agent name, native session and reported status.
/// Every field is a distinct newtype, so positions cannot be transposed.
/// A pane with no agent has no row: it resolves no caller and matches no
/// child identity. The `Option` fields are `None` when Herdr reports no
/// value (a `null` field, or an `agent_status` of `unknown` for status).
type AgentRow = (
    PaneId,
    TerminalId,
    Option<AgentKind>,
    Option<AgentName>,
    Option<NativeSession>,
    Option<ChildStatus>,
);

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

/// F2 — the minted agent name `gov-<runId[0..8]>` (H#52).
#[must_use]
pub fn mint_agent_name(run: &RunId) -> AgentName {
    let mut name = String::from("gov-");
    name.extend(run.0.chars().take(8));
    AgentName(name)
}

/// F3 — classify one target-local read of a fresh snapshot against a
/// captured child identity:
///
/// - `unique` — exactly one pane matches the identity, wherever it is now;
///   a move is followed, never a loss (H#75);
/// - `absent` — the snapshot is valid and no pane matches; a new
///   `native_session` or `terminal_id` in the Run's pane means that pane is
///   someone else's;
/// - `invalid` — the snapshot is malformed, duplicated, unavailable, or
///   ambiguous about the incarnation; it never counts as absence and never
///   settles a Run (H#74).
///
/// `snapshot_incarnation` is the incarnation the read was taken under:
/// `None` is ambiguous → `invalid`. A read under a different, known
/// incarnation untrusts the bare ids — the identity re-proves by its
/// unique native session alone (F28, A4/A6), and a sessionless identity is
/// unprovable → `invalid`, never `absent`.
#[must_use]
pub fn classify(
    identity: &ChildIdentity,
    snapshot_incarnation: Option<&HerdrIncarnation>,
    agents: &[AgentRow],
) -> Observation {
    if snapshot_incarnation.is_none() {
        return Observation::Invalid;
    }
    if has_duplicate_locator(agents) {
        return Observation::Invalid;
    }
    let foreign = snapshot_incarnation != Some(&identity.herdr_incarnation);
    if foreign && identity.native_session.is_none() {
        return Observation::Invalid;
    }
    let matched = |row: &AgentRow| -> bool {
        if foreign {
            // F28 — on a foreign incarnation the unique native session
            // alone re-proves identity; a sessionless identity can match
            // nothing (unprovable, handled above).
            row.4 == identity.native_session
        } else {
            row.1 == identity.terminal_id
                && row.2.as_ref() == Some(&identity.agent_kind)
                && row.3.as_ref() == Some(&identity.agent_name)
                && match &identity.native_session {
                    Some(session) => row.4.as_ref() == Some(session),
                    None => true,
                }
        }
    };
    let mut hits = agents.iter().filter(|row| matched(row));
    match hits.next() {
        None => Observation::Absent,
        Some(row) => match hits.next() {
            Some(_) => Observation::Invalid,
            None => Observation::Unique {
                status: row.5,
                pane: row.0.clone(),
                native_session: row.4.clone(),
            },
        },
    }
}

/// F3 — a snapshot that lists the same pane locator twice is malformed:
/// locators are unique keys, so a duplicate is `invalid`, never a match.
fn has_duplicate_locator(agents: &[AgentRow]) -> bool {
    agents
        .iter()
        .any(|row| agents.iter().filter(|other| other.0 == row.0).count() > 1)
}

/// F4 — every Run and mailbox operation requires the caller to be the
/// current owner: `observe`, `message`, `ack` and `cancel` refuse
/// `NOT_OWNER` otherwise.
pub fn require_owner(run: &Run, caller: &CallerKey) -> Result<(), Refusal> {
    if run.owner == *caller {
        Ok(())
    } else {
        Err(Refusal::NotOwner)
    }
}

/// F4/F6 — plan `handover {runIds, successorPaneId}`: the requester must be
/// the current owner (which makes it live — it just resolved through F1),
/// the successor pane is verified by the F1 caller-resolution rules, and a
/// Run is never handed to its own child — refused `CALLER_IS_RUN` (H#24).
/// The `ChangeOwner` write is conditional on the expected owner and bumps
/// `owner_generation` atomically (Appendix B).
pub fn plan_handover(
    run: &Run,
    caller: &CallerKey,
    successor_pane: &PaneId,
    agents: &[AgentRow],
) -> Result<Transition, Refusal> {
    require_owner(run, caller)?;
    let successor = resolve_caller_key(successor_pane, agents)?;
    if run_child_key(run).as_ref() == Some(&successor) {
        return Err(Refusal::CallerIsRun);
    }
    Ok(owner_transition(run, successor))
}

/// F19 — plan `adopt {runIds}`: a fresh snapshot must show the previous
/// owner's native session gone — still present is refused
/// `ADOPT_OWNER_LIVE`; an unsettled Run is adoptable; a settled Run is
/// adoptable only for its unread events or a pending recovery, and
/// adoption never reopens it — the write changes `owner_caller_id` and
/// `owner_generation` only. A Run is never adopted by its own child —
/// refused `CALLER_IS_RUN` (F4/H#24).
pub fn plan_adoption(
    run: &Run,
    adopter: &CallerKey,
    agents: &[AgentRow],
    has_unread_events: bool,
    has_pending_recovery: bool,
) -> Result<Transition, Refusal> {
    if owner_session_present(run, agents) {
        return Err(Refusal::AdoptOwnerLive);
    }
    if run_child_key(run).as_ref() == Some(adopter) {
        return Err(Refusal::CallerIsRun);
    }
    if run.settlement.is_some() && !has_unread_events && !has_pending_recovery {
        return Err(Refusal::NotOwner);
    }
    Ok(owner_transition(run, adopter.clone()))
}

/// H#24 — the caller key the Run's own child would resolve to, once its
/// captured identity holds a native session. A handover or adoption naming
/// it would make the Run its own caller — refused `CALLER_IS_RUN`.
fn run_child_key(run: &Run) -> Option<CallerKey> {
    let identity = run.identity.as_ref()?;
    Some(CallerKey {
        agent_kind: identity.agent_kind.clone(),
        native_session: identity.native_session.clone()?,
    })
}

/// F19 — whether a fresh snapshot still shows the Run owner's native
/// session; adoption requires it gone.
fn owner_session_present(run: &Run, agents: &[AgentRow]) -> bool {
    agents
        .iter()
        .any(|row| row.4.as_ref() == Some(&run.owner.native_session))
}

/// Appendix B — the `handover`/`adopt` transaction: `owner_caller_id` and
/// `owner_generation + 1`, conditional on the expected owner.
fn owner_transition(run: &Run, owner: CallerKey) -> Transition {
    Transition {
        state_changes: Vec::from([StateChange::ChangeOwner(OwnerChange {
            run: run.id.clone(),
            expected_owner: run.owner.clone(),
            owner,
        })]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
