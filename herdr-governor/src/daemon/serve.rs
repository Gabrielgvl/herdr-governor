//! `serve` — the tasks the listener runs (§4.3 step 8): the accept loop
//! answering one bounded JSON-RPC line per connection (`ping` → `{}`,
//! anything else → `-32601`; M2 replaces this with the real `mcp`
//! transport — this handler exists so the §4.3 lock-probe has an answer
//! and a bad client can never hold the socket open), plus the reconcile
//! `tick` posting a fresh `session.snapshot` (or its error) to the
//! coordinator every `reconcile`.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::adapters::herdr::{self, MAX_FRAME_BYTES};

use super::coordinator::Msg;
use super::settings::Resolved;

/// Spawn the accept task. Runs until aborted at teardown; per-connection
/// handlers are detached tasks so a wedged client never stalls `accept`.
pub(super) fn spawn(listener: UnixListener) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(conn(stream));
                }
                Err(_) => {
                    // EMFILE/EAGAIN-style storms: yield briefly, keep
                    // accepting — the listener itself is still valid.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    })
}

/// Spawn the reconcile tick: `session.snapshot` once, then every
/// `resolved.reconcile`, posting each result to the coordinator. The
/// interval has a floor so a `reconcile_secs = 0` catalog can't busy-spin
/// the task.
pub(super) fn spawn_tick(
    resolved: &Resolved,
    op_timeout: Duration,
    tx: mpsc::Sender<Msg>,
) -> JoinHandle<()> {
    const FLOOR: Duration = Duration::from_millis(250);
    let client = herdr::Client::new(resolved.herdr_socket.clone());
    let interval = resolved.reconcile.max(FLOOR);
    tokio::spawn(async move {
        loop {
            let snapshot = client.session_snapshot(op_timeout).await;
            if tx.send(Msg::Tick { snapshot }).await.is_err() {
                return;
            }
            tokio::time::sleep(interval).await;
        }
    })
}

/// One connection: bounded newline-delimited JSON-RPC. `ping` (the §4.3
/// lock-probe's method) gets `{}`; any other request gets `-32601`;
/// notifications (no `id`) get nothing. A line over `MAX_FRAME_BYTES`,
/// a `write` error or EOF ends the connection.
async fn conn(stream: UnixStream) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        let bound = u64::try_from(MAX_FRAME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
        let read = (&mut reader).take(bound).read_until(b'\n', &mut line).await;
        match read {
            // EOF or an over-limit line (no newline inside the bound).
            Ok(0) | Err(_) => return,
            Ok(_) if line.last() != Some(&b'\n') => return,
            Ok(_) => {}
        }
        let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&line)
        else {
            return;
        };
        let Some(id) = map.get("id").cloned() else {
            continue; // notification — nothing owed.
        };
        let reply = if map.get("method").and_then(serde_json::Value::as_str) == Some("ping") {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not found"},
            })
        };
        let mut out = serde_json::to_vec(&reply).unwrap_or_default();
        out.push(b'\n');
        if reader.get_mut().write_all(&out).await.is_err() {
            return;
        }
    }
}
