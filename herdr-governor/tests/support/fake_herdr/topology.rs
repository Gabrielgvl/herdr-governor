//! The fake's scripted topology — workspaces, tabs, panes and agents as
//! plain tables answering every unary op of the confirmed protocol-22
//! subset with evidence-shaped JSON (pinned schema + a46/a3 captures; the
//! recorded codes `pane_not_found`, `agent_pane_busy`, `agent_not_found`,
//! `invalid_request`). Pure: no I/O, no clock. Ids mirror the captures:
//! panes `w<n>:p<k>` counted per workspace (a move renumbers, a46
//! `after-workspace-move`), terminals `term_<hex>` surviving moves.
//!
//! `knobs` — the scripted-world fault knobs (`impl Topology` lives there
//! so this file stays under the 500-line gate); `json` — the
//! evidence-shaped emission (`session_snapshot`, `pane_info`,
//! `pane_read`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use herdr_governor::adapters::herdr::SessionKind;
use serde_json::{Map, Value, json};

pub mod json;
pub mod knobs;

pub use json::{pane_json, read_json};

/// A `{code, message}` the server answers instead of a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    /// The wire error code.
    pub code: &'static str,
    /// The wire error message.
    pub message: String,
}

fn pane_not_found(pane_id: &str) -> WireError {
    WireError {
        code: "pane_not_found",
        message: format!("pane {pane_id} not found"),
    }
}

fn invalid(message: &str) -> WireError {
    WireError {
        code: "invalid_request",
        message: message.to_owned(),
    }
}

/// The reported `agent_session` — `id` for a harness-minted session id,
/// `path` under a scripted `session_dir` (a46 `--session-dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    /// `kind` — `id` or `path`.
    pub kind: SessionKind,
    /// `value` — the session id, or the transcript path.
    pub value: String,
}

/// An agent occupying a pane (the a3 `agent_started` record's fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    /// `name` — the minted agent name.
    pub name: String,
    /// `agent` — the harness kind, opaque catalog data.
    pub kind: String,
    /// `agent_status` wire value.
    pub status: String,
    /// `agent_session` when the harness reports one.
    pub session: Option<SessionRef>,
}

/// One pane row — a shell until an agent starts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    /// `pane_id` locator.
    pub pane_id: String,
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `terminal_id` — stable across moves, fresh on replacement.
    pub terminal_id: String,
    /// `revision` — bumped on every text write.
    pub revision: u64,
    /// `state_change_seq` override (`set_state_change_seq`); unset reads
    /// as `revision`, the fake's stand-in for a real change counter.
    pub state_change_seq: Option<u64>,
    /// The screen contents `pane.read` returns and markers match on.
    pub text: String,
    /// Per-source `pane.read` overrides (`set_pane_text`) — the
    /// `detection`/`visible` reads the retirement sweep and composer
    /// guard take; a source without an entry reads `text`.
    pub source_texts: BTreeMap<String, String>,
    /// The occupant, if any.
    pub agent: Option<Occupant>,
}

/// One tab row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabRow {
    /// `tab_id`.
    pub tab_id: String,
    /// `workspace_id`.
    pub workspace_id: String,
    /// `number`.
    pub number: u32,
    /// `label`.
    pub label: String,
}

/// One workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    /// `workspace_id`.
    pub workspace_id: String,
    /// `number`.
    pub number: u32,
    /// `label`.
    pub label: String,
    /// Per-workspace pane counter (ids are `w<n>:p<k>`).
    pub next_pane: u32,
    /// Per-workspace tab counter.
    pub next_tab: u32,
}

/// The scripted session: the tables plus the counters that mint ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// `workspaces`.
    pub workspaces: Vec<WorkspaceRow>,
    /// `tabs`.
    pub tabs: Vec<TabRow>,
    /// `panes` (agents are panes with an occupant).
    pub panes: Vec<PaneRow>,
    /// Terminal-id counter.
    pub next_terminal: u64,
    /// Workspace counter.
    pub next_workspace: u32,
    /// Session counter — the first minted `id` session is bare
    /// (`<name>-session`), later mints carry the counter suffix so a
    /// `replace_occupant` never re-mints a live session.
    pub next_session: u64,
    /// The scripted session directory (`set_session_dir`): starts and
    /// occupant replacements mint `kind:"path"` sessions under it.
    pub session_dir: Option<PathBuf>,
}

impl Topology {
    /// The a46 `initial-shell` shape: one workspace `w1`, one tab
    /// `w1:t1`, one shell pane `w1:p1`.
    #[must_use]
    pub fn single_shell() -> Self {
        let mut t = Self {
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            next_terminal: 0x65c9_1800_0000,
            next_workspace: 1,
            next_session: 0,
            session_dir: None,
        };
        let ws = t.create_workspace("fake");
        t.create_tab(&ws);
        t
    }

    /// A new workspace with one tab and one root shell pane; returns its id.
    pub fn create_workspace(&mut self, label: &str) -> String {
        let number = self.next_workspace;
        self.next_workspace = self.next_workspace.saturating_add(1);
        let workspace_id = format!("w{number}");
        self.workspaces.push(WorkspaceRow {
            workspace_id: workspace_id.clone(),
            number,
            label: label.to_owned(),
            next_pane: 1,
            next_tab: 1,
        });
        workspace_id
    }

    /// A new tab with its root shell pane; returns `(tab_id, pane_id)`.
    /// Panics on an unknown workspace — a scripting error, not a wire one.
    pub fn create_tab(&mut self, workspace_id: &str) -> (String, String) {
        let ws = self
            .workspaces
            .iter_mut()
            .find(|w| w.workspace_id == workspace_id)
            .unwrap_or_else(|| panic!("workspace {workspace_id} is not scripted"));
        let number = ws.next_tab;
        ws.next_tab = ws.next_tab.saturating_add(1);
        let tab_id = format!("{workspace_id}:t{number}");
        self.tabs.push(TabRow {
            tab_id: tab_id.clone(),
            workspace_id: workspace_id.to_owned(),
            number,
            label: number.to_string(),
        });
        let pane_id = self.add_pane(workspace_id, &tab_id);
        (tab_id, pane_id)
    }

    fn mint_terminal(&mut self) -> String {
        self.next_terminal = self.next_terminal.wrapping_add(0x09af_7563);
        format!("term_{:014x}", self.next_terminal)
    }

    /// Mint the next `agent_session`: `kind:"path"` under `session_dir`
    /// (`<dir>/<seq>_<name>.jsonl`, the a46 `--session-dir` shape), else
    /// the `kind:"id"` `<name>-session` — bare on the first mint,
    /// counter-suffixed after.
    pub(super) fn mint_session(&mut self, name: &str) -> SessionRef {
        let n = self.next_session;
        self.next_session = self.next_session.saturating_add(1);
        if let Some(dir) = &self.session_dir {
            let file = dir.join(format!("{n:04}_{name}.jsonl"));
            return SessionRef {
                kind: SessionKind::Path,
                value: file.to_string_lossy().into_owned(),
            };
        }
        let value = if n == 0 {
            format!("{name}-session")
        } else {
            format!("{name}-session-{n}")
        };
        SessionRef {
            kind: SessionKind::Id,
            value,
        }
    }

    pub(super) fn add_pane(&mut self, workspace_id: &str, tab_id: &str) -> String {
        let terminal_id = self.mint_terminal();
        let ws = self
            .workspaces
            .iter_mut()
            .find(|w| w.workspace_id == workspace_id)
            .unwrap_or_else(|| panic!("workspace {workspace_id} is not scripted"));
        let pane_id = format!("{workspace_id}:p{}", ws.next_pane);
        ws.next_pane = ws.next_pane.saturating_add(1);
        self.panes.push(PaneRow {
            pane_id: pane_id.clone(),
            tab_id: tab_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            terminal_id,
            revision: 0,
            state_change_seq: None,
            text: String::new(),
            source_texts: BTreeMap::new(),
            agent: None,
        });
        pane_id
    }

    /// The pane row, if the locator resolves.
    #[must_use]
    pub fn pane(&self, pane_id: &str) -> Option<&PaneRow> {
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }

    pub(super) fn pane_mut(&mut self, pane_id: &str) -> Result<&mut PaneRow, WireError> {
        self.panes
            .iter_mut()
            .find(|p| p.pane_id == pane_id)
            .ok_or_else(|| pane_not_found(pane_id))
    }

    /// The agent's pane by `target` — an agent name or its pane id (the
    /// a3 `agent target <x> not found` resolution).
    fn agent_pane_mut(&mut self, target: &str) -> Result<&mut PaneRow, WireError> {
        self.panes
            .iter_mut()
            .find(|p| {
                p.agent
                    .as_ref()
                    .is_some_and(|a| a.name == target || p.pane_id == target)
            })
            .ok_or_else(|| WireError {
                code: "agent_not_found",
                message: format!("agent target {target} not found"),
            })
    }

    /// Answer one unary request: `Ok(result)` carries the `type`-tagged
    /// result object, `Err` the `{code, message}` to send. Unknown or
    /// mis-shaped requests are `invalid_request` (a2 `malformed` cases).
    pub fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, WireError> {
        let Some(p) = params.as_object() else {
            return Err(invalid("api request params are malformed"));
        };
        match method {
            "ping" => Ok(json!({"type": "pong", "version": "0.9.1", "protocol": 22,
                                "capabilities": {}})),
            "session.snapshot" => Ok(json!({"type": "session_snapshot",
                                            "snapshot": self.snapshot()})),
            "pane.get" => Ok(json!({"type": "pane_info",
                                    "pane": pane_json(self.pane_mut(str_param(p, "pane_id")?)?)})),
            "pane.read" => {
                let pane = self.pane_mut(str_param(p, "pane_id")?)?;
                Ok(json!({"type": "pane_read", "read": read_json(pane, p)}))
            }
            "agent.read" => {
                let pane = self.agent_pane_mut(str_param(p, "target")?)?;
                Ok(json!({"type": "pane_read", "read": read_json(pane, p)}))
            }
            "pane.split" => {
                let target = match p.get("target_pane_id").and_then(Value::as_str) {
                    Some(t) => t.to_owned(),
                    None => self
                        .panes
                        .first()
                        .map(|x| x.pane_id.clone())
                        .unwrap_or_default(),
                };
                let anchor = self.pane_mut(&target)?;
                let (ws, tab) = (anchor.workspace_id.clone(), anchor.tab_id.clone());
                let id = self.add_pane(&ws, &tab);
                Ok(json!({"type": "pane_info", "pane": pane_json(self.pane_mut(&id)?)}))
            }
            "pane.close" => {
                let id = str_param(p, "pane_id")?;
                self.pane_mut(id)?;
                self.panes.retain(|x| x.pane_id != id);
                Ok(json!({"type": "ok"}))
            }
            "tab.create" => {
                let ws = match p.get("workspace_id").and_then(Value::as_str) {
                    Some(w) => w.to_owned(),
                    None => self
                        .workspaces
                        .first()
                        .map(|w| w.workspace_id.clone())
                        .unwrap_or_default(),
                };
                if !self.workspaces.iter().any(|w| w.workspace_id == ws) {
                    return Err(invalid("workspace not found"));
                }
                let (tab_id, pane_id) = self.create_tab(&ws);
                let tab = self
                    .tabs
                    .iter()
                    .find(|t| t.tab_id == tab_id)
                    .map(|t| self.tab_json(t));
                Ok(json!({"type": "tab_created", "tab": tab,
                          "root_pane": pane_json(self.pane_mut(&pane_id)?)}))
            }
            "agent.start" => self.agent_start(p),
            "agent.prompt" => {
                let text = str_param(p, "text")?.to_owned();
                let pane = self.agent_pane_mut(str_param(p, "target")?)?;
                pane.text.push_str(&text);
                pane.text.push('\n');
                pane.revision = pane.revision.saturating_add(1);
                Ok(json!({"type": "agent_prompted", "agent": pane_json(pane)}))
            }
            "agent.get" => Ok(json!({"type": "agent_info",
                                     "agent": pane_json(self.agent_pane_mut(str_param(p, "target")?)?)})),
            "agent.list" => Ok(json!({"type": "agent_list", "agents": self.agents_json()})),
            _ => Err(invalid("api request method is unknown")),
        }
    }

    /// `agent.start`: pre-flight `agent_pane_busy` on an occupied pane (a3
    /// `start_on_occupied_pane`), else the agent registers idle and
    /// `interactive_ready` with the minted session.
    fn agent_start(&mut self, p: &Map<String, Value>) -> Result<Value, WireError> {
        let (name, kind, pane_id) = (
            str_param(p, "name")?.to_owned(),
            str_param(p, "kind")?.to_owned(),
            str_param(p, "pane_id")?,
        );
        let shell = self.pane(pane_id).ok_or_else(|| pane_not_found(pane_id))?;
        if shell.agent.is_some() {
            return Err(WireError {
                code: "agent_pane_busy",
                message: format!("agent target pane {pane_id} is not an available shell"),
            });
        }
        let session = self.mint_session(&name);
        let pane = self.pane_mut(pane_id)?;
        pane.agent = Some(Occupant {
            session: Some(session),
            name,
            kind: kind.clone(),
            status: "idle".to_owned(),
        });
        let mut argv = vec![Value::String(kind)];
        argv.extend(
            p.get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        Ok(json!({"type": "agent_started", "agent": pane_json(pane), "argv": argv}))
    }
}

fn str_param<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a str, WireError> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("api request params are malformed"))
}

/// An `Occupant` row the suites script onto `w1:p1` — `kind`/`status`
/// are opaque to the assertions; `session` is the `native_session`
/// caller binding reads back.
#[must_use]
pub fn occupant(name: &str, session: &str) -> Occupant {
    Occupant {
        name: name.to_owned(),
        kind: "harness-x".to_owned(),
        status: "idle".to_owned(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: session.to_owned(),
        }),
    }
}

/// `Topology::single_shell` with `w1:p1` occupied — the world the
/// status/identity suites assert against.
#[must_use]
pub fn occupied_topology() -> Topology {
    let mut topology = Topology::single_shell();
    if let Some(pane) = topology.panes.first_mut() {
        pane.agent = Some(occupant("gov-caller", "sess-1"));
    }
    topology
}

/// `single_shell` with `w1:p1` occupied by `name` on `session`,
/// returning `(topology, terminal_id)` — the launch suites seed the
/// run's captured identity against the terminal.
#[must_use]
pub fn agent_topology(name: &str, session: Option<&str>) -> (Topology, String) {
    let mut topology = Topology::single_shell();
    let terminal = topology
        .panes
        .first()
        .map(|pane| pane.terminal_id.clone())
        .unwrap_or_default();
    if let Some(pane) = topology.panes.first_mut() {
        pane.agent = Some(Occupant {
            name: name.to_owned(),
            kind: "kind-a".to_owned(),
            status: "working".to_owned(),
            session: session.map(|value| SessionRef {
                kind: SessionKind::Id,
                value: value.to_owned(),
            }),
        });
    }
    (topology, terminal)
}
