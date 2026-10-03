//! `coordinator` — the one task that owns the `Store` (§4.2): every
//! lifecycle write is a `Transition` computed and applied here, one `Msg`
//! processed to completion before the next is taken. A1 lands the loop,
//! the bounded apply-retry, `Event::Restart` marking (§4.3 step 5), the
//! shutdown `watch` gate and the `Tool` placeholder — B1 adds
//! `EffectResult`/`DispatchCommit`, B3 `Observation`. SQLite runs inline;
//! async I/O happens only in the tasks that *post* messages.

use std::collections::BTreeSet;
use std::path::PathBuf;

use governor_core::identity::{LaunchId, RunId, Timestamp};
use governor_core::lifecycle::{
    self, EffectKind, EffectState, Event, Transition, VersionTriple, Versioned, transition,
};
use governor_core::task::{AbstainReason, LaunchOutcome, LaunchPhase, finish};
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapters::config::{self, ConfigLoadError, DaemonSettings, LoadedConfig};
use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::store::{ApplyError, Store};

use super::DaemonError;
use super::api::{ToolError, ToolRequest, ToolResponse};
use super::clock::Clock;
use super::log;
use super::seam::SeamConfig;

/// The bounded mailbox §4.2 pins: 256 messages, so a flood of posts
/// applies backpressure instead of an unbounded queue.
pub(super) const MSG_CAPACITY: usize = 256;

/// The coordinator's mailbox. `#[non_exhaustive]`: B1/B3 add
/// `EffectResult`, `DispatchCommit` and `Observation` without touching
/// these arms.
#[derive(Debug)]
#[non_exhaustive]
pub(super) enum Msg {
    /// A tool call that reached the listener — A1 answers everything
    /// `DAEMON_UNAVAILABLE`: the transport exists, the tool surface
    /// lands with M2/A2.
    #[expect(
        dead_code,
        reason = "constructed by M2's connection task, which posts Tool to this mailbox"
    )]
    Tool {
        /// The validated request.
        request: ToolRequest,
        /// The reply slot the connection task waits on.
        reply: oneshot::Sender<ToolResponse>,
    },
    /// One reconcile tick: a fresh Herdr snapshot or its error (F28/B3
    /// derive observations from `Ok`; A1 records liveness only).
    Tick {
        /// The tick task's snapshot read.
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
    },
    /// `SIGHUP` → reload the catalog (F27 adopt/retain); `SIGTERM`/`SIGINT`
    /// → the §4.14 shutdown.
    Signal(Signal),
}

/// Which signal arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Signal {
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

/// The outcome of one bounded apply (§4.2's CAS contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyOutcome {
    /// The transition committed — `attempts` counts the tries taken.
    Applied {
        /// How many tries it took (1 = clean).
        attempts: usize,
    },
    /// Every attempt lost the CAS — logged and dropped, never retried past
    /// the bound.
    Dropped {
        /// Always 3 — the bound.
        attempts: usize,
    },
}

/// §4.3 step 5's counts — how many `dispatching` effects restart marked
/// `unconfirmed`, over how many Runs and stranded evaluations.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Marks {
    /// `dispatching` rows seen (the input set's size).
    pub effects: usize,
    /// Runs whose restart transition applied.
    pub runs: usize,
    /// Stranded `evaluating` launches abstained.
    pub evals: usize,
}

/// The apply bound §4.2 pins: re-read and recompute ≤ 3 attempts, then log
/// and drop.
const APPLY_BOUND: usize = 3;

/// §4.2 — one `store.apply` attempt per loop, recomputing the transition
/// against a fresh store read between attempts. `Conflict` retries; every
/// other `ApplyError` aborts (it is a bug or corruption, not a race).
/// An empty transition counts as a clean `Applied` without paying a
/// transaction.
pub(super) fn apply_with_retry(
    store: &mut Store,
    now: Timestamp,
    mut recompute: impl FnMut(&Store) -> Transition,
) -> Result<ApplyOutcome, ApplyError> {
    for attempt in 1..=APPLY_BOUND {
        let transition = recompute(store);
        let sizes = (
            transition.state_changes.len(),
            transition.events.len(),
            transition.effects.len(),
        );
        if sizes == (0, 0, 0) {
            return Ok(ApplyOutcome::Applied { attempts: attempt });
        }
        match store.apply(&transition, now) {
            Ok(()) => {
                log::applied(attempt, sizes.0, sizes.1, sizes.2);
                return Ok(ApplyOutcome::Applied { attempts: attempt });
            }
            Err(ApplyError::Conflict { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    log::apply_dropped(APPLY_BOUND, "conflict");
    Ok(ApplyOutcome::Dropped {
        attempts: APPLY_BOUND,
    })
}

/// §4.2 [r3] — concatenate `Transition`s into one apply: the governor-refused
/// path composes `[WriteEffect::Dispatch]` + `transition(Event::EffectResult)`
/// into a single commit. The three vecs append in order — `Transition` has
/// no other fields to merge.
#[expect(
    dead_code,
    reason = "the composed-transition consumer lands with P5.B1's DispatchCommit path"
)]
pub(super) fn concat(
    mut first: Transition,
    rest: impl IntoIterator<Item = Transition>,
) -> Transition {
    for next in rest {
        first.state_changes.extend(next.state_changes);
        first.events.extend(next.events);
        first.effects.extend(next.effects);
    }
    first
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
}

/// The coordinator: owns `Store`, `LoadedConfig`, `Clock`, the shutdown
/// `watch` sender and the current `[daemon]` table; serves `Msg` until a
/// shutdown.
pub(super) struct Coordinator {
    store: Store,
    loaded: LoadedConfig,
    daemon: DaemonSettings,
    catalog_path: PathBuf,
    #[expect(
        dead_code,
        reason = "the coordinator's one clock — B1's dispatchers and C3's review ticks read it"
    )]
    clock: Clock,
    seam: Option<SeamConfig>,
    /// The §4.14 step-1 flag the runner's pre-wire gate reads (`watch` —
    /// B1 subscribes receivers from this sender).
    shutdown_watch: watch::Sender<bool>,
}

impl Coordinator {
    /// The single-owner task: `store` is moved in; the shutdown `watch`
    /// starts unset and is raised exactly once (§4.14 step 1).
    pub(super) fn new(store: Store, args: CoordinatorArgs) -> Self {
        let (shutdown_watch, _) = watch::channel(false);
        Self {
            store,
            loaded: args.loaded,
            daemon: args.daemon,
            catalog_path: args.catalog_path,
            clock: args.clock,
            seam: args.seam,
            shutdown_watch,
        }
    }

    /// The armed seam — B1's runner reads it for `checkpoint` matching.
    #[expect(
        dead_code,
        reason = "carried through for P5.B1's runner/seam.rs checkpoints"
    )]
    pub(super) fn seam(&self) -> Option<SeamConfig> {
        self.seam.clone()
    }

    /// A receiver for the shutdown `watch` — B1's runner takes one per
    /// dispatch (§4.14's pre-wire gate).
    #[expect(dead_code, reason = "carried through for P5.B1's §4.14 step-4 gate")]
    pub(super) fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_watch.subscribe()
    }

    /// §4.3 step 5 — restart marking over **every** `dispatching` effect
    /// (the [r2] fix: the scan is global, not per unsettled Run — a
    /// settled Run's `close` row is reclassified too):
    ///
    /// * for every Run owning one (settled or not), apply
    ///   `transition(Event::Restart)` — `on_restart` writes
    ///   `dispatching → unconfirmed` and, for `prompting`, records the
    ///   task prompt's certainty;
    /// * for every `evaluating` Launch whose `jev_evaluate` is left
    ///   `dispatching`, apply `finish(Abstained{InterruptedBeforeDecision},
    ///   Some(Dispatching), None, …)` — the row's honest certainty is
    ///   `unknown` (OQ-13).
    ///
    /// Per-subject applies — one bad row drops only its own mark. The
    /// [r2] outbox `Unconfirmed` resolution composes in here with C3's
    /// `linked_outbox_resolution` helper (it is not a separate pass).
    pub(super) fn mark_restart(&mut self, now: Timestamp) -> Result<Marks, DaemonError> {
        let dispatching = self.store.effects_in_state(EffectState::Dispatching)?;
        let mut marks = Marks {
            effects: dispatching.len(),
            ..Marks::default()
        };
        let mut runs = BTreeSet::<RunId>::new();
        let mut evals = BTreeSet::<LaunchId>::new();
        for effect in &dispatching {
            if let Some(run) = &effect.subject_run {
                runs.insert(run.clone());
            }
            if effect.kind == EffectKind::JevEvaluate
                && let Some(launch) = &effect.subject_launch
            {
                evals.insert(launch.clone());
            }
        }
        // Disjoint field borrows: the store mutates, the policy only reads.
        let (store, policy) = (&mut self.store, &self.loaded.config.policy);
        for run_id in runs {
            match apply_with_retry(store, now, |st| {
                let Some(run) = st.run(&run_id).ok().flatten() else {
                    return empty();
                };
                let journal = st.journal(&run_id).unwrap_or_default();
                transition(
                    &run,
                    &versioned(&run, Event::Restart),
                    now,
                    policy,
                    (None, journal.as_slice(), &[]),
                    "",
                )
            })? {
                ApplyOutcome::Applied { .. } => marks.runs = marks.runs.saturating_add(1),
                ApplyOutcome::Dropped { .. } => {}
            }
        }
        for launch_id in evals {
            match apply_with_retry(store, now, |st| {
                let Some(launch) = st.launch(&launch_id).ok().flatten() else {
                    return empty();
                };
                if launch.phase != LaunchPhase::Evaluating {
                    return empty();
                }
                finish(
                    &launch,
                    LaunchOutcome::Abstained {
                        reason: AbstainReason::InterruptedBeforeDecision,
                    },
                    Some(EffectState::Dispatching),
                    None,
                    now,
                    policy,
                )
            })? {
                ApplyOutcome::Applied { .. } => marks.evals = marks.evals.saturating_add(1),
                ApplyOutcome::Dropped { .. } => {}
            }
        }
        log::restart_marks(marks.effects, marks.runs, marks.evals);
        Ok(marks)
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
                        Some(msg) => self.handle(msg).await,
                        None => break self.begin_shutdown(Stop::Requested),
                    }
                }
            }
        };
        (self, stop)
    }

    /// §4.14 step 1 — stop admission: set the watch *before* anything else
    /// so a runner's pre-wire gate sees it; in-flight effects are B1's
    /// concern (A1 runs none).
    fn begin_shutdown(&mut self, stop: Stop) -> Stop {
        let _unused = self.shutdown_watch.send(true);
        stop
    }

    /// One `Msg` to completion.
    async fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Tool { reply, .. } => {
                let _unused = reply.send(Err(ToolError::new(
                    ToolError::DAEMON_UNAVAILABLE,
                    "tool surface is not wired in this build",
                )));
            }
            Msg::Tick { snapshot } => {
                let answered = snapshot.is_ok();
                let panes = snapshot.map_or(0, |observed| observed.value.panes.len());
                log::tick(answered, panes);
            }
            Msg::Signal(Signal::Reload) => self.reload().await,
            Msg::Signal(Signal::Shutdown) => {
                // Handled in `serve`'s arm — unreachable here.
            }
        }
    }

    /// `SIGHUP` — `config::reload` against the last-good; on `Adopted` the
    /// `[daemon]` table must still be present (a daemonless reload would
    /// silently strand the daemon, so it retains instead).
    async fn reload(&mut self) {
        match config::reload(&self.loaded, &self.catalog_path).await {
            config::ReloadOutcome::Adopted(loaded) => match loaded.daemon.clone() {
                Some(daemon) => {
                    let settings_changed = self.daemon != daemon;
                    log::config_adopted(&loaded.version.0);
                    self.daemon = daemon;
                    self.loaded = *loaded;
                    if settings_changed {
                        // The spawned tasks captured their intervals at
                        // startup; a `[daemon]` change takes effect at
                        // the next restart (documented limitation).
                        log::config_retained("daemon-settings-changed");
                    }
                }
                None => log::config_retained("missing-daemon"),
            },
            config::ReloadOutcome::Retained { error, .. } => {
                log::config_retained(match error {
                    ConfigLoadError::Read(_) => "read",
                    ConfigLoadError::Decode(_) => "decode",
                    ConfigLoadError::Invalid(_) => "invalid",
                });
            }
        }
    }
}

/// The `Versioned` stamp a `Restart` event rides — `on_restart` applies
/// unconditionally (the staleness test exempts it), so the triple is the
/// Run's current versions.
fn versioned(run: &lifecycle::Run, event: Event) -> Versioned<Event> {
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
fn empty() -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::new(),
    }
}
