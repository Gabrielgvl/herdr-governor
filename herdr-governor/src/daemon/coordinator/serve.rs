//! `coordinator::serve` — the message loop: `serve` selects on the
//! shutdown oneshot and the mailbox, `handle` drives each `Msg` through
//! its arm, and `drain` serves the §4.14 step-2 shutdown window. The
//! arms themselves live in the sibling modules (`tool`, `tick`,
//! `effects`, `supervise`, `reload`).

use tokio::sync::{mpsc, oneshot};

use super::{CommitVerdict, Coordinator, Msg, Signal, Stop};

impl Coordinator {
    /// The message loop: process `Msg`s to completion until
    /// `Signal::Shutdown`, the `shutdown` oneshot, or every sender
    /// dropping — then hand back `self` so teardown drops fields in order.
    #[expect(
        clippy::integer_division_remainder_used,
        reason = "tokio::select! expands to an internal `%` index over its arms; the written arms have no arithmetic"
    )]
    pub(in crate::daemon) async fn serve(
        mut self,
        mut rx: mpsc::Receiver<Msg>,
        mut shutdown: Option<oneshot::Receiver<()>>,
    ) -> (Self, Stop) {
        // §4.7 step 8 — the maintainer arms on the set at bind time;
        // every tick/observation pushes the current set after it.
        self.push_subscription_specs();
        // §4.4 — a restart leaves `planned` rows behind: offer them before
        // the first message so the pipeline resumes without waiting on a
        // tick.
        self.hand_out();
        let stop = loop {
            tokio::select! {
                // The oneshot wins over a queued Msg (biased): a stop
                // request is never starved by a full mailbox.
                biased;
                fired = async {
                    match shutdown.as_mut() {
                        Some(stop_rx) => stop_rx.await.is_ok(),
                        // `pending` when no oneshot was given — the arm
                        // never completes.
                        None => std::future::pending::<bool>().await,
                    }
                } => {
                    if fired {
                        break self.begin_shutdown(Stop::Requested);
                    }
                    // The sender dropped without firing — disarm so the
                    // completed arm can't spin the loop.
                    shutdown = None;
                }
                incoming = rx.recv() => {
                    match incoming {
                        Some(Msg::Signal(Signal::Shutdown)) => {
                            break self.begin_shutdown(Stop::Signalled);
                        }
                        Some(msg) => {
                            // Every arm is also a dispatch trigger: an
                            // apply that planned an effect hands it out.
                            // A hold is not (F4/F10) — re-offering the
                            // row it left unchanged would spin its runner
                            // at round-trip speed; the next tick,
                            // observation or other arm re-offers it.
                            if self.handle(msg).await {
                                self.hand_out();
                            }
                        }
                        None => break self.begin_shutdown(Stop::Requested),
                    }
                }
            }
        };
        self.drain(&mut rx).await;
        (self, stop)
    }

    /// §4.14 step 2 — the in-flight drain: after the watch is set, serve
    /// `EffectResult`s (the wire answers runners still post) for
    /// `shutdown_grace`; `DispatchCommit`s are refused `Skip` (a row left
    /// `planned` re-dispatches at the next start — far more honest than a
    /// `dispatching` row the wire never saw). Everything else is dropped —
    /// the gate is closed, admission is over.
    async fn drain(&mut self, rx: &mut mpsc::Receiver<Msg>) {
        let deadline = tokio::time::Instant::now()
            .checked_add(self.daemon.shutdown_grace)
            .unwrap_or_else(tokio::time::Instant::now);
        while let Ok(incoming) = tokio::time::timeout_at(deadline, rx.recv()).await {
            match incoming {
                Some(Msg::EffectResult(result)) => self.on_effect_result(result).await,
                Some(Msg::DispatchCommit { reply, .. }) => {
                    let _gone = reply.send(CommitVerdict::Skip);
                }
                Some(_) => {}
                None => break,
            }
        }
    }

    /// §4.14 step 1 — stop admission: set the watch *before* anything else
    /// so a runner's pre-wire gate sees it; in-flight effects are B1's
    /// concern (A1 runs none).
    fn begin_shutdown(&mut self, stop: Stop) -> Stop {
        let _unused = self.shutdown_watch.send(true);
        stop
    }
}
