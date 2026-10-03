//! The scripted-world knobs — the `impl Topology` block lives here so
//! `topology.rs` stays under the 500-line gate. Each knob mutates the
//! world directly: it never goes through a wire request, and a request
//! the mutation mimics is never logged in `requests`.

use std::path::PathBuf;

use herdr_governor::adapters::herdr::SessionKind;

use super::{Occupant, PaneRow, SessionRef, Topology};

impl Topology {
    /// Knob: append screen output to a pane (bumps `revision`); returns
    /// the row so the caller can run output matchers. Panics on an
    /// unknown pane — a scripting error.
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

    /// Knob: occupant replacement — the same `pane_id` and `terminal_id`
    /// with a fresh `native_session` (F1 `a4_native_new_replaces_session`;
    /// name, kind and status carry over). Panics on a pane with no
    /// scripted occupant.
    pub fn replace_occupant(&mut self, pane_id: &str) -> &PaneRow {
        let Some(occupant) = self.pane(pane_id).and_then(|p| p.agent.clone()) else {
            panic!("pane {pane_id} has no scripted occupant");
        };
        let session = self.mint_session(&occupant.name);
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.agent = Some(Occupant {
            session: Some(session),
            ..occupant
        });
        pane
    }

    /// Knob: the occupant's process exits — the pane reverts to a shell
    /// in place (`agent_status` `unknown`, no agent row). Returns the row
    /// so the caller can deliver the status change.
    pub fn agent_exit(&mut self, pane_id: &str) -> &PaneRow {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.agent = None;
        pane
    }

    /// Knob: the pane is gone — removed outright, no `pane.close`
    /// request (S4b's gone-before-the-tick absence).
    pub fn remove_pane(&mut self, pane_id: &str) {
        let pos = self
            .panes
            .iter()
            .position(|p| p.pane_id == pane_id)
            .unwrap_or_else(|| panic!("pane {pane_id} is not scripted"));
        self.panes.remove(pos);
    }

    /// Knob: set or clear the occupant's `agent_session` — `None` makes
    /// the Run sessionless (S7b `identity_unprovable`); a `Some` keeps
    /// the session's current kind.
    pub fn set_agent_session(&mut self, pane_id: &str, session: Option<&str>) {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        let Some(agent) = pane.agent.as_mut() else {
            panic!("pane {pane_id} has no scripted occupant");
        };
        let kind = agent.session.as_ref().map_or(SessionKind::Id, |s| s.kind);
        agent.session = session.map(|value| SessionRef {
            kind,
            value: value.to_owned(),
        });
    }

    /// Knob: set the agent surface's `state_change_seq` (the retirement
    /// stability probe, R7) — independent of the content `revision`.
    pub fn set_state_change_seq(&mut self, pane_id: &str, seq: u64) {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.state_change_seq = Some(seq);
    }

    /// Knob: set the text one `pane.read` `source` reports — the
    /// `detection`/`visible` reads the retirement clock and the composer
    /// guard take. Bumps `revision`; sources without an override still
    /// read `text`. Returns the row for the output matchers.
    pub fn set_pane_text(&mut self, pane_id: &str, source: &str, text: &str) -> &PaneRow {
        let pane = self
            .pane_mut(pane_id)
            .unwrap_or_else(|e| panic!("{}", e.message));
        pane.source_texts.insert(source.to_owned(), text.to_owned());
        pane.revision = pane.revision.saturating_add(1);
        pane
    }

    /// Knob: script a session directory — later `agent.start`s and
    /// `replace_occupant`s mint `kind:"path"` `agent_session`s under it
    /// (a46 `--session-dir`); the file is materialized by the fake, the
    /// transcript records are the test's to write.
    pub fn set_session_dir(&mut self, dir: PathBuf) {
        self.session_dir = Some(dir);
    }
}
