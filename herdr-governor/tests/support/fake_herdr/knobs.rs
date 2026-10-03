//! The test-facing knobs — the `impl FakeHerdr` block lives here so
//! `fake_herdr.rs` stays under the 500-line gate: scripted faults, world
//! mutations, stream deliveries and the request-log cursor the Phase-5
//! fault suite drives between wire requests. Each delegates to a
//! `Topology`/`Script`/`Subscriptions` method; see those for the
//! semantics.

use std::path::Path;

use herdr_governor::adapters::herdr::{ReadSource, SessionKind};
use serde_json::Value;

use super::topology::SessionRef;
use super::{FakeHerdr, Fault, State, touch};

impl FakeHerdr {
    /// Queue a fault for the next request of `method`.
    pub fn fault(&self, method: &str, fault: Fault) {
        self.state().script.push(method, fault);
    }

    /// Append screen output to a pane and run the output matchers.
    pub fn write_output(&self, pane_id: &str, text: &str) {
        let mut s = self.state();
        let State { topology, subs, .. } = &mut *s;
        let pane = topology.write_output(pane_id, text);
        subs.match_output(pane);
        drop(s);
    }

    /// Knob: pane replacement (see `Topology::replace_pane`).
    #[must_use]
    pub fn replace_pane(&self, pane_id: &str) -> String {
        self.state().topology.replace_pane(pane_id)
    }

    /// Knob: workspace move (see `Topology::move_to_workspace`).
    #[must_use]
    pub fn move_to_workspace(&self, pane_id: &str, workspace_id: &str) -> String {
        self.state()
            .topology
            .move_to_workspace(pane_id, workspace_id)
    }

    /// Knob: occupant replacement (see `Topology::replace_occupant`) —
    /// F1's `a4_native_new_replaces_session`. Returns the new
    /// `native_session`.
    pub fn replace_occupant(&self, pane_id: &str) -> String {
        let session = self
            .state()
            .topology
            .replace_occupant(pane_id)
            .agent
            .as_ref()
            .and_then(|a| a.session.clone());
        if let Some(s) = &session {
            materialize(s);
        }
        session.map_or_else(String::new, |s| s.value)
    }

    /// Knob: the occupant exits (see `Topology::agent_exit`) — the pane
    /// reverts to a shell and armed streams see the `unknown` status.
    pub fn agent_exit(&self, pane_id: &str) {
        let mut s = self.state();
        let State { topology, subs, .. } = &mut *s;
        let pane = topology.agent_exit(pane_id);
        subs.agent_status_changed(pane);
        drop(s);
    }

    /// Knob: the pane is gone (see `Topology::remove_pane`) — absence
    /// without a `pane.close` request.
    pub fn remove_pane(&self, pane_id: &str) {
        self.state().topology.remove_pane(pane_id);
    }

    /// Knob: set or clear the occupant's `agent_session` (see
    /// `Topology::set_agent_session`).
    pub fn set_agent_session(&self, pane_id: &str, session: Option<&str>) {
        self.state().topology.set_agent_session(pane_id, session);
    }

    /// Knob: set the agent surface's `state_change_seq` (see
    /// `Topology::set_state_change_seq`).
    pub fn set_state_change_seq(&self, pane_id: &str, seq: u64) {
        self.state().topology.set_state_change_seq(pane_id, seq);
    }

    /// Knob: set one `pane.read` `source`'s text and run the output
    /// matchers (see `Topology::set_pane_text`).
    pub fn set_pane_text(&self, pane_id: &str, source: ReadSource, text: &str) {
        let wire = serde_json::to_value(source).expect("a source serializes");
        let mut s = self.state();
        let State { topology, subs, .. } = &mut *s;
        let pane = topology.set_pane_text(pane_id, wire.as_str().unwrap_or("recent"), text);
        subs.match_output(pane);
        drop(s);
    }

    /// Knob: script a session directory — create it; every later
    /// `agent.start`/`replace_occupant` reports `agent_session`
    /// `kind:"path"` under it (a46 `--session-dir`). The transcript file
    /// is created empty; its records are the test's to write.
    pub fn session_dir(&self, dir: &Path) {
        std::fs::create_dir_all(dir).expect("session dir");
        self.state().topology.set_session_dir(dir.to_path_buf());
    }

    /// Knob: latch an `unavailable` error on every `session.snapshot`
    /// until cleared — **scripted divergence** (no snapshot failure is
    /// recorded; S7b's post-restart unavailable snapshot). The request
    /// is still accepted and logged.
    pub fn snapshot_fault(&self, on: bool) {
        self.state().snapshot_fault = on;
    }

    /// Knob: an agent status change, delivered to the armed streams.
    pub fn set_agent_status(&self, pane_id: &str, status: &str) {
        let mut s = self.state();
        let State { topology, subs, .. } = &mut *s;
        let pane = topology.set_agent_status(pane_id, status);
        subs.agent_status_changed(pane);
        drop(s);
    }

    /// Knob: a scroll change on `pane_id`, delivered to the armed streams.
    pub fn scroll(&self, pane_id: &str, scroll: &Value) {
        let mut s = self.state();
        let State { topology, subs, .. } = &mut *s;
        let pane = topology.pane(pane_id).expect("scripted pane");
        subs.scroll_changed(pane, scroll);
        drop(s);
    }

    /// Knob: subscription EOF mid-stream — every armed stream closes
    /// silently; the listener stays up (`immediate_rearm_after_teardown`).
    pub fn close_subscriptions(&self) {
        self.state().subs.close_all();
    }

    /// Knob: the accepted requests since index `n` — a cursor over
    /// `requests()`; feed it an earlier `requests().len()`.
    #[must_use]
    pub fn requests_since(&self, n: usize) -> Vec<(String, Value)> {
        self.state().requests.get(n..).unwrap_or(&[]).to_vec()
    }
}

/// Create a `kind:"path"` session's transcript file — empty and only
/// when absent (`create_new`: test-written records are never
/// truncated).
fn materialize(session: &SessionRef) {
    if session.kind == SessionKind::Path {
        touch(&session.value);
    }
}
