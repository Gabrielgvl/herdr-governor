//! `coordinator` — the one task that owns the `Store` (§4.2): every
//! lifecycle write is a `Transition` computed and applied here, one `Msg`
//! processed to completion before the next is taken. A1 landed the loop,
//! the bounded apply-retry, `Event::Restart` marking (§4.3 step 5) and the
//! shutdown `watch` gate; A2 the `Tool` arm (F1/F7), B3 the `Tick`/
//! `Observation` arms (§4.7) — B1 adds `EffectResult`/`DispatchCommit`.
//! SQLite runs inline; async I/O happens only in the tasks that *post*
//! messages.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use governor_core::identity::{CallerEnvelope, ChildStatus, EffectKey, PaneId, Timestamp};
use governor_core::lifecycle::{self, EffectResult, Event, Transition, VersionTriple, Versioned};
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::store::Store;

use super::api::{ToolError, ToolRequest, ToolResponse};
use super::clock::Clock;
use super::paths::Paths;
use super::reconcile::HerdrHealth;
use super::runner;
use super::seam::SeamConfig;
use super::{CommitVerdict, RenderContext};

/// `apply` — the §4.2 bounded-apply mechanics every arm shares.
pub(super) mod apply;
/// `effects` — the `DispatchCommit`/`EffectResult` arms plus the §4.4
/// hand-out scan (B1).
mod effects;
/// `reload` — the F27 `SIGHUP` arm (`config::reload` against last-good).
mod reload;
/// `restart` — §4.3 step 5's `mark_restart` arm `daemon::run` drives.
mod restart;
/// `tick` — the `Msg::Tick`/`Msg::Observation` arms (§4.7).
mod tick;
/// `tool` — the `Msg::Tool` arm's implementation (F1 + F7).
mod tool;

/// The bounded mailbox §4.2 pins: 256 messages, so a flood of posts
/// applies backpressure instead of an unbounded queue.
pub(super) const MSG_CAPACITY: usize = 256;

/// The coordinator's mailbox. `#[non_exhaustive]`: B1/B3 add
/// `EffectResult`, `DispatchCommit` and `Observation` without touching
/// these arms. M2's connection task (`mcp::serve`) constructs `Tool`
/// and `VerifyCaller` from outside `daemon` through the `daemon::Msg`
/// re-export.
#[derive(Debug)]
#[non_exhaustive]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum Msg {
    /// A tool call that reached the listener — M2's connection task
    /// posts it with the request-time evidence it gathered; the arm
    /// resolves and binds the caller (F1) then serves the call
    /// (`herdr_status` is PR A's only tool, §7).
    Tool {
        /// The validated request.
        request: ToolRequest,
        /// The connection task's request-time `session.snapshot` — F1
        /// resolves the caller against this fresh read, never a cached
        /// one (§4.6); an `Err` here is `DAEMON_UNAVAILABLE`, never an
        /// identity verdict. Boxed: `Tool` otherwise dwarfs `Tick`.
        snapshot: Box<Result<Observed<SessionSnapshot>, HerdrError>>,
        /// The connection task's `canonicalize` of the envelope's
        /// `projectRoot` — `None` when the path does not resolve (H#3).
        resolved_root: Option<String>,
        /// The reply slot the connection task waits on.
        reply: oneshot::Sender<ToolResponse>,
    },
    /// A framed non-tool request's F1 check — `initialize`, `ping`,
    /// `tools/list` and unknown methods carry the relay's
    /// `relayInstanceId` too, so the FIRST framed request (often an
    /// `initialize`) must bind it and every later one must verify it,
    /// not only the first `tools/call` (F1). The arm runs the Tool
    /// arm's own resolve-and-bind; the reply carries the verdict alone.
    VerifyCaller {
        /// The relay-attached caller envelope.
        caller: CallerEnvelope,
        /// The same request-time `session.snapshot` `Tool` carries —
        /// boxed for size parity; an `Err` is `DAEMON_UNAVAILABLE`,
        /// never an identity verdict.
        snapshot: Box<Result<Observed<SessionSnapshot>, HerdrError>>,
        /// The connection task's `canonicalize` of the envelope's
        /// `projectRoot` — `None` when the path does not resolve (H#3).
        resolved_root: Option<String>,
        /// The verdict slot the connection task waits on — `Ok(())`
        /// lets the method answer locally.
        reply: oneshot::Sender<Result<(), ToolError>>,
    },
    /// One reconcile tick: a fresh Herdr snapshot or its error — the
    /// `on_tick` arm records health and runs the §4.7 steps-0–2 pass.
    Tick {
        /// The tick task's snapshot read.
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
    },
    /// §4.7 step 8 — a `pane.agent_status_changed` event: the
    /// subscription task's fresh `session.snapshot` rides the message
    /// (the coordinator performs no I/O, so the read it triggers travels
    /// with it). `status` is the wire value, carried for diagnostics —
    /// classification re-derives state from the snapshot.
    Observation {
        /// The pane the status event named.
        pane_id: PaneId,
        /// The event's `agent_status` reduced to the spec states.
        status: Option<ChildStatus>,
        /// The fresh snapshot the subscription task took for the event —
        /// or its error (a re-arm `Connect` failure's "gone" report).
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
    },
    /// §4.4 step 3 — a runner's dispatch commit request: the coordinator
    /// re-reads the row and every gate against durable state, then either
    /// writes `[WriteEffect::Dispatch]` (plus the outbox link) and replies
    /// `Go`, or leaves/refuses the row and replies `Skip`/`Refused`.
    DispatchCommit {
        /// The effect key the runner is asking to dispatch.
        key: EffectKey,
        /// The runner's frozen context — the commit arm's audit input.
        context: Arc<RenderContext>,
        /// The runner waits on this for its verdict.
        reply: oneshot::Sender<CommitVerdict>,
    },
    /// §4.4 step 7 — a runner's wire result: the journaled resolution
    /// plus its receipt or cause. Boxed: the receipt's `JudgmentRecord`
    /// dwarfs the other variants.
    EffectResult(Box<EffectResult>),
    /// A runner's pre-commit exit — its fresh verify found nothing honest
    /// to wire (§4.4 step 2's `None`), so the row stays `planned` and the
    /// in-flight subject claim drops: the next hand-off re-offers it.
    /// Runner exits *after* `DispatchCommit` release via that arm's
    /// non-`Go` verdicts or the `EffectResult` arm instead.
    ReleaseSubject {
        /// The effect key the claim was taken under.
        key: EffectKey,
    },
    /// `SIGHUP` → reload the catalog (F27 adopt/retain); `SIGTERM`/`SIGINT`
    /// → the §4.14 shutdown.
    Signal(Signal),
}

/// Which signal arrived. Rides `Msg`'s visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum Signal {
    /// `SIGHUP` — re-read `catalog.toml`; valid swaps in, invalid retains.
    Reload,
    /// `SIGTERM`/`SIGINT` — the §4.14 orderly stop.
    Shutdown,
}

/// How `serve` ended — for diagnostics; both paths tear down identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// `Signal::Shutdown` from the signal task.
    Signalled,
    /// The `daemon::run` oneshot fired (in-process tests) or every
    /// `Msg` sender dropped.
    Requested,
}

/// What `Coordinator::new` needs beyond the `Store` — bundled so the
/// constructor stays readable.
pub(super) struct CoordinatorArgs {
    /// The loaded+validated catalog and policy.
    pub loaded: LoadedConfig,
    /// The `[daemon]` table (required by the time this is constructed).
    pub daemon: DaemonSettings,
    /// The catalog's path (for `SIGHUP` reload).
    pub catalog_path: PathBuf,
    /// The one clock.
    pub clock: Clock,
    /// The armed fault seam, if any.
    pub seam: Option<SeamConfig>,
    /// The state-dir layout — the render contexts read `handoffs`/
    /// `frozen` for the paths prompts and asks name (§4.4).
    pub paths: Paths,
}

/// The coordinator: owns `Store`, `LoadedConfig`, `Clock`, the shutdown
/// `watch` sender and the current `[daemon]` table; serves `Msg` until a
/// shutdown.
pub(super) struct Coordinator {
    store: Store,
    loaded: LoadedConfig,
    daemon: DaemonSettings,
    catalog_path: PathBuf,
    clock: Clock,
    seam: Option<SeamConfig>,
    /// The §4.14 step-1 flag the runner's pre-wire gate reads (`watch` —
    /// B1 subscribes receivers from this sender).
    shutdown_watch: watch::Sender<bool>,
    /// When the coordinator was constructed — F7's uptime epoch (the one
    /// clock).
    started_at: Timestamp,
    /// §4.7 health/freshness — the one Herdr-health record every
    /// snapshot read feeds (tick, observation, startup pass and the
    /// `Tool` arm's request-time read); F7 renders it as
    /// `herdr.freshSecsAgo`/`incarnation`.
    health: HerdrHealth,
    /// The pane-set feed the subscription maintainer (§4.7 step 8)
    /// watches — `None` until `arm_subscriptions`, so the startup pass
    /// and unit tests push nothing.
    subs_feed: Option<watch::Sender<BTreeSet<String>>>,
    /// When the live config adopted — F7's `config.lastGoodAt`.
    config_adopted_at: Timestamp,
    /// The last refused reload's class (`read`/`decode`/`invalid`) —
    /// `None` while the last catalog attempt adopted; F7 renders it as
    /// `config.valid`/`lastError`.
    config_last_error: Option<&'static str>,
    /// The runner pool's environment — `None` until `arm_runner` wires it
    /// (the mailbox `tx` exists only after `daemon::run`'s channel).
    runner: Option<runner::RunnerEnv>,
    /// The state-dir layout for `RenderContext` builds.
    paths: Paths,
    /// §4.4's per-subject serialization: subject → the in-flight effect
    /// key. One effect per subject at a time, dispatched in
    /// `planned_at, effect_id` order.
    in_flight: BTreeMap<effects::Subject, EffectKey>,
}

impl Coordinator {
    /// The single-owner task: `store` is moved in; the shutdown `watch`
    /// starts unset and is raised exactly once (§4.14 step 1). The
    /// construction `now` doubles as F7's `lastGoodAt` — the startup
    /// catalog was just adopted.
    pub(super) fn new(store: Store, args: CoordinatorArgs) -> Self {
        let (shutdown_watch, _) = watch::channel(false);
        let now = args.clock.now();
        Self {
            store,
            loaded: args.loaded,
            daemon: args.daemon,
            catalog_path: args.catalog_path,
            clock: args.clock,
            seam: args.seam,
            shutdown_watch,
            started_at: now,
            health: HerdrHealth::default(),
            subs_feed: None,
            config_adopted_at: now,
            config_last_error: None,
            runner: None,
            paths: args.paths,
            in_flight: BTreeMap::new(),
        }
    }

    /// The armed seam — the runner's checkpoints and the result arm's
    /// `result_committed` checkpoint read it.
    pub(super) fn seam(&self) -> Option<SeamConfig> {
        self.seam.clone()
    }

    /// A receiver for the shutdown `watch` — M2's accept task takes one
    /// for §4.14 step-1 admission stop; B1's runner takes one per
    /// dispatch (§4.14's pre-wire gate).
    pub(super) fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_watch.subscribe()
    }

    /// §4.7 health/freshness — the tests' read of the record `tool`'s
    /// `status_view` renders (`health` itself stays field-private).
    #[cfg(test)]
    pub(super) fn health(&self) -> &HerdrHealth {
        &self.health
    }

    /// The message loop: process `Msg`s to completion until
    /// `Signal::Shutdown`, the `shutdown` oneshot, or every sender
    /// dropping — then hand back `self` so teardown drops fields in order.
    #[expect(
        clippy::integer_division_remainder_used,
        reason = "tokio::select! expands to an internal `%` index over its arms; the written arms have no arithmetic"
    )]
    pub(super) async fn serve(
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
                            self.handle(msg).await;
                            // Every arm is also a dispatch trigger: an
                            // apply that planned an effect hands it out.
                            self.hand_out();
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

    /// One `Msg` to completion.
    pub(super) async fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Tool {
                request,
                snapshot,
                resolved_root,
                reply,
            } => {
                let response = self.tool(request, *snapshot, resolved_root.as_deref());
                let _unused = reply.send(response);
            }
            Msg::VerifyCaller {
                caller,
                snapshot,
                resolved_root,
                reply,
            } => {
                let verdict = self.verify(&caller, *snapshot, resolved_root.as_deref());
                let _unused = reply.send(verdict);
            }
            Msg::Tick { snapshot } => self.on_tick(&snapshot),
            Msg::Observation {
                pane_id,
                status,
                snapshot,
            } => self.on_observation(&pane_id, status, &snapshot),
            Msg::DispatchCommit {
                key,
                context,
                reply,
            } => self.on_dispatch_commit(&key, &context, reply),
            Msg::EffectResult(result) => self.on_effect_result(result).await,
            Msg::ReleaseSubject { key } => self.free(&key),
            Msg::Signal(Signal::Reload) => self.reload().await,
            Msg::Signal(Signal::Shutdown) => {
                // Handled in `serve`'s arm — unreachable here.
            }
        }
    }
}

/// The `Versioned` stamp a `Restart`/`Obs`/`Deadline` event rides — the
/// Run's current versions (`on_restart` exempts the staleness check;
/// `obs`/`deadline` events use the full triple).
pub(super) fn versioned(run: &lifecycle::Run, event: Event) -> Versioned<Event> {
    Versioned {
        requested_against: VersionTriple {
            version: run.version,
            work_generation: run.work_generation,
            evidence_generation: run.evidence_generation,
        },
        value: event,
    }
}

/// An empty `Transition` — the recompute's "row vanished, nothing to
/// write" answer (core's `nothing()` is crate-private).
pub(super) fn empty() -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::new(),
    }
}
