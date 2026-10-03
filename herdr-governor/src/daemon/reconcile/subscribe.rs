//! `subscribe` — §4.7 step 8, the status-event feed: a maintainer task
//! arms one `events.subscribe` stream over the child-pane set the
//! coordinator feeds through a `watch` sender, and posts each
//! `pane.agent_status_changed` event as `Msg::Observation` with a fresh
//! snapshot taken by the task — events are latency only; the tick's
//! snapshot carries correctness (the A2 ruling: no replay, catch-up is
//! the next snapshot).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use governor_core::identity::PaneId;

use crate::adapters::herdr::{self, SubEvent, Subscription, SubscriptionSpec};

use super::super::coordinator::Msg;
use crate::daemon::identity::child_status;

/// How long the subscription maintainer waits before retrying a failed
/// arm/re-arm — a failed arm usually means a dead server, and the tick is
/// the correctness backstop anyway.
const REARM_DELAY: Duration = Duration::from_secs(1);

/// The `events.subscribe` spec set for a pane set: one
/// `pane.agent_status_changed` per pane, unfiltered.
fn specs_for(panes: &BTreeSet<String>) -> Vec<SubscriptionSpec> {
    panes
        .iter()
        .map(|pane| SubscriptionSpec::AgentStatusChanged {
            pane_id: pane.clone(),
            agent_status: None,
        })
        .collect()
}

/// Spawn the subscription maintainer (§4.7 step 8): a task owning one
/// `events.subscribe` stream over the child-pane set the coordinator
/// feeds through the returned `watch` sender. Re-arms when the pane set
/// diverges or the stream ends; a `Connect`-class re-arm failure posts
/// the fresh snapshot's failure through `Msg::Observation` so health
/// records `gone` (the next tick would reach it anyway). Events post
/// `Msg::Observation{pane_id, status, snapshot}` — the coordinator never
/// performs I/O, so the task's fresh `session.snapshot` rides the
/// message it was taken for. Exits when the mailbox or the feed closes.
pub(in crate::daemon) fn spawn_subscriptions(
    socket: PathBuf,
    op_timeout: Duration,
    tx: mpsc::Sender<Msg>,
) -> (watch::Sender<BTreeSet<String>>, JoinHandle<()>) {
    let (specs_tx, specs_rx) = watch::channel(BTreeSet::<String>::new());
    let task = tokio::spawn(maintain(
        herdr::Client::new(socket),
        op_timeout,
        tx,
        specs_rx,
    ));
    (specs_tx, task)
}

/// The maintainer loop `spawn_subscriptions` spawns.
#[expect(
    clippy::integer_division_remainder_used,
    reason = "tokio::select! expands to an internal `%` index over its arms; the written arms have no arithmetic"
)]
async fn maintain(
    client: herdr::Client,
    op_timeout: Duration,
    tx: mpsc::Sender<Msg>,
    mut specs_rx: watch::Receiver<BTreeSet<String>>,
) {
    // The armed stream and the pane set it was armed on.
    let mut armed: Option<(BTreeSet<String>, Subscription)> = None;
    loop {
        let wanted = specs_rx.borrow_and_update().clone();
        if armed.as_ref().is_some_and(|(set, _)| *set != wanted) {
            // The pane set diverged under a live stream — re-arm on
            // the fresh set.
            armed = None;
        }
        let Some((set, mut sub)) = armed.take() else {
            if wanted.is_empty() {
                if specs_rx.changed().await.is_err() {
                    return;
                }
                continue;
            }
            match client.subscribe(specs_for(&wanted), op_timeout).await {
                Ok(sub) => armed = Some((wanted, sub)),
                Err(_) => tokio::time::sleep(REARM_DELAY).await,
            }
            continue;
        };
        tokio::select! {
            biased;
            changed = specs_rx.changed() => {
                armed = Some((set, sub));
                if changed.is_err() {
                    return;
                }
                // Loop head re-reads `wanted` and re-arms on divergence.
            }
            event = sub.next() => {
                match event {
                    Some(Ok(SubEvent::AgentStatusChanged {
                        pane_id,
                        agent_status,
                        ..
                    })) => {
                        armed = Some((set, sub));
                        let snapshot = client.session_snapshot(op_timeout).await;
                        let msg = Msg::Observation {
                            pane_id: PaneId(pane_id),
                            status: child_status(agent_status),
                            snapshot,
                        };
                        if tx.send(msg).await.is_err() {
                            return;
                        }
                    }
                    // Only `agent_status_changed` is ever armed.
                    Some(Ok(_other)) => armed = Some((set, sub)),
                    Some(Err(_)) | None => match sub.rearm().await {
                        Ok(next) => armed = Some((set, next)),
                        Err(_gone) => {
                            let snapshot = client.session_snapshot(op_timeout).await;
                            let msg = Msg::Observation {
                                pane_id: PaneId(String::new()),
                                status: None,
                                snapshot,
                            };
                            if tx.send(msg).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(REARM_DELAY).await;
                        }
                    },
                }
            }
        }
    }
}
