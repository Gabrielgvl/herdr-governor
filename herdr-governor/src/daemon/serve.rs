//! `serve` — the reconcile `tick` task (§4.3 step 8): a fresh
//! `session.snapshot` (or its error) posted to the coordinator every
//! `reconcile`. The MCP accept loop lived here as a placeholder under
//! A1; M2 moved it to `mcp::serve` (the real transport — v1 relay
//! frames, `Msg::Tool`, one bounded line per connection).

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::adapters::herdr;

use super::coordinator::Msg;
use super::settings::Resolved;

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
