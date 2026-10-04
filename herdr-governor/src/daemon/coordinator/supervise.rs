//! `coordinator::supervise` — §4.7 step 4 on the coordinator (C3): the
//! tick decides which supervised Runs need evidence and spawns one
//! `evidence::gather` per Run (async I/O off the coordinator, posting
//! `Msg::Evidence` back); the `Msg::Evidence` arm absorbs the result into
//! the Run's `EvidenceTail`, applies `Event::Evidence` + the periodic
//! review, and stamps the bundle the asks render from. The acceptance
//! retry rides the same tick.

use governor_core::identity::{RunId, Timestamp};
use governor_core::lifecycle::{LimitRecordKey, Run};

use crate::adapters::transcript::{SessionPointer, TranscriptRoots, devin_log_default_dir};
use crate::daemon::evidence::{self, GatherRequest, Gathered};
use crate::daemon::reconcile::SnapshotView;
use crate::daemon::{log, supervision};

use super::{Coordinator, Msg};

impl Coordinator {
    /// The tick's step 4: drop the evidence of Runs no longer supervised,
    /// spawn a gather for every supervised Run whose bundle is missing,
    /// stale or older than `review_interval`, then retry acceptance asks.
    pub(super) fn supervise(&mut self, view: Option<&SnapshotView>) {
        let now = self.clock.now();
        let runs: Vec<Run> = self
            .store
            .unsettled_runs()
            .unwrap_or_default()
            .into_iter()
            .filter(|run| supervision::supervised(run.state))
            .collect();
        self.evidence
            .retain(|id, _| runs.iter().any(|run| run.id == *id));
        let roots = self.roots();
        if let Some(env) = self.runner.clone() {
            for run in &runs {
                let entry = self.evidence.entry(run.id.clone()).or_default();
                if !entry.needs_gather(run, now, self.daemon.review_interval) {
                    continue;
                }
                let Some(identity) = run.identity.as_ref() else {
                    continue;
                };
                let request = GatherRequest {
                    run: run.id.clone(),
                    pointer: identity.native_session.as_ref().map(|session| {
                        SessionPointer::new(&identity.agent_kind.0, &session.0, Some(&run.cwd))
                    }),
                    roots: roots.clone(),
                    tail: entry.checkout(),
                    pane: identity.pane_id.clone(),
                    cwd: run.cwd.clone(),
                    with_git: run.base_commit.is_some(),
                    herdr: (env.herdr.clone(), env.herdr_op),
                    cycle_start: self.cycle_start(&run.id),
                    log_dir: self.log_dir(),
                    owner_absent: view.is_some_and(|v| owner_absent(run, v)),
                };
                let tx = env.tx.clone();
                drop(tokio::spawn(async move {
                    let gathered = evidence::gather(request).await;
                    let _closed = tx.send(Msg::Evidence(Box::new(gathered))).await;
                }));
            }
        }
        if let Err(error) = supervision::retry_acceptance(&mut self.store, now) {
            log::apply_dropped(1, super::tick::kind_of(&error));
        }
    }

    /// `Msg::Evidence` — absorb the gather, apply `Event::Evidence` and
    /// the periodic review, then stamp the bundle with the Run's
    /// resulting generation (an ask renders only from a stamped bundle).
    pub(super) fn on_evidence(&mut self, gathered: Gathered) {
        let now = self.clock.now();
        let run_id: RunId = gathered.run.clone();
        let owner_absent = gathered.owner_absent;
        let Some(entry) = self.evidence.get_mut(&run_id) else {
            return;
        };
        let digest = entry.absorb(gathered, now);
        let policy = &self.loaded.config.policy;
        if let Err(error) = supervision::apply_evidence(
            &mut self.store,
            policy,
            now,
            &run_id,
            (digest, owner_absent),
        ) {
            log::apply_dropped(1, super::tick::kind_of(&error));
        }
        // F31 — the pass's typed provider-limit record plans its own
        // once-per-lifetime ask (`limit:<record_id>`); a `blocked:<ep>`
        // ask already in flight carries it and `limit_observed`
        // suppresses this one — one ask per pass, never a double.
        if let Some(record) = self
            .evidence
            .get(&run_id)
            .and_then(|tail| tail.latest_limit())
        {
            let key = LimitRecordKey(record.record_id());
            if let Err(error) = supervision::plan_limit(&mut self.store, now, &run_id, &key) {
                log::apply_dropped(1, super::tick::kind_of(&error));
            }
        }
        if let (Some(tail), Ok(Some(run))) =
            (self.evidence.get_mut(&run_id), self.store.run(&run_id))
        {
            tail.stamp(&run);
        }
    }

    /// F31/F34 — the limit probe's anchor: the task prompt's
    /// `dispatched_at`, the first-cycle boundary (`run:<id>:prompt:task`
    /// — nudges and follow-ups never move it). `None` — the probe fails
    /// closed — when the row or its dispatch stamp is absent.
    fn cycle_start(&self, run: &RunId) -> Option<Timestamp> {
        let task = format!("run:{}:prompt:task", run.0);
        self.store
            .journal(run)
            .ok()?
            .iter()
            .find(|effect| effect.key.0 == task)
            .and_then(|effect| effect.dispatched_at)
    }

    /// The `[daemon] devin_log_dir` override, else the harness data dir.
    fn log_dir(&self) -> Option<std::path::PathBuf> {
        self.daemon
            .devin_log_dir
            .clone()
            .or_else(devin_log_default_dir)
    }

    /// The transcript roots: `[daemon]`'s overrides, else the harnesses'
    /// env-derived locations.
    fn roots(&self) -> TranscriptRoots {
        let derived = TranscriptRoots::from_env();
        TranscriptRoots::new(
            self.daemon
                .transcript_data_dirs
                .clone()
                .unwrap_or(derived.data_dirs),
            self.daemon
                .transcript_project_dirs
                .clone()
                .unwrap_or(derived.project_dirs),
        )
    }
}

/// F23's pause: the owner's `(agent_kind, native_session)` holds no agent
/// row in the tick's snapshot. An unavailable snapshot proves nothing, so
/// the caller passes `false` without a view.
fn owner_absent(run: &Run, view: &SnapshotView) -> bool {
    !view.agents().iter().any(|row| {
        row.2.as_ref() == Some(&run.owner.agent_kind)
            && row.4.as_ref() == Some(&run.owner.native_session)
    })
}
