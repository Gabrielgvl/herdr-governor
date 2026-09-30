//! F3 — the observation classes: how a target-local read of a fresh
//! snapshot resolves a captured child identity.

use super::{AgentRow, ChildIdentity, HerdrIncarnation, NativeSession, PaneId};

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
