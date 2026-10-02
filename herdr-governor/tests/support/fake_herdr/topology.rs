//! The fake's scripted topology — workspaces, tabs, panes and agents as
//! plain tables answering every unary op of the confirmed protocol-22
//! subset with evidence-shaped JSON (pinned schema + a46/a3 captures; the
//! recorded codes `pane_not_found`, `agent_pane_busy`, `agent_not_found`,
//! `invalid_request`). Pure: no I/O, no clock. Ids mirror the captures:
//! panes `w<n>:p<k>` counted per workspace (a move renumbers, a46
//! `after-workspace-move`), terminals `term_<hex>` surviving moves.

use serde_json::{Map, Value, json};

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

/// An agent occupying a pane (the a3 `agent_started` record's fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    /// `name` — the minted agent name.
    pub name: String,
    /// `agent` — the harness kind, opaque catalog data.
    pub kind: String,
    /// `agent_status` wire value.
    pub status: String,
    /// `agent_session.value` when the harness reports one (`kind:"id"`).
    pub session: Option<String>,
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
    /// The screen contents `pane.read` returns and markers match on.
    pub text: String,
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

    fn add_pane(&mut self, workspace_id: &str, tab_id: &str) -> String {
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
            text: String::new(),
            agent: None,
        });
        pane_id
    }

    /// The pane row, if the locator resolves.
    #[must_use]
    pub fn pane(&self, pane_id: &str) -> Option<&PaneRow> {
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }

    fn pane_mut(&mut self, pane_id: &str) -> Result<&mut PaneRow, WireError> {
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

    /// Append screen output to a pane (bumps `revision`); returns the new
    /// text so the caller can run output matchers. Panics on an unknown
    /// pane — a scripting error.
    pub fn write_output(&mut self, pane_id: &str, text: &str) -> &PaneRow {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.text.push_str(text);
        pane.revision = pane.revision.saturating_add(1);
        pane
    }

    /// Knob: pane replacement — the pane is closed and a fresh shell pane
    /// takes its place in the same tab (a46 `closed-pane` →
    /// `recreated-pane`: new `pane_id`, new `terminal_id`, `agent_status`
    /// `unknown`, no agent). Returns the new pane id.
    pub fn replace_pane(&mut self, pane_id: &str) -> String {
        let pos = self
            .panes
            .iter()
            .position(|p| p.pane_id == pane_id)
            .unwrap_or_else(|| panic!("pane {pane_id} is not scripted"));
        let old = self.panes.remove(pos);
        self.add_pane(&old.workspace_id, &old.tab_id)
    }

    /// Knob: workspace move — the pane keeps its terminal, occupant and
    /// session but is renumbered into the destination workspace's first
    /// tab (a46 `after-workspace-move`: `w1:p1` → `w2:p1`, same
    /// `terminal_id`). Returns the new pane id.
    pub fn move_to_workspace(&mut self, pane_id: &str, workspace_id: &str) -> String {
        let tab_id = self
            .tabs
            .iter()
            .find(|t| t.workspace_id == workspace_id)
            .map_or_else(
                || panic!("workspace {workspace_id} has no tab"),
                |t| t.tab_id.clone(),
            );
        let ws = self
            .workspaces
            .iter_mut()
            .find(|w| w.workspace_id == workspace_id)
            .unwrap_or_else(|| panic!("workspace {workspace_id} is not scripted"));
        let new_id = format!("{workspace_id}:p{}", ws.next_pane);
        ws.next_pane = ws.next_pane.saturating_add(1);
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.pane_id.clone_from(&new_id);
        pane.tab_id = tab_id;
        pane.workspace_id = workspace_id.to_owned();
        new_id
    }

    /// Knob: set an agent's wire status (drives `agent_status_changed`).
    pub fn set_agent_status(&mut self, pane_id: &str, status: &str) -> &PaneRow {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        if let Some(agent) = pane.agent.as_mut() {
            agent.status = status.to_owned();
        }
        pane
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
    /// `interactive_ready` with an `id`-kind session.
    fn agent_start(&mut self, p: &Map<String, Value>) -> Result<Value, WireError> {
        let (name, kind, pane_id) = (
            str_param(p, "name")?.to_owned(),
            str_param(p, "kind")?.to_owned(),
            str_param(p, "pane_id")?,
        );
        let pane = self.pane_mut(pane_id)?;
        if pane.agent.is_some() {
            return Err(WireError {
                code: "agent_pane_busy",
                message: format!("agent target pane {pane_id} is not an available shell"),
            });
        }
        pane.agent = Some(Occupant {
            session: Some(format!("{name}-session")),
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

    /// The `session_snapshot` payload.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let layouts: Vec<Value> = self
            .tabs
            .iter()
            .map(|t| {
                let panes: Vec<Value> = self
                    .panes
                    .iter()
                    .filter(|p| p.tab_id == t.tab_id)
                    .map(|p| json!({"pane_id": p.pane_id, "focused": false}))
                    .collect();
                json!({"tab_id": t.tab_id, "workspace_id": t.workspace_id,
                       "panes": panes, "splits": [], "zoomed": false})
            })
            .collect();
        json!({
            "version": "0.9.1", "protocol": 22,
            "workspaces": self.workspaces.iter().map(|w| self.workspace_json(w)).collect::<Vec<_>>(),
            "tabs": self.tabs.iter().map(|t| self.tab_json(t)).collect::<Vec<_>>(),
            "panes": self.panes.iter().map(pane_json).collect::<Vec<_>>(),
            "layouts": layouts,
            "agents": self.agents_json(),
            "focused_workspace_id": self.workspaces.first().map(|w| w.workspace_id.clone()),
            "focused_tab_id": self.tabs.first().map(|t| t.tab_id.clone()),
            "focused_pane_id": self.panes.first().map(|p| p.pane_id.clone()),
        })
    }

    fn agents_json(&self) -> Vec<Value> {
        self.panes
            .iter()
            .filter(|p| p.agent.is_some())
            .map(pane_json)
            .collect()
    }

    fn rollup(&self, f: impl Fn(&PaneRow) -> bool) -> &'static str {
        if self.panes.iter().any(|p| f(p) && p.agent.is_some()) {
            "idle"
        } else {
            "unknown"
        }
    }

    fn tab_json(&self, t: &TabRow) -> Value {
        json!({"tab_id": t.tab_id, "workspace_id": t.workspace_id, "number": t.number,
               "label": t.label, "focused": false,
               "agent_status": self.rollup(|p| p.tab_id == t.tab_id),
               "pane_count": self.panes.iter().filter(|p| p.tab_id == t.tab_id).count()})
    }

    fn workspace_json(&self, w: &WorkspaceRow) -> Value {
        let in_ws = |p: &PaneRow| p.workspace_id == w.workspace_id;
        json!({"workspace_id": w.workspace_id, "number": w.number, "label": w.label,
               "focused": false, "agent_status": self.rollup(in_ws),
               "active_tab_id": self.tabs.iter().find(|t| t.workspace_id == w.workspace_id)
                                    .map(|t| t.tab_id.clone()),
               "tab_count": self.tabs.iter().filter(|t| t.workspace_id == w.workspace_id).count(),
               "pane_count": self.panes.iter().filter(|p| in_ws(p)).count()})
    }
}

fn str_param<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a str, WireError> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("api request params are malformed"))
}

/// A pane as `PaneInfo` — plus the `AgentInfo`-only members when occupied
/// (the schema's two records share every pane field; a46 shows `name` and
/// `interactive_ready` only on agent surfaces).
#[must_use]
pub fn pane_json(p: &PaneRow) -> Value {
    let mut v = json!({
        "pane_id": p.pane_id, "tab_id": p.tab_id, "workspace_id": p.workspace_id,
        "terminal_id": p.terminal_id, "revision": p.revision, "focused": false,
        "agent_status": p.agent.as_ref().map_or("unknown", |a| a.status.as_str()),
        "cwd": "/home/user/lane", "foreground_cwd": "/home/user/lane",
        "scroll": {"max_offset_from_bottom": 0, "offset_from_bottom": 0, "viewport_rows": 40},
    });
    if let (Some(a), Some(obj)) = (p.agent.as_ref(), v.as_object_mut()) {
        obj.insert("agent".to_owned(), json!(a.kind));
        obj.insert("name".to_owned(), json!(a.name));
        obj.insert("interactive_ready".to_owned(), json!(true));
        obj.insert("state_change_seq".to_owned(), json!(p.revision));
        if let Some(s) = &a.session {
            obj.insert(
                "agent_session".to_owned(),
                json!({"agent": a.kind, "kind": "id", "source": format!("herdr:{}", a.kind),
                       "value": s}),
            );
        }
    }
    v
}

/// The `pane_read` payload; `source` echoes the request. ponytail: every
/// source reads the same text — add a scrollback model when a test needs it.
#[must_use]
pub fn read_json(p: &PaneRow, params: &Map<String, Value>) -> Value {
    json!({"pane_id": p.pane_id, "tab_id": p.tab_id, "workspace_id": p.workspace_id,
           "revision": p.revision,
           "source": params.get("source").cloned().unwrap_or_else(|| json!("recent")),
           "format": "text", "text": p.text, "truncated": false})
}
