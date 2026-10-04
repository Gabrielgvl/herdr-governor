//! `effects::gate` — the `DispatchCommit` revalidation (§4.4 step 3):
//! every gate the commit consults, each against a fresh durable read —
//! the row state, the subject's liveness, the kind gates (`prompt`'s
//! ordering barrier, F15's candidate/provider/qualification recheck, the
//! run-bound ask generation check), then the `op_digest` audit and the
//! frozen-file audits. Split from `effects.rs` under the 500-line cap.

use governor_core::config::{Capability, args_digest};
use governor_core::delivery::prompt_dispatchable;
use governor_core::identity::{Digest, EffectKey, Timestamp};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectResolution, EffectState, EffectTarget, FailureCause,
    State, op_digest,
};
use governor_core::routing::{Candidate, cooling_down};
use governor_core::task::LaunchPhase;
use sha2::Digest as _;

use crate::daemon::runner;
use crate::daemon::supervision::ask_current;
use crate::daemon::{FileRef, RenderContext};

use super::Coordinator;

/// The revalidation's answer (§4.4 step 3): `Go` commits the dispatch
/// write, `Skip` leaves the row `planned`, `Refuse` commits the
/// governor-refused result in the same transaction as the dispatch.
pub(super) enum Gate {
    /// Every gate passed — commit `[WriteEffect::Dispatch]`.
    Go,
    /// The effect no longer qualifies — the row stays `planned`.
    Skip,
    /// The coordinator refuses on the subject's behalf: the composed
    /// `[Dispatch] + EffectResult` apply journals `failed`/`absent` (or
    /// `pre_interactive` for the F15 recheck) atomically. Boxed — the
    /// resolution dwarfs the other verdicts.
    Refuse(Box<EffectResolution>),
}

impl Coordinator {
    /// §4.4 step 3's revalidation — every gate the commit consults, each
    /// against a fresh durable read. Order is fixed: the row, the subject,
    /// the kind gate, the `op_digest` audit, the frozen-file audits.
    pub(super) fn commit_gate(
        &self,
        key: &EffectKey,
        context: &RenderContext,
        now: Timestamp,
    ) -> Gate {
        let Ok(Some(effect)) = self.store.effect(key) else {
            return Gate::Skip;
        };
        if effect.state != EffectState::Planned {
            return Gate::Skip;
        }
        // The subject gates mirror READY_EFFECTS' exclusions — a
        // `settled` Run, or a launch-bound effect's `done` Launch, can no
        // longer dispatch (F10's defense-in-depth); `close` and `event:`
        // hints are exempt by design. A Run-bound effect answers to its
        // Run alone: a `launched` Launch is `done` for the Run's life.
        let subject = match &effect.subject_run {
            Some(run_id) => match self.store.run(run_id) {
                Ok(Some(run)) => Some(run),
                _ => return Gate::Skip,
            },
            None => None,
        };
        let hint = effect.key.0.starts_with("event:");
        if let Some(run) = &subject
            && run.state == State::Settled
            && effect.kind != EffectKind::Close
            && !hint
        {
            return Gate::Skip;
        }
        if let Some(launch_id) = &effect.subject_launch
            && subject.is_none()
        {
            let Ok(Some(launch)) = self.store.launch(launch_id) else {
                return Gate::Skip;
            };
            if launch.phase == LaunchPhase::Done && !hint {
                return Gate::Skip;
            }
        }

        // The kind gates — each kind's live eligibility, revalidated.
        let mut digest_params: Option<Vec<u8>> = None;
        match effect.kind {
            EffectKind::Prompt => {
                if matches!(effect.target, Some(EffectTarget::Child(_))) {
                    let Some(run) = &subject else {
                        return Gate::Skip;
                    };
                    let outbox = self.store.outbox(&run.id).unwrap_or_default();
                    let journal = self.store.journal(&run.id).unwrap_or_default();
                    if !prompt_dispatchable(run, &outbox, &journal, &effect.key) {
                        return Gate::Skip;
                    }
                }
                digest_params = Some(effect.key.0.clone().into_bytes());
            }
            EffectKind::AgentStart => {
                // The pipeline leg exists only while the Run is still
                // `starting`; a state that moved on skips (never refuses —
                // the start may yet matter to no one).
                if subject.is_none_or(|run| run.state != State::Starting) {
                    return Gate::Skip;
                }
                match self.agent_start_gate(&effect, now) {
                    Ok(candidate) => {
                        digest_params = Some(
                            args_digest(candidate.args.iter().map(String::as_str))
                                .0
                                .to_vec(),
                        );
                    }
                    Err(cause) => return Gate::Refuse(Box::new(pre_interactive(cause))),
                }
            }
            EffectKind::TabCreate | EffectKind::PaneSplit => {
                if subject.is_none_or(|run| run.state != State::Starting) {
                    return Gate::Skip;
                }
                digest_params = Some(Vec::new());
            }
            EffectKind::Close => {
                digest_params = Some(Vec::new());
            }
            EffectKind::JevEvaluate => {
                // A run-bound ask is stale the moment a generation moved —
                // the ask name carries the generations it was minted under.
                if let Some(run) = &subject
                    && !ask_current(run, &effect.key)
                {
                    return Gate::Skip;
                }
            }
        }

        // The F8/`op_digest` audit — the row's recorded digest must equal
        // the descriptor recomputed from durable state. `params` is `None`
        // only for `jev_evaluate` (its honest NULL) and a `start:` whose
        // candidate no longer exists — the latter never reaches here (the
        // F15 gate refused it already).
        let expected = digest_params
            .map(|params| op_digest(effect.kind, effect.target.as_ref(), params.as_slice()));
        if expected != effect.payload_digest {
            return Gate::Refuse(Box::new(failed_absent("payload_digest_mismatch")));
        }

        // The frozen-file audits — a file the context commits to must
        // still carry the size+digest the coordinator recorded at
        // hand-off.
        for file in context.files() {
            if !file_intact(file) {
                return Gate::Refuse(Box::new(failed_absent("frozen_digest_mismatch")));
            }
        }
        Gate::Go
    }

    /// F15's recheck: candidate `i` from `start:<i>` in the persisted
    /// decision must still name a candidate whose provider is not cooling
    /// and whose `(operating_point, args_digest(candidate.args))` `start`
    /// qualification still reads `passed`. The refusals are the
    /// governor's own — `PreInteractiveFailed`, so the result lane plans
    /// the next candidate in the same transaction.
    fn agent_start_gate(&self, effect: &Effect, now: Timestamp) -> Result<Candidate, &'static str> {
        let suffix = runner::seam::suffix_of(&effect.key);
        let index = suffix
            .strip_prefix("start:")
            .and_then(|digits| digits.parse::<usize>().ok())
            .ok_or("start_key_malformed")?;
        let run_id = effect.subject_run.as_ref().ok_or("subject_absent")?;
        let run = self
            .store
            .run(run_id)
            .ok()
            .flatten()
            .ok_or("subject_absent")?;
        let launch = self
            .store
            .launch(&run.launch)
            .ok()
            .flatten()
            .ok_or("launch_absent")?;
        let candidate = launch
            .decision
            .as_ref()
            .ok_or("decision_absent")?
            .candidates
            .get(index)
            .ok_or("candidate_absent")?
            .clone();
        if cooling_down(&self.store.cooldowns().unwrap_or_default(), now)
            .contains(&candidate.provider)
        {
            return Err("provider_cooling_down");
        }
        let passed = self
            .store
            .qualification(
                &candidate.operating_point,
                args_digest(candidate.args.iter().map(String::as_str)),
                &Capability(Capability::START.into()),
            )
            .ok()
            .flatten()
            .is_some_and(|qualification| qualification.passed);
        if !passed {
            return Err("qualification_lapsed");
        }
        Ok(candidate)
    }
}

/// Whether the frozen file the context commits to still holds its
/// recorded bytes: size first (a cheap refuse), then the full sha256 —
/// the pointer the wire sends must name exactly those bytes.
fn file_intact(file: &FileRef) -> bool {
    let Ok(metadata) = std::fs::metadata(&file.path) else {
        return false;
    };
    if metadata.len() != file.size {
        return false;
    }
    let Ok(bytes) = std::fs::read(&file.path) else {
        return false;
    };
    Digest(sha2::Sha256::digest(&bytes).into()) == file.digest
}

/// `PreInteractiveFailed` — the F15 refusal class: provably never ran.
fn pre_interactive(cause: &'static str) -> EffectResolution {
    EffectResolution::PreInteractiveFailed {
        cause: Some(FailureCause(cause.into())),
    }
}

/// `Failed{Absent}` — the audit refusal class: nothing was written.
fn failed_absent(cause: &'static str) -> EffectResolution {
    EffectResolution::Failed {
        certainty: EffectCertainty::Absent,
        cause: Some(FailureCause(cause.into())),
    }
}
