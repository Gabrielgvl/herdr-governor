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
/// `paneId` must be present and `projectRoot` absolute, single-line, not
/// `/`, and canonical as given. An invalid envelope is refused
/// `CALLER_IDENTITY_MISSING`: it can establish no caller, and a
/// relay-derived root that fails validation is refused, never re-anchored
/// (H#3).
pub fn validate_caller_envelope(envelope: &CallerEnvelope) -> Result<(), Refusal> {
    if envelope.pane_id.0.is_empty() {
        return Err(Refusal::CallerIdentityMissing);
    }
    if !project_root_canonical(&envelope.project_root.0) {
        return Err(Refusal::CallerIdentityMissing);
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
/// Run is never handed to its own child (H#24). The `ChangeOwner` write is
/// conditional on the expected owner and bumps `owner_generation`
/// atomically (Appendix B).
pub fn plan_handover(
    run: &Run,
    caller: &CallerKey,
    successor_pane: &PaneId,
    agents: &[AgentRow],
) -> Result<Transition, Refusal> {
    require_owner(run, caller)?;
    let successor = resolve_caller_key(successor_pane, agents)?;
    if run_child_key(run).as_ref() == Some(&successor) {
        return Err(Refusal::NotOwner);
    }
    Ok(owner_transition(run, successor))
}

/// F19 — plan `adopt {runIds}`: a fresh snapshot must show the previous
/// owner's native session gone; an unsettled Run is adoptable; a settled
/// Run is adoptable only for its unread events or a pending recovery, and
/// adoption never reopens it — the write changes `owner_caller_id` and
/// `owner_generation` only. A Run is never adopted by its own child
/// (F4/H#24).
pub fn plan_adoption(
    run: &Run,
    adopter: &CallerKey,
    agents: &[AgentRow],
    has_unread_events: bool,
    has_pending_recovery: bool,
) -> Result<Transition, Refusal> {
    if owner_session_present(run, agents) {
        return Err(Refusal::NotOwner);
    }
    if run_child_key(run).as_ref() == Some(adopter) {
        return Err(Refusal::NotOwner);
    }
    if run.settlement.is_some() && !has_unread_events && !has_pending_recovery {
        return Err(Refusal::NotOwner);
    }
    Ok(owner_transition(run, adopter.clone()))
}

/// H#24 — the caller key the Run's own child would resolve to, once its
/// captured identity holds a native session. A handover or adoption naming
/// it would make the Run its own caller.
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
mod tests {
    //! Requirement-named tests for the pure identity rules (F1–F4, F19).
    //! Every input is a constructed value — snapshots, bindings, times —
    //! never a live system.

    use alloc::string::String;
    use alloc::vec::Vec;

    use super::{
        AgentKind, AgentName, AgentRow, CallerBinding, CallerEnvelope, CallerKey, ChildIdentity,
        ChildStatus, HerdrIncarnation, LaunchId, NativeSession, Observation, ObservationClass,
        PaneId, ProjectRoot, RelayInstanceId, RunId, TerminalId, Timestamp, classify,
        mint_agent_name, plan_adoption, plan_handover, require_owner, resolve_caller,
        resolve_caller_key, validate_caller_envelope,
    };
    use crate::lifecycle::{OwnerChange, Run, Settlement, State, StateChange, Transition};
    use crate::task::Refusal;

    // ---- constructors ---------------------------------------------------

    fn key(kind: &str, session: &str) -> CallerKey {
        CallerKey {
            agent_kind: AgentKind(kind.into()),
            native_session: NativeSession(session.into()),
        }
    }

    fn envelope(pane: &str, root: &str, relay: &str) -> CallerEnvelope {
        CallerEnvelope {
            pane_id: PaneId(pane.into()),
            project_root: ProjectRoot(root.into()),
            relay_instance_id: RelayInstanceId(relay.into()),
        }
    }

    fn agent_row(
        pane: &str,
        terminal: &str,
        kind: Option<&str>,
        name: Option<&str>,
        session: Option<&str>,
        status: Option<ChildStatus>,
    ) -> AgentRow {
        (
            PaneId(pane.into()),
            TerminalId(terminal.into()),
            kind.map(|k| AgentKind(k.into())),
            name.map(|n| AgentName(n.into())),
            session.map(|s| NativeSession(s.into())),
            status,
        )
    }

    fn occupied(
        pane: &str,
        terminal: &str,
        kind: &str,
        name: &str,
        session: Option<&str>,
        status: Option<ChildStatus>,
    ) -> AgentRow {
        agent_row(pane, terminal, Some(kind), Some(name), session, status)
    }

    fn child(pane: &str, session: Option<&str>) -> ChildIdentity {
        ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: AgentKind("kind-1".into()),
            agent_name: AgentName("gov-deadbeef".into()),
            native_session: session.map(|s| NativeSession(s.into())),
            pane_id: PaneId(pane.into()),
        }
    }

    fn run(
        owner: &CallerKey,
        identity: Option<ChildIdentity>,
        settlement: Option<Settlement>,
    ) -> Run {
        Run {
            id: RunId("018f3c2a-7b1d-7e90-8abc-0123456789ab".into()),
            launch: LaunchId("launch-1".into()),
            owner: owner.clone(),
            owner_generation: 0,
            version: 0,
            state: State::Active,
            prompt_certainty: None,
            child_name: String::from("gov-deadbeef"),
            identity,
            operating_point: None,
            provider: None,
            tier_start: None,
            cwd: String::from("/work"),
            base_commit: None,
            work_generation: 0,
            evidence_generation: 0,
            child_status: None,
            idle_since: None,
            idle_deadline: None,
            repair_deadline: None,
            judgment_deadline: None,
            max_age_deadline: Timestamp(1_800_000_000_000),
            nudge_episode: 0,
            nudged_episode: None,
            settlement,
            settled_at: settlement.map(|_| Timestamp(1_700_000_000_000)),
        }
    }

    fn owner_change(run: &Run, owner: CallerKey) -> Transition {
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

    // ---- F1 — caller envelope validation ---------------------------------

    #[test]
    fn f1_envelope_requires_pane_id() {
        let no_pane = envelope("", "/work/repo", "relay-1");
        assert_eq!(
            validate_caller_envelope(&no_pane),
            Err(Refusal::CallerIdentityMissing),
            "F1 — an empty paneId cannot resolve any caller"
        );
        let pane = envelope("w6:p1", "/work/repo", "relay-1");
        assert_eq!(
            validate_caller_envelope(&pane),
            Ok(()),
            "F1 — a present paneId passes"
        );
    }

    #[test]
    fn f1_project_root_absolute_single_line_canonical() {
        let pane = |root: &str| envelope("w6:p1", root, "relay-1");
        for (root, ok) in [
            ("/work/repo", true),
            ("/w", true),
            ("/work/repo/sub", true),
            ("work/repo", false), // relative
            ("", false),          // empty
            ("/", false),         // root itself is refused (F1)
            ("//work", false),    // doubled separator is not canonical
            ("/work//repo", false),
            ("/work/", false),       // trailing separator
            ("/work/./repo", false), // dot component
            ("/work/../repo", false),
            ("/work\n/repo", false), // line break — not single-line
            ("/work\r\n/repo", false),
            ("/work/\0repo", false), // NUL — never a realpath output
        ] {
            assert_eq!(
                validate_caller_envelope(&pane(root)).is_ok(),
                ok,
                "F1 — projectRoot {root:?} canonical-as-given verdict"
            );
        }
    }

    #[test]
    fn f1_resolve_missing_duplicate_sessionless() {
        let agents: Vec<AgentRow> = Vec::from([
            occupied("w6:p1", "t1", "kind-a", "caller", Some("sess-a"), None),
            occupied("w6:p2", "t2", "kind-a", "caller", None, None),
            occupied("w6:p3", "t3", "kind-b", "caller", Some("sess-b"), None),
            occupied("w6:p3", "t3", "kind-b", "caller", Some("sess-b"), None),
            agent_row("w6:p4", "t4", None, None, None, None),
        ]);
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p9".into()), &agents),
            Err(Refusal::CallerIdentityMissing),
            "F1 — no pane for the envelope is CALLER_IDENTITY_MISSING"
        );
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p3".into()), &agents),
            Err(Refusal::CallerIdentityDuplicate),
            "F1 — two panes for the id is CALLER_IDENTITY_DUPLICATE"
        );
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p2".into()), &agents),
            Err(Refusal::CallerIdentitySessionless),
            "F1 — an occupant without a native session is CALLER_IDENTITY_SESSIONLESS"
        );
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p4".into()), &agents),
            Err(Refusal::CallerIdentityMissing),
            "F1 — an empty pane resolves no caller"
        );
        let session_no_kind =
            Vec::from([agent_row("w6:p5", "t5", None, None, Some("sess-x"), None)]);
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p5".into()), &session_no_kind),
            Err(Refusal::CallerIdentityMissing),
            "F1 — a session without an agent kind keys no caller"
        );
        assert_eq!(
            resolve_caller_key(&PaneId("w6:p1".into()), &agents),
            Ok(key("kind-a", "sess-a")),
            "F1 — a unique occupied pane resolves its caller key"
        );
    }

    #[test]
    fn f1_unbound_relay_binds_on_first_use() {
        let agents = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-a"),
            None,
        )]);
        let env = envelope("w6:p1", "/work/repo", "relay-1");
        assert_eq!(
            resolve_caller(&env, None, &agents),
            Ok((
                key("kind-a", "sess-a"),
                Some(CallerBinding {
                    caller: key("kind-a", "sess-a"),
                    relay_instance: RelayInstanceId("relay-1".into()),
                    pane_at_bind: PaneId("w6:p1".into()),
                })
            )),
            "F1 — the first request resolves the caller and yields the binding to persist"
        );
    }

    #[test]
    fn f1_bound_relay_reresolves_to_same_session() {
        let agents = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-a"),
            None,
        )]);
        let bound = CallerBinding {
            caller: key("kind-a", "sess-a"),
            relay_instance: RelayInstanceId("relay-1".into()),
            pane_at_bind: PaneId("w6:p1".into()),
        };
        let env = envelope("w6:p1", "/work/repo", "relay-1");
        assert_eq!(
            resolve_caller(&env, Some(&bound), &agents),
            Ok((key("kind-a", "sess-a"), None)),
            "F1 — a bound id that re-resolves to the same caller produces no new binding"
        );
    }

    #[test]
    fn f1_bound_relay_refuses_session_drift() {
        // a4_native_new_replaces_session — same pane, replaced occupant.
        let agents = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-new"),
            None,
        )]);
        let bound = CallerBinding {
            caller: key("kind-a", "sess-a"),
            relay_instance: RelayInstanceId("relay-1".into()),
            pane_at_bind: PaneId("w6:p1".into()),
        };
        let env = envelope("w6:p1", "/work/repo", "relay-1");
        assert_eq!(
            resolve_caller(&env, Some(&bound), &agents),
            Err(Refusal::CallerIdentityMismatch),
            "F1 — re-resolving to a different native session is CALLER_IDENTITY_MISMATCH"
        );
        let kind_drift = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-b",
            "caller",
            Some("sess-a"),
            None,
        )]);
        assert_eq!(
            resolve_caller(&env, Some(&bound), &kind_drift),
            Err(Refusal::CallerIdentityMismatch),
            "F1 — a different caller key is drift even with the same session"
        );
        let wrong_binding = CallerBinding {
            caller: key("kind-a", "sess-a"),
            relay_instance: RelayInstanceId("relay-other".into()),
            pane_at_bind: PaneId("w6:p1".into()),
        };
        assert_eq!(
            resolve_caller(&env, Some(&wrong_binding), &agents),
            Err(Refusal::CallerIdentityMismatch),
            "F1 — a binding for another relay id cannot verify this request"
        );
        let gone: Vec<AgentRow> = Vec::new();
        assert_eq!(
            resolve_caller(&env, Some(&bound), &gone),
            Err(Refusal::CallerIdentityMissing),
            "F1 — a bound id whose locator now resolves to nothing is missing"
        );
    }

    #[test]
    fn f1_respawned_relay_binds_afresh() {
        let agents = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-a"),
            None,
        )]);
        let old = CallerBinding {
            caller: key("kind-a", "sess-a"),
            relay_instance: RelayInstanceId("relay-1".into()),
            pane_at_bind: PaneId("w6:p1".into()),
        };
        let env = envelope("w6:p1", "/work/repo", "relay-2");
        // ADR-0004 — a respawned relay mints a new id; the daemon looks it
        // up, finds no binding (None), and binds afresh.
        match resolve_caller(&env, None, &agents) {
            Ok((caller, Some(binding))) => {
                assert_eq!(caller, old.caller, "F1 — same caller key rebinds");
                assert_eq!(
                    binding.relay_instance,
                    RelayInstanceId("relay-2".into()),
                    "F1 — the new binding carries the new relay id"
                );
            }
            other => panic!("F1 — a fresh relay id must bind, got {other:?}"),
        }
    }

    #[test]
    fn f1_resolution_refuses_invalid_envelope() {
        let agents = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-a"),
            None,
        )]);
        let env = envelope("w6:p1", "relative/path", "relay-1");
        assert_eq!(
            resolve_caller(&env, None, &agents),
            Err(Refusal::CallerIdentityMissing),
            "F1 — resolution validates the envelope first"
        );
    }

    // ---- F2 — child identity ----------------------------------------------

    #[test]
    fn f2_agent_name_mints_gov_run_prefix() {
        let name = mint_agent_name(&RunId("018f3c2a-7b1d-7e90-8abc-0123456789ab".into()));
        assert_eq!(
            name,
            AgentName("gov-018f3c2a".into()),
            "F2 — the name is gov-<runId[0..8]> (H#52)"
        );
        let short = mint_agent_name(&RunId("r1".into()));
        assert_eq!(
            short,
            AgentName("gov-r1".into()),
            "F2 — the mint takes at most eight characters, never panics"
        );
    }

    #[test]
    fn f2_first_four_parts_suffice_before_prompt() {
        // F2 — before the first prompt, incarnation/terminal/kind/name are
        // enough: a sessionless identity still matches its pane.
        let identity = child("w6:p9", None);
        let agents = Vec::from([occupied(
            "w6:p3",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            None,
            Some(ChildStatus::Working),
        )]);
        let inc = HerdrIncarnation("inc-1".into());
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Unique {
                status: Some(ChildStatus::Working),
                pane: PaneId("w6:p3".into()),
                native_session: None,
            },
            "F2/F3 — four parts match before any session is reported"
        );
    }

    // ---- F3 — observation classes -----------------------------------------

    #[test]
    fn f3_observation_class_spellings() {
        let cases = [
            (ObservationClass::Unique, "unique"),
            (ObservationClass::Absent, "absent"),
            (ObservationClass::Invalid, "invalid"),
        ];
        for (class, name) in cases {
            assert_eq!(
                class.as_str(),
                name,
                "observation class spelling must match F3"
            );
        }
    }

    #[test]
    fn appendix_c_child_status_spellings() {
        let cases = [
            (ChildStatus::Working, "working"),
            (ChildStatus::Idle, "idle"),
            (ChildStatus::Done, "done"),
            (ChildStatus::Blocked, "blocked"),
        ];
        for (status, name) in cases {
            assert_eq!(
                status.as_str(),
                name,
                "child status spelling must match Appendix C"
            );
        }
    }

    #[test]
    fn f3_observation_maps_to_its_class() {
        let unique = Observation::Unique {
            status: Some(ChildStatus::Working),
            pane: PaneId("w6:p1".into()),
            native_session: Some(NativeSession("sess".into())),
        };
        assert_eq!(unique.class(), ObservationClass::Unique);
        assert_eq!(Observation::Absent.class(), ObservationClass::Absent);
        assert_eq!(Observation::Invalid.class(), ObservationClass::Invalid);
    }

    #[test]
    fn f3_unique_when_exactly_one_pane_matches() {
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                Some(ChildStatus::Done),
            ),
            occupied(
                "w6:p4",
                "term-9",
                "kind-2",
                "other-agent",
                Some("sess-9"),
                None,
            ),
        ]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Unique {
                status: Some(ChildStatus::Done),
                pane: PaneId("w6:p9".into()),
                native_session: Some(NativeSession("sess-1".into())),
            },
            "F3 — a full identity match is unique, wherever the pane sits"
        );
    }

    #[test]
    fn f3_move_is_followed_never_a_loss() {
        // a4_workspace_move_new_locator — a workspace move re-keys the
        // pane_id; terminal, session, kind and name survive.
        let identity = child("w1:p3", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([occupied(
            "w2:p1",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            Some(ChildStatus::Working),
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Unique {
                status: Some(ChildStatus::Working),
                pane: PaneId("w2:p1".into()),
                native_session: Some(NativeSession("sess-1".into())),
            },
            "F3 — a moved child is unique at its new locator (H#75)"
        );
    }

    #[test]
    fn f3_absent_when_no_pane_matches() {
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        for agents in [
            Vec::new(),
            Vec::from([occupied(
                "w6:p4",
                "term-9",
                "kind-2",
                "other",
                Some("sess-9"),
                None,
            )]),
        ] {
            assert_eq!(
                classify(&identity, Some(&inc), &agents),
                Observation::Absent,
                "F3 — a valid snapshot with no matching pane is absent"
            );
        }
    }

    #[test]
    fn f3_new_session_or_terminal_in_the_pane_is_absent() {
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        // a4_native_new_replaces_session — same pane/terminal/name, new
        // session.
        let new_session = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-2"),
            None,
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &new_session),
            Observation::Absent,
            "F3 — a new native_session in the Run's pane is someone else's pane"
        );
        // a4_pane_replacement_fields — a recreated pane keeps nothing
        // stable.
        let new_terminal = Vec::from([occupied(
            "w6:p9",
            "term-9",
            "kind-1",
            "gov-deadbeef",
            Some("sess-9"),
            None,
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &new_terminal),
            Observation::Absent,
            "F3 — a new terminal_id in the Run's pane is someone else's pane"
        );
    }

    #[test]
    fn f3_dropped_session_means_absent() {
        // a4_native_replace_new_session — the occupant's session vanished.
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            None,
            Some(ChildStatus::Idle),
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Absent,
            "F3 — a pane that no longer reports the captured session is not the Run"
        );
    }

    #[test]
    fn f3_sessionless_identity_ignores_reported_session() {
        // F2 — before the session is captured the four parts match, and
        // the Unique carries the session Herdr now reports for capture.
        let identity = child("w6:p9", None);
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Unique {
                status: None,
                pane: PaneId("w6:p9".into()),
                native_session: Some(NativeSession("sess-1".into())),
            },
            "F3 — a pre-capture identity matches and reports the new session"
        );
    }

    #[test]
    fn f3_every_identity_field_is_required() {
        // One differing field at a time — no single part of the captured
        // identity may be dropped from the match.
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        for (label, row) in [
            (
                "terminal",
                occupied(
                    "w6:p9",
                    "term-9",
                    "kind-1",
                    "gov-deadbeef",
                    Some("sess-1"),
                    None,
                ),
            ),
            (
                "kind",
                occupied(
                    "w6:p9",
                    "term-1",
                    "kind-9",
                    "gov-deadbeef",
                    Some("sess-1"),
                    None,
                ),
            ),
            (
                "name",
                occupied(
                    "w6:p9",
                    "term-1",
                    "kind-1",
                    "gov-ffffffff",
                    Some("sess-1"),
                    None,
                ),
            ),
            (
                "session",
                occupied(
                    "w6:p9",
                    "term-1",
                    "kind-1",
                    "gov-deadbeef",
                    Some("sess-9"),
                    None,
                ),
            ),
            (
                "missing kind field",
                agent_row(
                    "w6:p9",
                    "term-1",
                    None,
                    Some("gov-deadbeef"),
                    Some("sess-1"),
                    None,
                ),
            ),
            (
                "missing name field",
                agent_row(
                    "w6:p9",
                    "term-1",
                    Some("kind-1"),
                    None,
                    Some("sess-1"),
                    None,
                ),
            ),
        ] {
            let agents = Vec::from([row]);
            assert_eq!(
                classify(&identity, Some(&inc), &agents),
                Observation::Absent,
                "F3 — a pane differing only in {label} is not the Run"
            );
        }
    }

    #[test]
    fn f3_ambiguous_incarnation_is_invalid() {
        let identity = child("w6:p9", Some("sess-1"));
        let agents = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        )]);
        assert_eq!(
            classify(&identity, None, &agents),
            Observation::Invalid,
            "F3 — a read ambiguous about the incarnation is invalid"
        );
    }

    #[test]
    fn f3_duplicate_locator_is_invalid() {
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
            occupied("w6:p9", "term-7", "kind-7", "other", Some("sess-7"), None),
        ]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Invalid,
            "F3 — a snapshot listing one locator twice is malformed"
        );
    }

    #[test]
    fn f3_duplicate_identity_match_is_invalid() {
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-1".into());
        let agents = Vec::from([
            occupied(
                "w6:p2",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
            occupied(
                "w6:p5",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
        ]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Invalid,
            "F3 — two panes matching one identity is ambiguous, never unique"
        );
    }

    #[test]
    fn f3_foreign_incarnation_reproves_by_native_session() {
        // F28/A4 — after a discontinuity the unique session re-proves
        // identity; the bare ids are untrusted.
        let identity = child("w6:p9", Some("sess-1"));
        let inc = HerdrIncarnation("inc-2".into());
        let agents = Vec::from([
            occupied(
                "w9:p2",
                "term-9",
                "kind-2",
                "gov-deadbeef",
                Some("sess-1"),
                Some(ChildStatus::Idle),
            ),
            occupied("w6:p9", "term-1", "kind-1", "gov-deadbeef", None, None),
        ]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Unique {
                status: Some(ChildStatus::Idle),
                pane: PaneId("w9:p2".into()),
                native_session: Some(NativeSession("sess-1".into())),
            },
            "F3 — the session re-proves identity across an incarnation change"
        );
        assert_eq!(
            classify(&identity, Some(&inc), &agents[1..]),
            Observation::Absent,
            "F3 — without the session the Run is absent under the new incarnation"
        );
    }

    #[test]
    fn f3_foreign_incarnation_sessionless_is_invalid_never_absent() {
        // F28 — a Run without a native session cannot re-prove itself; the
        // observation is ambiguous (invalid), never absence.
        let identity = child("w6:p9", None);
        let inc = HerdrIncarnation("inc-2".into());
        let agents = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            None,
            None,
        )]);
        assert_eq!(
            classify(&identity, Some(&inc), &agents),
            Observation::Invalid,
            "F3 — a sessionless identity under a foreign incarnation is unprovable"
        );
    }

    #[test]
    fn f3_invalid_never_counts_as_absent() {
        let inc = HerdrIncarnation("inc-1".into());
        let other = HerdrIncarnation("inc-2".into());
        let sessionful = child("w6:p9", Some("sess-1"));
        let sessionless = child("w6:p9", None);
        let good = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-1"),
            None,
        )]);
        let dup_locator = Vec::from([
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
            occupied("w6:p9", "term-2", "kind-2", "other", None, None),
        ]);
        let dup_match = Vec::from([
            occupied(
                "w6:p1",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
            occupied(
                "w6:p2",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-1"),
                None,
            ),
        ]);
        let cases: Vec<(ChildIdentity, Option<&HerdrIncarnation>, Vec<AgentRow>)> = Vec::from([
            (sessionful.clone(), None, good.clone()),
            (sessionful.clone(), Some(&inc), dup_locator),
            (sessionful.clone(), Some(&inc), dup_match),
            (sessionless.clone(), Some(&other), Vec::new()),
        ]);
        for (identity, incarnation, agents) in cases {
            let observation = classify(&identity, incarnation, &agents);
            assert_eq!(
                observation.class(),
                ObservationClass::Invalid,
                "F3 — invalid never counts as absence (H#74)"
            );
        }
    }

    // ---- F4 — ownership ----------------------------------------------------

    #[test]
    fn f4_observe_message_ack_cancel_refuse_not_owner() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, None);
        assert_eq!(
            require_owner(&run, &owner),
            Ok(()),
            "F4 — the current owner passes the check"
        );
        assert_eq!(
            require_owner(&run, &key("kind-a", "sess-b")),
            Err(Refusal::NotOwner),
            "F4 — any other caller is NOT_OWNER"
        );
    }

    #[test]
    fn f4_handover_changes_owner_conditionally() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, None);
        let agents = Vec::from([occupied(
            "w6:p7",
            "t7",
            "kind-b",
            "successor",
            Some("sess-b"),
            None,
        )]);
        assert_eq!(
            plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
            Ok(owner_change(&run, key("kind-b", "sess-b"))),
            "F4 — handover writes the conditional owner change (owner_generation bumps)"
        );
    }

    #[test]
    fn f4_handover_requires_the_current_owner() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, None);
        let agents = Vec::from([occupied(
            "w6:p7",
            "t7",
            "kind-b",
            "successor",
            Some("sess-b"),
            None,
        )]);
        assert_eq!(
            plan_handover(
                &run,
                &key("kind-a", "sess-x"),
                &PaneId("w6:p7".into()),
                &agents
            ),
            Err(Refusal::NotOwner),
            "F4 — a non-owner cannot hand the Run over"
        );
    }

    #[test]
    fn f4_handover_verifies_successor_by_f1() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, None);
        let agents = Vec::from([occupied("w6:p7", "t7", "kind-b", "successor", None, None)]);
        assert_eq!(
            plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
            Err(Refusal::CallerIdentitySessionless),
            "F4/F6 — a sessionless successor pane is refused by the F1 rules"
        );
        assert_eq!(
            plan_handover(&run, &owner, &PaneId("w6:p0".into()), &agents),
            Err(Refusal::CallerIdentityMissing),
            "F4/F6 — a successor pane that resolves to nothing is refused"
        );
    }

    #[test]
    fn f4_run_is_never_its_own_caller() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, Some(child("w6:p9", Some("sess-child"))), None);
        let agents = Vec::from([
            occupied(
                "w6:p9",
                "term-1",
                "kind-1",
                "gov-deadbeef",
                Some("sess-child"),
                None,
            ),
            occupied("w6:p7", "t7", "kind-b", "successor", Some("sess-b"), None),
        ]);
        assert_eq!(
            plan_handover(&run, &owner, &PaneId("w6:p9".into()), &agents),
            Err(Refusal::NotOwner),
            "F4/H#24 — a handover to the Run's own child is refused"
        );
        // A child key that differs only in kind is still that child's
        // caller.
        let agents_kind = Vec::from([occupied(
            "w6:p9",
            "term-1",
            "kind-1",
            "gov-deadbeef",
            Some("sess-child"),
            None,
        )]);
        assert_eq!(
            plan_adoption(
                &run,
                &key("kind-1", "sess-child"),
                &agents_kind,
                false,
                false
            ),
            Err(Refusal::NotOwner),
            "F4/H#24 — the Run can never become its own caller via adopt either"
        );
    }

    #[test]
    fn f4_child_without_session_cannot_be_self_named() {
        // With no captured session there is no child caller key to
        // forbid — handover proceeds on the verified successor.
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, Some(child("w6:p9", None)), None);
        let agents = Vec::from([occupied(
            "w6:p7",
            "t7",
            "kind-b",
            "successor",
            Some("sess-b"),
            None,
        )]);
        assert_eq!(
            plan_handover(&run, &owner, &PaneId("w6:p7".into()), &agents),
            Ok(owner_change(&run, key("kind-b", "sess-b"))),
            "F4 — a sessionless child has no caller key to collide with"
        );
    }

    // ---- F19 — adoption ----------------------------------------------------

    #[test]
    fn f19_adopt_requires_previous_owner_session_gone() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, None);
        let live_owner = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-a"),
            None,
        )]);
        let adopter = key("kind-b", "sess-b");
        assert_eq!(
            plan_adoption(&run, &adopter, &live_owner, false, false),
            Err(Refusal::NotOwner),
            "F19 — a live previous owner keeps the Run (use handover)"
        );
        let gone_owner = Vec::from([occupied(
            "w6:p1",
            "t1",
            "kind-a",
            "caller",
            Some("sess-z"),
            None,
        )]);
        assert_eq!(
            plan_adoption(&run, &adopter, &gone_owner, false, false),
            Ok(owner_change(&run, adopter)),
            "F19 — the owner's session gone makes an unsettled Run adoptable"
        );
    }

    #[test]
    fn f19_adopt_unsettled_run_writes_owner_change() {
        let owner = key("kind-a", "sess-a");
        let mut run = run(&owner, None, None);
        run.owner_generation = 3;
        let adopter = key("kind-b", "sess-b");
        let agents: Vec<AgentRow> = Vec::new();
        let transition = plan_adoption(&run, &adopter, &agents, false, false);
        let expected = owner_change(&run, adopter);
        assert_eq!(
            transition,
            Ok(expected),
            "F19 — adoption writes the conditional owner change"
        );
        if let Ok(t) = transition {
            assert_eq!(
                t.state_changes.len(),
                1,
                "F19 — one write: the owner change"
            );
            assert_eq!(
                t.events.len(),
                0,
                "F19 — adoption emits no mailbox event by itself"
            );
        }
    }

    #[test]
    fn f19_settled_run_adoptable_only_for_unread_or_recovery() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, Some(Settlement::NoHandoff));
        let adopter = key("kind-b", "sess-b");
        let agents: Vec<AgentRow> = Vec::new();
        assert_eq!(
            plan_adoption(&run, &adopter, &agents, false, false),
            Err(Refusal::NotOwner),
            "F19 — a settled Run with nothing pending is not adoptable"
        );
        assert_eq!(
            plan_adoption(&run, &adopter, &agents, true, false),
            Ok(owner_change(&run, adopter.clone())),
            "F19 — a settled Run is adopted for its unread events"
        );
        assert_eq!(
            plan_adoption(&run, &adopter, &agents, false, true),
            Ok(owner_change(&run, adopter.clone())),
            "F19 — a settled Run is adopted for a pending recovery"
        );
        assert_eq!(
            plan_adoption(&run, &adopter, &agents, true, true),
            Ok(owner_change(&run, adopter)),
            "F19 — either obligation suffices"
        );
    }

    #[test]
    fn f19_adoption_never_reopens_a_settled_run() {
        let owner = key("kind-a", "sess-a");
        let run = run(&owner, None, Some(Settlement::Rejected));
        let adopter = key("kind-b", "sess-b");
        let agents: Vec<AgentRow> = Vec::new();
        let transition = plan_adoption(&run, &adopter, &agents, true, false);
        match transition {
            Ok(t) => {
                assert_eq!(
                    t.state_changes.len(),
                    1,
                    "F19 — the only write is the owner change"
                );
                for change in &t.state_changes {
                    match change {
                        StateChange::ChangeOwner(oc) => {
                            assert_eq!(
                                oc.expected_owner, run.owner,
                                "F19 — conditional on the previous owner"
                            );
                        }
                        StateChange::UpdateRun(_) => panic!(
                            "F19 — adoption must not rewrite the Run record (settlement is immutable)"
                        ),
                        StateChange::BindCaller(_)
                        | StateChange::RecordLaunch(_)
                        | StateChange::ReserveRun(_)
                        | StateChange::WriteEffect(_)
                        | StateChange::RecordFollowUp(_)
                        | StateChange::ExpireFollowUps { .. }
                        | StateChange::RecordRecovery(_)
                        | StateChange::SetCooldown(_)
                        | StateChange::FreezeHandoff(_)
                        | StateChange::AckEvent(_) => {
                            panic!("F19 — adoption writes the owner change only")
                        }
                    }
                }
            }
            Err(refusal) => panic!("F19 — adoptable run refused: {refusal:?}"),
        }
    }
}
