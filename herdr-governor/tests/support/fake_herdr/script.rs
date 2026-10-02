//! The fault script and the armed-subscription registry. A `Fault` is a
//! per-method, first-in-first-out knob the connection handler pops before
//! answering; the registry holds every armed `events.subscribe` stream and
//! runs the recorded one-shot output matching against the topology.
//!
//! Which knobs are *confirmed* wire behavior and which are *scripted
//! divergence* (needed by Phase 5, never recorded) is stated per variant —
//! the fake is not trusted, so the line matters (P4.H2 escalate-when).

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::topology::{PaneRow, read_json};

/// One scripted deviation from the normal answer to a unary request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Lost ack: apply the request, never reply, hold the connection open
    /// until the client gives up (its deadline) — confirmed shape of the
    /// `request_timeout` evidence: the server kept the handler running.
    DropResponse,
    /// Typed pre-flight refusal `agent_pane_busy` (a3
    /// `start_on_occupied_pane`); the request is not applied.
    Busy,
    /// Ambiguous start: after `delay`, the untyped `timeout` error (a3
    /// `runtime_failures`: "timed out waiting for agent startup") and the
    /// request is not applied — the inflight agent deregisters, the pane is
    /// back at the shell.
    TimeoutAfter(Duration),
    /// Apply the request, wait `delay`, then reply normally.
    Delay(Duration),
    /// Reply with these exact bytes (no `\n` appended) and close. Carries
    /// the malformed-frame and oversized-line knobs — **scripted
    /// divergence**: the evidence never recorded the server emitting a
    /// malformed or over-bound reply; Phase 5 needs the client side of it.
    Raw(Vec<u8>),
}

impl Fault {
    /// A reply line that is not a protocol-22 envelope.
    #[must_use]
    pub fn malformed() -> Self {
        Self::Raw(b"{\"type\":\"pong\"}\n".to_vec())
    }

    /// A reply line one byte over the client's 1 MiB bound (spec N5).
    #[must_use]
    pub fn oversized() -> Self {
        let mut line = vec![b'x'; 1024 * 1024 + 1];
        line.push(b'\n');
        Self::Raw(line)
    }
}

/// A queued fault plus the gate its delay opens. The delay timer is
/// registered when the fault is *queued*, not when the request arrives:
/// Tokio auto-advances a paused clock past I/O waits, so a sleep started
/// only inside the connection handler can lose the race against the
/// client's own deadline timer. Registered up front, it sits in the timer
/// wheel first and auto-advance reaches it first — deterministic.
#[derive(Debug)]
pub struct Queued {
    /// The scripted fault.
    pub fault: Fault,
    /// Resolves when the fault's delay has elapsed (`Delay`/`TimeoutAfter`).
    pub gate: Option<oneshot::Receiver<()>>,
}

impl Queued {
    /// Wait for the delay, if the fault carries one.
    pub async fn wait(self) {
        if let Some(gate) = self.gate {
            gate.await.ok();
        }
    }
}

/// Per-method FIFO queues of faults.
#[derive(Debug, Default)]
pub struct Script {
    queues: Vec<(String, VecDeque<Queued>)>,
}

impl Script {
    /// Queue `fault` for the next unscripted request of `method`. Must run
    /// inside the Tokio runtime (delay knobs spawn their timer here).
    pub fn push(&mut self, method: &str, fault: Fault) {
        let gate = match fault {
            Fault::Delay(delay) | Fault::TimeoutAfter(delay) => {
                let (tx, rx) = oneshot::channel();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    tx.send(()).ok();
                });
                Some(rx)
            }
            Fault::DropResponse | Fault::Busy | Fault::Raw(_) => None,
        };
        let queued = Queued { fault, gate };
        match self.queues.iter_mut().find(|(m, _)| m == method) {
            Some((_, q)) => q.push_back(queued),
            None => self
                .queues
                .push((method.to_owned(), VecDeque::from([queued]))),
        }
    }

    /// The next fault queued for `method`, if any.
    pub fn pop(&mut self, method: &str) -> Option<Queued> {
        self.queues
            .iter_mut()
            .find(|(m, _)| m == method)
            .and_then(|(_, q)| q.pop_front())
    }
}

/// One armed `events.subscribe` connection: the line sender its conn task
/// drains, the specs it armed, and the per-spec one-shot flag for
/// `pane.output_matched` (fires at most once per arm — a2 `one_shot`).
#[derive(Debug)]
pub struct ArmedSub {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    specs: Vec<Value>,
    fired: Vec<bool>,
}

/// The registry of armed subscription streams.
#[derive(Debug, Default)]
pub struct Subscriptions {
    armed: Vec<ArmedSub>,
}

impl Subscriptions {
    /// Register an armed stream; returns the receiver its conn task drains.
    pub fn arm(&mut self, specs: Vec<Value>) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let fired = vec![false; specs.len()];
        self.armed.push(ArmedSub { tx, specs, fired });
        rx
    }

    /// Knob: subscription EOF mid-stream — drop every armed sender; each
    /// conn task sees its channel end and closes the socket silently (the
    /// recorded teardown: zero bytes, no error frame).
    pub fn close_all(&mut self) {
        self.armed.clear();
    }

    /// Run the one-shot output matchers against `pane`'s current text and
    /// deliver `pane.output_matched` to each stream whose unfired
    /// `substring` marker is on screen (a2: arming while the marker is on
    /// screen fires immediately; the same marker never fires twice per
    /// arm). `regex` markers match as substrings — ponytail: no regex
    /// engine in the fake, add one when a test arms a real pattern.
    pub fn match_output(&mut self, pane: &PaneRow) {
        self.armed.retain(|s| !s.tx.is_closed());
        for sub in &mut self.armed {
            for (i, spec) in sub.specs.iter().enumerate() {
                let Some(fired) = sub.fired.get_mut(i) else {
                    continue;
                };
                if *fired
                    || spec.get("type").and_then(Value::as_str) != Some("pane.output_matched")
                    || spec.get("pane_id").and_then(Value::as_str) != Some(pane.pane_id.as_str())
                {
                    continue;
                }
                let Some(marker) = spec.pointer("/match/value").and_then(Value::as_str) else {
                    continue;
                };
                let Some(line) = pane.text.lines().find(|l| l.contains(marker)) else {
                    continue;
                };
                *fired = true;
                let empty = serde_json::Map::new();
                let read = read_json(pane, spec.as_object().unwrap_or(&empty));
                let data = json!({"pane_id": pane.pane_id, "matched_line": line, "read": read});
                sub.tx.send(frame("pane.output_matched", &data)).ok();
            }
        }
    }

    /// Deliver `pane.scroll_changed` to every stream armed on the pane.
    pub fn scroll_changed(&mut self, pane: &PaneRow, scroll: &Value) {
        let data = json!({"pane_id": pane.pane_id, "workspace_id": pane.workspace_id,
                          "scroll": scroll});
        self.deliver("pane.scroll_changed", &pane.pane_id, &data, |_| true);
    }

    /// Deliver `pane.agent_status_changed` to every stream armed on the
    /// pane whose optional `agent_status` filter admits the new status.
    pub fn agent_status_changed(&mut self, pane: &PaneRow) {
        let status = pane.agent.as_ref().map_or("unknown", |a| a.status.as_str());
        let data = json!({"pane_id": pane.pane_id, "workspace_id": pane.workspace_id,
                          "agent_status": status,
                          "agent": pane.agent.as_ref().map(|a| a.kind.clone()),
                          "display_agent": Value::Null, "title": Value::Null,
                          "state_labels": {}});
        self.deliver("pane.agent_status_changed", &pane.pane_id, &data, |spec| {
            spec.get("agent_status")
                .and_then(Value::as_str)
                .is_none_or(|want| want == status)
        });
    }

    fn deliver(&mut self, kind: &str, pane_id: &str, data: &Value, admit: impl Fn(&Value) -> bool) {
        self.armed.retain(|s| !s.tx.is_closed());
        let line = frame(kind, data);
        for sub in &self.armed {
            let armed_here = sub.specs.iter().any(|spec| {
                spec.get("type").and_then(Value::as_str) == Some(kind)
                    && spec.get("pane_id").and_then(Value::as_str) == Some(pane_id)
                    && admit(spec)
            });
            if armed_here {
                sub.tx.send(line.clone()).ok();
            }
        }
    }
}

/// One `{event, data}` NDJSON line.
fn frame(event: &str, data: &Value) -> Vec<u8> {
    let mut line = json!({"event": event, "data": data})
        .to_string()
        .into_bytes();
    line.push(b'\n');
    line
}
