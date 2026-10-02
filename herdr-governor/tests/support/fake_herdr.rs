//! The fake Herdr server (P4.H2): an async `tokio::net::UnixListener`
//! test double speaking the H1 client's protocol-22 surface over a
//! scripted topology, with the fault knobs Phase 5's supervision suite
//! drives. Every wire behavior it reproduces is the recorded one
//! (`a2-subscription-evidence.json`, `a3-start-prompt-evidence.json`,
//! `a46-identity-evidence.json`): one unary request per connection, the
//! `subscription_started` ack, `{event, data}` events, silent close on a
//! second or malformed frame of an armed stream, `id:""` +
//! `invalid_request` then close on a malformed fresh-connection request.
//! Knobs that go beyond the evidence are named *scripted divergence* in
//! `script.rs`. Deterministic: delays are `tokio::time::sleep` (the
//! paused test clock advances them), ordering is scripted queues.
//!
//! `topology` — the tables and the unary dispatcher; `script` — the fault
//! queue and the armed-subscription registry; this file — the listener,
//! connection discipline, restart and the test-facing handle.

pub mod script;
pub mod topology;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use herdr_governor::adapters::herdr::Client;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

pub use script::{Fault, Script, Subscriptions};
pub use topology::{Topology, WireError};

/// The confirmed protocol-22 subset the fake answers; anything else is an
/// unknown method → `invalid_request` with `id:""` and close (a2
/// `unknown_method` in `cases_closed_connection`).
pub const KNOWN_METHODS: [&str; 13] = [
    "ping",
    "session.snapshot",
    "pane.get",
    "pane.read",
    "pane.split",
    "pane.close",
    "tab.create",
    "agent.start",
    "agent.prompt",
    "agent.get",
    "agent.list",
    "agent.read",
    "events.subscribe",
];

/// The server-side line bound: 2 MiB (a2 `oversize_2mib` closed the
/// connection; the client's own bound is the stricter 1 MiB).
pub const SERVER_MAX_LINE: usize = 2 * 1024 * 1024;

/// Everything the connection handlers share, behind one mutex (never held
/// across an await).
#[derive(Debug)]
pub struct State {
    /// The scripted tables.
    pub topology: Topology,
    /// The fault queues.
    pub script: Script,
    /// The armed streams.
    pub subs: Subscriptions,
    /// Every well-formed request the fake accepted: `(method, params)` —
    /// the proof a lost-ack request still landed.
    pub requests: Vec<(String, Value)>,
}

/// The test-facing handle: the socket in a tempdir, the listener task, the
/// live connection tasks and the restart incarnation.
#[derive(Debug)]
pub struct FakeHerdr {
    _dir: tempfile::TempDir,
    path: PathBuf,
    state: Arc<Mutex<State>>,
    accept: Option<JoinHandle<()>>,
    conns: Arc<Mutex<JoinSet<()>>>,
    incarnation: u64,
}

impl FakeHerdr {
    /// Bind a fresh socket (inside a Tokio runtime) and serve `topology`.
    #[must_use]
    pub fn start(topology: Topology) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("herdr.sock");
        let mut server = Self {
            _dir: dir,
            path,
            state: Arc::new(Mutex::new(State {
                topology,
                script: Script::default(),
                subs: Subscriptions::default(),
                requests: Vec::new(),
            })),
            accept: None,
            conns: Arc::new(Mutex::new(JoinSet::new())),
            incarnation: 0,
        };
        server.relisten();
        server
    }

    /// A client dialing this fake.
    #[must_use]
    pub fn client(&self) -> Client {
        Client::new(&self.path)
    }

    /// The socket path.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The restart count: 1 for the first listen, +1 per `relisten`.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// The shared state, for scripting and assertions.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state poisoned")
    }

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

    /// Knob: daemon down — stop accepting, cut every connection (unary
    /// and armed) without a frame, remove the socket file. Connects fail
    /// until `relisten`.
    pub fn shutdown(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
        self.conns.lock().expect("conns poisoned").abort_all();
        self.state().subs.close_all();
        std::fs::remove_file(&self.path).ok();
    }

    /// Knob: daemon up again on the same path under a new incarnation —
    /// a fresh socket file, so the client's `ConnEpoch` inode moves. The
    /// topology survives (a scripted choice: the real server's restart
    /// topology is not recorded; Phase 5 scripts what it needs).
    pub fn relisten(&mut self) {
        let listener = UnixListener::bind(&self.path).expect("bind fake socket");
        self.incarnation = self.incarnation.saturating_add(1);
        let state = Arc::clone(&self.state);
        let conns = Arc::clone(&self.conns);
        self.accept = Some(tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let shared = Arc::clone(&state);
                conns
                    .lock()
                    .expect("conns poisoned")
                    .spawn(async move { handle_conn(stream, &shared).await });
            }
        }));
    }

    /// Knob: daemon restart — `shutdown` then `relisten`, atomically from
    /// the client's point of view (no window where connects fail).
    pub fn restart(&mut self) {
        self.shutdown();
        self.relisten();
    }

    /// The accepted requests so far.
    #[must_use]
    pub fn requests(&self) -> Vec<(String, Value)> {
        self.state().requests.clone()
    }
}

impl Drop for FakeHerdr {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read one bounded line (without `\n`). `Ok(None)` on EOF or a dead
/// peer, `Err(())` when the pending line exceeds the server bound.
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>, ()> {
    let mut line = Vec::new();
    loop {
        let Ok(chunk) = r.fill_buf().await else {
            return Ok(None);
        };
        if chunk.is_empty() {
            return Ok(None);
        }
        let n = chunk.len();
        if let Some(at) = chunk.iter().position(|b| *b == b'\n') {
            line.extend_from_slice(chunk.get(..at).unwrap_or_default());
            r.consume(at.saturating_add(1));
            return if line.len() > SERVER_MAX_LINE {
                Err(())
            } else {
                Ok(Some(line))
            };
        }
        line.extend_from_slice(chunk);
        r.consume(n);
        if line.len() > SERVER_MAX_LINE {
            return Err(());
        }
    }
}

async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, frame: &Value) -> bool {
    let mut line = frame.to_string().into_bytes();
    line.push(b'\n');
    w.write_all(&line).await.is_ok() && w.flush().await.is_ok()
}

/// The correlation-less `invalid_request` reply a malformed fresh
/// connection gets (a2 `fresh_connection_error_id: ""`), then close.
async fn reject<W: AsyncWrite + Unpin>(w: &mut W, message: &str) {
    let frame = json!({"id": "", "error": {"code": "invalid_request", "message": message}});
    write_json(w, &frame).await;
}

/// A well-formed `{id, method, params}` request line.
fn parse_request(line: &[u8]) -> Option<(String, String, Value)> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let id = v.get("id")?.as_str()?.to_owned();
    let method = v.get("method")?.as_str()?.to_owned();
    let params = v.get("params")?.clone();
    params.is_object().then_some((id, method, params))
}

/// One accepted connection: the first line decides everything —
/// malformed → reject + close; `events.subscribe` → arm and stream; any
/// other method → one scripted reply, then close (a pipelined second
/// frame is never read — a2 `pipelined_two_unary_one_connection`).
async fn handle_conn(stream: UnixStream, state: &Mutex<State>) {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let line = match read_line(&mut reader).await {
        Ok(Some(line)) => line,
        Ok(None) => return,
        Err(()) => return reject(&mut wr, "api request line is too large").await,
    };
    let Some((id, method, params)) = parse_request(&line) else {
        return reject(&mut wr, "api request is malformed").await;
    };
    if !KNOWN_METHODS.contains(&method.as_str()) {
        return reject(&mut wr, "api request method is unknown").await;
    }
    if method == "events.subscribe" {
        return serve_subscription(reader, wr, &id, &params, state).await;
    }
    let queued = guard(state).script.pop(&method);
    let reply = match queued.as_ref().map(|q| &q.fault) {
        Some(Fault::Raw(bytes)) => {
            wr.write_all(bytes).await.ok();
            wr.flush().await.ok();
            return;
        }
        Some(Fault::Busy) => {
            let pane = params.get("pane_id").and_then(Value::as_str).unwrap_or("?");
            error_frame(
                &id,
                "agent_pane_busy",
                &format!("agent target pane {pane} is not an available shell"),
            )
        }
        Some(Fault::TimeoutAfter(_)) => {
            wait(queued).await;
            error_frame(&id, "timeout", "timed out waiting for agent startup")
        }
        Some(Fault::DropResponse) => {
            apply(state, &method, &params).ok();
            // Hold the connection until the client gives up (its deadline)
            // or closes — nothing is ever written.
            while let Ok(Some(_)) = read_line(&mut reader).await {}
            return;
        }
        Some(Fault::Delay(_)) => {
            let frame = reply_frame(&id, apply(state, &method, &params));
            wait(queued).await;
            frame
        }
        None => reply_frame(&id, apply(state, &method, &params)),
    };
    write_json(&mut wr, &reply).await;
}

/// Apply one unary request to the topology, record it, and run the
/// output matchers (a prompt's text lands on the pane).
fn apply(state: &Mutex<State>, method: &str, params: &Value) -> Result<Value, WireError> {
    let mut s = guard(state);
    s.requests.push((method.to_owned(), params.clone()));
    let State { topology, subs, .. } = &mut *s;
    let out = topology.dispatch(method, params);
    for pane in &topology.panes {
        subs.match_output(pane);
    }
    drop(s);
    out
}

/// The state lock; a poisoned fake is a test bug, not a wire outcome.
fn guard(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().expect("fake state poisoned")
}

/// Block on a queued fault's delay gate (no-op without one).
async fn wait(queued: Option<script::Queued>) {
    if let Some(q) = queued {
        q.wait().await;
    }
}

fn error_frame(id: &str, code: &str, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

fn reply_frame(id: &str, out: Result<Value, WireError>) -> Value {
    match out {
        Ok(result) => json!({"id": id, "result": result}),
        Err(e) => error_frame(id, e.code, &e.message),
    }
}

/// Validate and arm the specs under the lock (never across an await):
/// `Err` carries the frame to send before closing.
fn arm(
    state: &Mutex<State>,
    id: &str,
    params: &Value,
    specs: &[Value],
) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, Value> {
    let malformed = || error_frame("", "invalid_request", "api request params are malformed");
    let mut s = guard(state);
    let checked = specs.iter().enumerate().try_for_each(|(i, spec)| {
        let kind = spec.get("type").and_then(Value::as_str);
        let pane_id = spec
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(malformed)?;
        let known = matches!(
            kind,
            Some("pane.output_matched" | "pane.scroll_changed" | "pane.agent_status_changed")
        );
        if !known {
            return Err(malformed());
        }
        if s.topology.pane(pane_id).is_none() {
            return Err(error_frame(
                &format!("{id}:sub:{i}:probe"),
                "pane_not_found",
                &format!("pane {pane_id} not found"),
            ));
        }
        Ok(())
    });
    let rx = checked.map(|()| {
        s.requests
            .push(("events.subscribe".to_owned(), params.clone()));
        let State { topology, subs, .. } = &mut *s;
        let rx = subs.arm(specs.to_vec());
        for pane in &topology.panes {
            subs.match_output(pane);
        }
        rx
    });
    drop(s);
    rx
}

/// The armed stream: validate every spec (a structurally bad spec is
/// `invalid_request` with `id:""`; an unknown pane is `pane_not_found`
/// under the derived `<id>:sub:<i>:probe` id — both close), ack with
/// `subscription_started`, fire any marker already on screen, then relay
/// events until the registry drops the stream or the client sends
/// anything further (closed silently, no error frame).
async fn serve_subscription(
    mut reader: BufReader<OwnedReadHalf>,
    mut wr: OwnedWriteHalf,
    id: &str,
    params: &Value,
    state: &Mutex<State>,
) {
    let Some(specs) = params.get("subscriptions").and_then(Value::as_array) else {
        return reject(&mut wr, "api request params are malformed").await;
    };
    let mut rx = match arm(state, id, params, specs) {
        Ok(rx) => rx,
        Err(frame) => {
            write_json(&mut wr, &frame).await;
            return;
        }
    };
    let ack = json!({"id": id, "result": {"type": "subscription_started"}});
    if !write_json(&mut wr, &ack).await {
        return;
    }
    // The relay owns the write half: when the registry drops this stream
    // the channel ends, the half is dropped, and the client sees EOF.
    let relay = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if wr.write_all(&bytes).await.is_err() || wr.flush().await.is_err() {
                return;
            }
        }
    });
    // Any further frame from the client — or its EOF — closes silently.
    read_line(&mut reader).await.ok();
    relay.abort();
}
