//! The evidence-shaped JSON the topology emits — `session_snapshot`,
//! `pane_info`, `pane_read` and the `agents`/`tabs`/`workspaces`
//! collections (pinned schema + a46/a3 captures).

use serde_json::{Map, Value, json};

use super::{PaneRow, Topology};

impl Topology {
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

    pub(super) fn agents_json(&self) -> Vec<Value> {
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

    pub(super) fn tab_json(&self, t: &super::TabRow) -> Value {
        json!({"tab_id": t.tab_id, "workspace_id": t.workspace_id, "number": t.number,
               "label": t.label, "focused": false,
               "agent_status": self.rollup(|p| p.tab_id == t.tab_id),
               "pane_count": self.panes.iter().filter(|p| p.tab_id == t.tab_id).count()})
    }

    pub(super) fn workspace_json(&self, w: &super::WorkspaceRow) -> Value {
        let in_ws = |p: &PaneRow| p.workspace_id == w.workspace_id;
        json!({"workspace_id": w.workspace_id, "number": w.number, "label": w.label,
               "focused": false, "agent_status": self.rollup(in_ws),
               "active_tab_id": self.tabs.iter().find(|t| t.workspace_id == w.workspace_id)
                                    .map(|t| t.tab_id.clone()),
               "tab_count": self.tabs.iter().filter(|t| t.workspace_id == w.workspace_id).count(),
               "pane_count": self.panes.iter().filter(|p| in_ws(p)).count()})
    }
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
        obj.insert(
            "state_change_seq".to_owned(),
            json!(p.state_change_seq.unwrap_or(p.revision)),
        );
        if let Some(s) = &a.session {
            obj.insert(
                "agent_session".to_owned(),
                json!({"agent": a.kind, "kind": s.kind,
                       "source": format!("herdr:{}", a.kind),
                       "value": s.value}),
            );
        }
    }
    v
}

/// The `pane_read` payload; `source` echoes the request. A
/// `set_pane_text` override wins for its source; every other source
/// reads `text` — ponytail: still no scrollback model, add one when a
/// test needs distinct `recent`/`recent_unwrapped` tails.
#[must_use]
pub fn read_json(p: &PaneRow, params: &Map<String, Value>) -> Value {
    let source = params.get("source").and_then(Value::as_str);
    let text = source
        .and_then(|s| p.source_texts.get(s))
        .map_or(p.text.as_str(), String::as_str);
    json!({"pane_id": p.pane_id, "tab_id": p.tab_id, "workspace_id": p.workspace_id,
           "revision": p.revision,
           "source": params.get("source").cloned().unwrap_or_else(|| json!("recent")),
           "format": "text", "text": text, "truncated": false})
}
