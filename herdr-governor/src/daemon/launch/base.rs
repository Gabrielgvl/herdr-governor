//! `launch/base` — the F6 `base_commit` a Launch's `decided` pins. The
//! connection task probes it before admission (`ToolPrepared`); nothing
//! durable names it until the Run is reserved, so the coordinator holds
//! it in memory. A restart between admission and `decided` loses that
//! value — the convergence rows (`eval planned` → dispatch, `eval
//! acknowledged` → re-run `on_evaluated`) would otherwise reserve the
//! Run with a silent `None` and drop its git evidence. Instead the
//! missing pin is re-taken once by a spawned probe (the coordinator
//! performs no I/O): the Run's base is then HEAD at decision time, which
//! is still before any child exists — the same pre-topology point the
//! admission probe pins. A re-probe that fails abstains the Launch
//! `interrupted_before_decision`: the restart interrupted it before a
//! decision could be made on honest evidence.

use governor_core::identity::LaunchId;
use governor_core::task::{AbstainReason, Launch, LaunchPhase};

use crate::adapters::git::GitError;
use crate::daemon::api::probe_base_commit;
use crate::daemon::coordinator::{Coordinator, Msg};
use crate::daemon::log;

/// One Launch's base-commit pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::daemon) enum BasePin {
    /// The probe's verdict: `Some(head)`, or the legal plain-directory
    /// `None`.
    Pinned(Option<String>),
    /// A restart lost the admission pin; the re-probe is in flight.
    Probing,
}

impl Coordinator {
    /// The pin for `launch` — `Probing` while a re-probe runs, started
    /// here, once, when no pin exists (a restart lost it).
    pub(super) fn base_for(&mut self, launch: &Launch) -> BasePin {
        if let Some(pin) = self.launch_bases.get(&launch.id) {
            return pin.clone();
        }
        // Without the runner env there is no mailbox to answer on (unit
        // tests drive the coordinator bare) — the Launch waits.
        let Some(env) = self.runner.as_ref() else {
            return BasePin::Probing;
        };
        let tx = env.tx.clone();
        self.launch_bases
            .insert(launch.id.clone(), BasePin::Probing);
        let cwd = launch
            .task
            .cwd
            .clone()
            .unwrap_or_else(|| launch.project_root.0.clone());
        let id = launch.id.clone();
        drop(tokio::spawn(async move {
            let base = probe_base_commit(std::path::Path::new(&cwd)).await;
            let _closed = tx.send(Msg::LaunchBase { launch: id, base }).await;
        }));
        BasePin::Probing
    }

    /// The `LaunchBase` arm: pin the verdict and resume the decision, or
    /// abstain on a failed probe. A Launch that left `evaluating` while
    /// the probe ran takes nothing (its pin was already cleared).
    pub(in crate::daemon) fn on_launch_base(
        &mut self,
        launch_id: &LaunchId,
        verdict: Result<Option<String>, GitError>,
    ) {
        let evaluating = self
            .store
            .launch(launch_id)
            .ok()
            .flatten()
            .is_some_and(|launch| launch.phase == LaunchPhase::Evaluating);
        if !evaluating {
            self.launch_bases.remove(launch_id);
            return;
        }
        match verdict {
            Ok(pinned) => {
                self.launch_bases
                    .insert(launch_id.clone(), BasePin::Pinned(pinned));
                self.on_evaluated(launch_id);
            }
            Err(_error) => {
                log::base_reprobe_failed();
                self.eval_abstain(launch_id, AbstainReason::InterruptedBeforeDecision);
            }
        }
    }
}
