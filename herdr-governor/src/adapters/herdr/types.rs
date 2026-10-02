//! The wire DTOs of the confirmed protocol-22 subset: records, enums and
//! event payloads. Field names and optionality follow the pinned schema
//! fixture (`tests/fixtures/herdr-api-schema.json`, protocol 22) and the
//! contract fixtures under `tests/fixtures/contract/` — where they and
//! intuition disagree, the fixture wins. Fields the governor never
//! inspects stay opaque (`tokens`, `worktree`, `state_labels` values)
//! rather than being dropped, so a later node does not re-learn the shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The wire `agent_status` enum (`AgentStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// `idle`.
    Idle,
    /// `working`.
    Working,
    /// `blocked`.
    Blocked,
    /// `done`.
    Done,
    /// `unknown` — the wire's "no status", not a fifth real state.
    Unknown,
}

/// The wire `agent_session.kind` enum (`AgentSessionRefKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// `id` — a harness-native session id.
    Id,
    /// `path` — a transcript file path.
    Path,
}

/// The wire `agent_session` record (`AgentSessionInfo`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentSession {
    /// `agent` — the harness kind the session belongs to (opaque).
    pub agent: String,
    /// `kind` — `id` or `path`.
    pub kind: SessionKind,
    /// `source` — the reporting source (e.g. `herdr:<kind>`).
    pub source: String,
    /// `value` — the session identity the core's `NativeSession` wraps.
    pub value: String,
}

/// The wire `source` enum (`ReadSource`) — the same values travel in both
/// directions (request param, read echo, subscription arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    /// `visible` — the on-screen viewport only.
    Visible,
    /// `recent` — viewport plus scrollback, wrapped.
    Recent,
    /// `recent_unwrapped` — the same range with hard-wraps undone.
    RecentUnwrapped,
    /// `detection` — the detector's view.
    Detection,
}

/// The wire `format` enum (`ReadFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadFormat {
    /// `text` — ANSI stripped.
    Text,
    /// `ansi` — escape sequences retained.
    Ansi,
}

/// The wire `pane.split` `direction` enum (`SplitDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// `right` — a vertical split.
    Right,
    /// `down` — a horizontal split.
    Down,
}

/// The `scroll` sub-record (`PaneScrollInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PaneScrollInfo {
    /// `max_offset_from_bottom` — the total scrollback depth.
    pub max_offset_from_bottom: u64,
    /// `offset_from_bottom` — the viewport's distance from the bottom.
    pub offset_from_bottom: u64,
    /// `viewport_rows` — the visible row count.
    pub viewport_rows: u64,
}

/// A pane record (`PaneInfo`). `Option` fields cover both absent and null;
/// `#[serde(default)]` on the maps keeps empty-on-wire records decodable.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PaneInfo {
    /// `pane_id` — the locator.
    pub pane_id: String,
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `terminal_id` — the stable terminal under the pane (F2, A4).
    pub terminal_id: String,
    /// `revision` — the pane's content revision.
    pub revision: u64,
    /// `focused`.
    pub focused: bool,
    /// `agent_status` — required on the wire; `Unknown` when unset.
    pub agent_status: AgentStatus,
    /// `agent` — the occupying harness kind, when an agent is attached.
    #[serde(default)]
    pub agent: Option<String>,
    /// `agent_session` — the native session, when one exists.
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    /// `cwd` — the pane's start directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `display_agent` — the operator-facing agent label.
    #[serde(default)]
    pub display_agent: Option<String>,
    /// `foreground_cwd` — the shell's current directory.
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    /// `label` — the pane label.
    #[serde(default)]
    pub label: Option<String>,
    /// `scroll` — the viewport position, when reported.
    #[serde(default)]
    pub scroll: Option<PaneScrollInfo>,
    /// `state_labels` — detector key/values; the schema leaves values
    /// unconstrained (`object`), so they pass through uninspected.
    #[serde(default)]
    pub state_labels: BTreeMap<String, serde_json::Value>,
    /// `terminal_title` — the raw terminal title.
    #[serde(default)]
    pub terminal_title: Option<String>,
    /// `terminal_title_stripped` — the title with decoration removed.
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    /// `title` — the pane title.
    #[serde(default)]
    pub title: Option<String>,
    /// `tokens` — bookkeeping, opaque to the governor.
    #[serde(default)]
    pub tokens: Option<serde_json::Value>,
}

/// An agent record (`AgentInfo`) — the `PaneInfo` surface plus the
/// agent-only fields. Kept flat (not serde-flattened) so callers read
/// `agent.pane_id` directly; the duplication mirrors the schema.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentInfo {
    /// `pane_id` — the locator.
    pub pane_id: String,
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `terminal_id`.
    pub terminal_id: String,
    /// `revision`.
    pub revision: u64,
    /// `focused`.
    pub focused: bool,
    /// `agent_status`.
    pub agent_status: AgentStatus,
    /// `agent` — the harness kind.
    #[serde(default)]
    pub agent: Option<String>,
    /// `agent_session` — the native session. Absent until a first-run gate
    /// clears; never present for harnesses Herdr cannot see into.
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    /// `cwd`.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `display_agent`.
    #[serde(default)]
    pub display_agent: Option<String>,
    /// `foreground_cwd`.
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    /// `label`.
    #[serde(default)]
    pub label: Option<String>,
    /// `scroll`.
    #[serde(default)]
    pub scroll: Option<PaneScrollInfo>,
    /// `state_labels` — see `PaneInfo::state_labels`.
    #[serde(default)]
    pub state_labels: BTreeMap<String, serde_json::Value>,
    /// `terminal_title`.
    #[serde(default)]
    pub terminal_title: Option<String>,
    /// `terminal_title_stripped`.
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    /// `title`.
    #[serde(default)]
    pub title: Option<String>,
    /// `tokens` — opaque bookkeeping.
    #[serde(default)]
    pub tokens: Option<serde_json::Value>,
    /// `name` — the agent name; exists only on agent surfaces (A4 evidence:
    /// `name` on the agent list, `label` only on the pane surfaces).
    #[serde(default)]
    pub name: Option<String>,
    /// `interactive_ready` — absent from `agent.list` rows during the start
    /// window (the a3 registration-window evidence); the start response's
    /// flag is the authoritative return-time signal.
    #[serde(default)]
    pub interactive_ready: Option<bool>,
    /// `launch_pending`.
    #[serde(default)]
    pub launch_pending: Option<bool>,
    /// `screen_detection_skipped`.
    #[serde(default)]
    pub screen_detection_skipped: Option<bool>,
    /// `state_change_seq`.
    #[serde(default)]
    pub state_change_seq: Option<u64>,
}

/// A tab record (`TabInfo`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TabInfo {
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `number` — the tab's position.
    pub number: u32,
    /// `label`.
    pub label: String,
    /// `focused`.
    pub focused: bool,
    /// `agent_status` — the roll-up status.
    pub agent_status: AgentStatus,
    /// `pane_count`.
    pub pane_count: u32,
}

/// A workspace record (`WorkspaceInfo`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkspaceInfo {
    /// `workspace_id`.
    pub workspace_id: String,
    /// `number`.
    pub number: u32,
    /// `label`.
    pub label: String,
    /// `focused`.
    pub focused: bool,
    /// `agent_status` — the roll-up status.
    pub agent_status: AgentStatus,
    /// `active_tab_id`.
    pub active_tab_id: String,
    /// `tab_count`.
    pub tab_count: u32,
    /// `pane_count`.
    pub pane_count: u32,
    /// `tokens` — opaque bookkeeping.
    #[serde(default)]
    pub tokens: Option<serde_json::Value>,
    /// `worktree` — the worktree attachment, when present; opaque.
    #[serde(default)]
    pub worktree: Option<serde_json::Value>,
}

/// The `session.snapshot` payload (`SessionSnapshot`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionSnapshot {
    /// `version` — the server version string.
    pub version: String,
    /// `protocol` — the protocol revision (22 in the pinned schema).
    pub protocol: u32,
    /// `workspaces`.
    pub workspaces: Vec<WorkspaceInfo>,
    /// `tabs`.
    pub tabs: Vec<TabInfo>,
    /// `panes` — every pane, agents included.
    pub panes: Vec<PaneInfo>,
    /// `layouts` — the per-tab layout snapshots, opaque: the governor
    /// never consumes layout geometry (tab membership comes from
    /// `panes[*].tab_id`), so elements pass through uninspected.
    pub layouts: Vec<serde_json::Value>,
    /// `agents` — the occupied panes only.
    pub agents: Vec<AgentInfo>,
    /// `focused_workspace_id`, when one is focused.
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    /// `focused_tab_id`.
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    /// `focused_pane_id`.
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

/// An op result paired with the epoch of the connection that carried it
/// (see `conn::ConnEpoch`): reads feed F3 `classify`/`resolve_caller`,
/// which need the incarnation the read was taken under, and the epoch is
/// how the daemon derives one.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed<T> {
    /// The connection epoch this value was observed under.
    pub epoch: super::conn::ConnEpoch,
    /// The op's typed result.
    pub value: T,
}

impl<T> Observed<T> {
    /// Map the carried value, keeping the epoch.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Observed<U> {
        Observed {
            epoch: self.epoch,
            value: f(self.value),
        }
    }
}

/// A per-pane occupant row — the shape `identity::{resolve_caller,
/// classify}` consume (the daemon maps it onto the core's row tuple). A
/// pane with no agent has no row; a wire `unknown` status is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    /// `pane_id` — the pane locator.
    pub pane_id: String,
    /// `terminal_id` — the stable terminal under the pane.
    pub terminal_id: String,
    /// The occupying harness kind — opaque catalog data.
    pub agent: Option<String>,
    /// The agent's `name` — present only on agent surfaces.
    pub name: Option<String>,
    /// The native session (`agent_session.value`) — the F28 re-proof key.
    pub native_session: Option<String>,
    /// The reported status; `None` on wire `unknown`.
    pub status: Option<AgentStatus>,
}

impl SessionSnapshot {
    /// The occupant rows `identity::{resolve_caller, classify}` consume:
    /// one per `agents` record, `unknown` status to `None`, and
    /// `native_session` taken from the session record's `value`.
    #[must_use]
    pub fn agent_rows(&self) -> Vec<AgentRow> {
        self.agents
            .iter()
            .map(|a| AgentRow {
                pane_id: a.pane_id.clone(),
                terminal_id: a.terminal_id.clone(),
                agent: a.agent.clone(),
                name: a.name.clone(),
                native_session: a.agent_session.as_ref().map(|s| s.value.clone()),
                status: (a.agent_status != AgentStatus::Unknown).then_some(a.agent_status),
            })
            .collect()
    }
}

/// The `pane_read` payload (`PaneReadResult`) — also `agent.read`'s
/// result: the schema's result union has no `agent_read` variant, the read
/// of an agent's pane is a `pane_read` (fixture over intuition).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PaneRead {
    /// `pane_id`.
    pub pane_id: String,
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `revision` — the content revision the read was taken at.
    pub revision: u64,
    /// `source` — the read source the server used.
    pub source: ReadSource,
    /// `format` — `text`/`ansi`.
    pub format: ReadFormat,
    /// `text` — the pane contents.
    pub text: String,
    /// `truncated` — the read hit a bound.
    pub truncated: bool,
}

/// A typed subscription event — one variant per kind the confirmed subset
/// arms (`pane.output_matched`, `pane.scroll_changed`,
/// `pane.agent_status_changed`). An armed stream delivering anything else
/// is malformed, never dropped.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum SubEvent {
    /// `pane.output_matched` — one-shot: fires at most once per arm;
    /// re-arming while the marker is on screen fires immediately (the
    /// recorded state-triggered behavior).
    OutputMatched {
        /// The pane the marker matched on.
        pane_id: String,
        /// The line that matched.
        matched_line: String,
        /// The read the server took at match time.
        read: Box<PaneRead>,
    },
    /// `pane.scroll_changed`.
    ScrollChanged {
        /// The pane.
        pane_id: String,
        /// Its workspace.
        workspace_id: String,
        /// The new scroll position.
        scroll: PaneScrollInfo,
    },
    /// `pane.agent_status_changed`.
    AgentStatusChanged {
        /// The pane.
        pane_id: String,
        /// Its workspace.
        workspace_id: String,
        /// The new status — the wire value verbatim (`unknown` stays
        /// `Unknown`; flattening to `None` is the caller's call).
        agent_status: AgentStatus,
        /// The occupying harness kind, when reported.
        agent: Option<String>,
        /// The display label, when reported.
        display_agent: Option<String>,
        /// The title, when reported.
        title: Option<String>,
        /// Detector key/values (the event schema pins `string -> string`).
        state_labels: BTreeMap<String, String>,
    },
}

/// The `pane.output_matched` matcher (`OutputMatch`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum OutputMatch {
    /// `substring` — literal containment.
    #[serde(rename = "substring")]
    Substring {
        /// The literal.
        value: String,
    },
    /// `regex`.
    #[serde(rename = "regex")]
    Regex {
        /// The pattern.
        value: String,
    },
}

/// One `events.subscribe` item (`Subscription` on the wire) — the three
/// kinds the confirmed subset arms.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum SubscriptionSpec {
    /// `pane.output_matched` — arms a one-shot output marker.
    #[serde(rename = "pane.output_matched")]
    OutputMatched {
        /// `pane_id`.
        pane_id: String,
        /// `source` — the read source to match against.
        source: ReadSource,
        /// `match` — the matcher.
        #[serde(rename = "match")]
        output_match: OutputMatch,
        /// `lines`.
        #[serde(skip_serializing_if = "Option::is_none")]
        lines: Option<u32>,
        /// `strip_ansi`.
        #[serde(skip_serializing_if = "Option::is_none")]
        strip_ansi: Option<bool>,
    },
    /// `pane.scroll_changed`.
    #[serde(rename = "pane.scroll_changed")]
    ScrollChanged {
        /// `pane_id`.
        pane_id: String,
    },
    /// `pane.agent_status_changed`.
    #[serde(rename = "pane.agent_status_changed")]
    AgentStatusChanged {
        /// `pane_id`.
        pane_id: String,
        /// `agent_status` — restrict to one status, when set.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_status: Option<AgentStatus>,
    },
}
