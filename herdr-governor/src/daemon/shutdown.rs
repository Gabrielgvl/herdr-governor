//! `shutdown` — §4.14: the signal task translating `SIGTERM`/`SIGINT` →
//! `Signal::Shutdown` and `SIGHUP` → `Signal::Reload` on the coordinator
//! mailbox, and `teardown` — the ordered drain: abort the accept/tick/
//! signal tasks, remove the socket file, then release the instance lock
//! last. Admission was already stopped inside the coordinator (the
//! shutdown `watch` was set before `serve` returned), so this is only the
//! file-system + task cleanup.

use std::io;
use std::path::Path;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::coordinator::{Msg, Signal};
use super::lock::InstanceLock;

/// Spawn the signal bridges — one task per signal kind: the first
/// `SIGTERM`/`SIGINT` posts one `Signal::Shutdown` and its task exits (a
/// second signal is the operator's `kill -9` cue, not a second post);
/// every `SIGHUP` posts a `Signal::Reload`. Registration failure (a
/// platform without unix signals — unreachable on the Linux-only build)
/// skips that task: the oneshot + mailbox-drop paths still stop the
/// daemon.
pub(super) fn spawn_signals(tx: &mpsc::Sender<Msg>) -> Vec<JoinHandle<()>> {
    [
        (SignalKind::terminate(), Signal::Shutdown),
        (SignalKind::interrupt(), Signal::Shutdown),
        (SignalKind::hangup(), Signal::Reload),
    ]
    .into_iter()
    .filter_map(|(kind, msg)| {
        let sender = tx.clone();
        signal(kind).ok().map(|mut stream| {
            tokio::spawn(async move {
                while stream.recv().await.is_some() {
                    let stop = msg == Signal::Shutdown;
                    if sender.send(Msg::Signal(msg)).await.is_err() || stop {
                        return;
                    }
                }
            })
        })
    })
    .collect()
}

/// §4.14 steps 3–5: the coordinator already set the shutdown `watch`
/// (step 1) and served out (step 2's in-flight bound — A1 runs no
/// effects), so: abort the accept/tick/signal tasks, remove the socket
/// file, release the lock last. `sock` is removed best-effort — an
/// `ENOENT` (a racing `rm`) is fine, every other error surfaces.
pub(super) async fn teardown(
    sock: &Path,
    tasks: Vec<JoinHandle<()>>,
    lock: InstanceLock,
) -> io::Result<()> {
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _unused = task.await;
    }
    match std::fs::remove_file(sock) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    drop(lock);
    Ok(())
}
