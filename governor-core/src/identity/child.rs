//! F2 — the child's captured identity and the minted `gov-<runId[0..8]>`
//! agent name (H#52).

use alloc::string::String;

use super::{AgentKind, AgentName, HerdrIncarnation, NativeSession, PaneId, RunId, TerminalId};

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

/// F2 — the minted agent name `gov-<runId[0..8]>` (H#52).
#[must_use]
pub fn mint_agent_name(run: &RunId) -> AgentName {
    let mut name = String::from("gov-");
    name.extend(run.0.chars().take(8));
    AgentName(name)
}
