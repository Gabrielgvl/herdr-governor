//! `supervision` — §4.9's Run-bound Jev asks (F23/F24): the
//! `RenderContext` each ask family renders at hand-off (`review:<eg>`,
//! `blocked:<episode>`, `accept:<wg>:<eg>`) from the Run's stamped
//! evidence bundle, the evidence apply (`Event::Evidence` then
//! `periodic_review`), and the per-tick `acceptance_retry`. The answered
//! receipts flow through the effect-result arm unchanged — `apply_review`
//! for reviews, `acceptance_verdict` + `Event::Judgment` for acceptance
//! (B1's lift) — so a `too_large` acceptance attempt ends its family
//! there and the wait falls to `judgment_deadline` (OQ-I, → F30).

use std::collections::BTreeMap;

use governor_core::config::Policy;
use governor_core::identity::{Digest, EffectKey, JudgmentSetId, RunId, Timestamp};
use governor_core::lifecycle::{
    Effect, EffectKind, Event, Run, State, Transition, VersionTriple, acceptance_retry,
    periodic_review, transition,
};
use governor_core::routing::{JudgmentOutcome, JudgmentPurpose, JudgmentSet, QuestionVersion};
use sha2::Digest as _;

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::adapters::jev::{
    AcceptanceState, BlockedState, QuestionSpec, ReviewState, State as JevState, TaskDigest,
};
use crate::store::Store;

use super::coordinator::apply::apply_with_retry;
use super::coordinator::{empty, versioned};
use super::evidence::EvidenceTail;
use super::runner::seam::suffix_of;
use super::{DaemonError, FileRef, RenderContext, questions};

/// The states the evidence pass serves — the child is working on, or
/// being judged for, the task.
pub(super) fn supervised(state: State) -> bool {
    matches!(state, State::Active | State::Judging | State::Repair)
}

/// A Run-bound ask's generation check: the ask key's `family:gens` must
/// still match the Run's live numbers — `accept:<wg>:<eg>`, `review:<eg>`,
/// `blocked:<ep>`, `limit:<wg>` (attempt suffixes are ignored). The
/// commit gate and the context builder share it: a stale ask never
/// renders, so it never spins the hand-off.
pub(super) fn ask_current(run: &Run, key: &EffectKey) -> bool {
    let suffix = suffix_of(key);
    let mut parts = suffix.split(':');
    let family = parts.next();
    let mut number = || parts.next().and_then(|part| part.parse::<u64>().ok());
    match family {
        Some("accept") => {
            number() == Some(run.work_generation) && number() == Some(run.evidence_generation)
        }
        Some("review") => number() == Some(run.evidence_generation),
        Some("blocked") => number() == Some(run.blocked_episode),
        Some("limit") => number() == Some(run.work_generation),
        _ => false,
    }
}

/// The values every ask context reads beyond the store.
pub(super) struct AskEnv<'a> {
    /// The live catalog (policy threshold, `policy_version`).
    pub loaded: &'a LoadedConfig,
    /// `[daemon]` (`jev_model`).
    pub daemon: &'a DaemonSettings,
    /// The per-Run evidence the coordinator holds.
    pub evidence: &'a BTreeMap<RunId, EvidenceTail>,
}

/// The `RenderContext` for a Run-bound `jev_evaluate` — `None` for every
/// other effect, and for an ask that cannot honestly render yet: stale
/// (its generation moved), no stamped evidence bundle (the next pass
/// gathers it), or an acceptance ask whose frozen copy does not hash to
/// the digest under judgment. `None` leaves the row `planned`.
pub(super) fn ask_context(
    store: &Store,
    env: &AskEnv<'_>,
    effect: &Effect,
) -> Option<RenderContext> {
    if effect.kind != EffectKind::JevEvaluate {
        return None;
    }
    let run = store.run(effect.subject_run.as_ref()?).ok().flatten()?;
    if !supervised(run.state) || !ask_current(&run, &effect.key) {
        return None;
    }
    let launch = store.launch(&run.launch).ok().flatten()?;
    let bundle = env.evidence.get(&run.id)?.bundle_for(&run)?;
    let task = TaskDigest::from(&launch.task);
    let policy = &env.loaded.config.policy;
    let (state, questions, purpose, frozen): (_, Vec<QuestionSpec>, _, _) =
        match suffix_of(&effect.key).split(':').next()? {
            "review" => (
                JevState::Review(ReviewState {
                    task,
                    scope: launch.task.scope.clone(),
                    transcript: bundle.transcript.clone(),
                    terminal: bundle.terminal.clone(),
                    git: bundle.git.clone(),
                }),
                questions::review_specs(),
                JudgmentPurpose::Review,
                None,
            ),
            "blocked" => (
                JevState::Blocked(BlockedState {
                    task,
                    transcript: bundle.transcript.clone(),
                    terminal: bundle.terminal.clone(),
                    git: bundle.git.clone(),
                    limit_record: None,
                }),
                questions::blocked_specs(policy.provider_limit_threshold),
                JudgmentPurpose::ProviderLimit,
                None,
            ),
            "accept" => {
                let (file, text) = frozen_handoff(store, &run)?;
                (
                    JevState::Acceptance(AcceptanceState {
                        task,
                        handoff: text,
                        transcript: bundle.transcript.clone(),
                        terminal: bundle.terminal.clone(),
                        git: bundle.git.clone(),
                    }),
                    questions::acceptance_specs(&launch.task.done_when),
                    JudgmentPurpose::Acceptance,
                    Some(file),
                )
            }
            _ => return None,
        };
    let set = JudgmentSet {
        id: JudgmentSetId(format!("jset:{}", effect.key.0)),
        purpose,
        launch: None,
        run: Some(run.id.clone()),
        versions: Some(VersionTriple {
            version: run.version,
            work_generation: run.work_generation,
            evidence_generation: run.evidence_generation,
        }),
        task_digest: launch.task_digest,
        handoff_digest: frozen.as_ref().map(|file: &FileRef| file.digest),
        evidence_digest: Some(bundle.digest),
        model: String::new(),
        question_version: QuestionVersion(questions::SUPERVISION_QUESTION_VERSION.into()),
        policy_version: env.loaded.version.clone(),
        // Placeholder until the wire stamps the honest outcome.
        outcome: JudgmentOutcome::Stale,
    };
    Some(RenderContext::Jev {
        model: env.daemon.jev_model.clone(),
        state,
        questions,
        set: Box::new(set),
        frozen,
    })
}

/// The frozen copy under judgment: the `handoffs` row at the Run's
/// `work_generation` whose digest is `judging_digest`, read whole and
/// re-hashed — the `FileRef` the commit gate re-verifies, and the text
/// the acceptance state carries.
fn frozen_handoff(store: &Store, run: &Run) -> Option<(FileRef, String)> {
    let digest = run.judging_digest?;
    let row = store
        .handoffs(&run.id)
        .ok()?
        .into_iter()
        .find(|h| h.work_generation == run.work_generation && h.digest == digest)?;
    let bytes = std::fs::read(&row.frozen_path).ok()?;
    if Digest(sha2::Sha256::digest(&bytes).into()) != digest {
        return None;
    }
    let size = u64::try_from(bytes.len()).ok()?;
    Some((
        FileRef {
            path: row.frozen_path,
            size,
            digest,
        },
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

/// §4.7 step 4's apply: `Event::Evidence{digest}` against the Run's
/// current versions (an unchanged digest is a no-op; a change bumps the
/// generation, and in `judging` re-keys the acceptance ask), then
/// `periodic_review` on the result — one review per generation, paused
/// while the owner is absent (F23). Two applies, each against a fresh
/// read.
pub(super) fn apply_evidence(
    store: &mut Store,
    policy: &Policy,
    now: Timestamp,
    run_id: &RunId,
    (digest, owner_absent): (Digest, bool),
) -> Result<(), DaemonError> {
    apply_with_retry(store, now, |st| {
        let Some(run) = st.run(run_id).ok().flatten() else {
            return empty();
        };
        if !supervised(run.state) {
            return empty();
        }
        let journal = st.journal(run_id).unwrap_or_default();
        let handoffs = st.handoffs(run_id).unwrap_or_default();
        transition(
            &run,
            &versioned(&run, Event::Evidence { digest }),
            now,
            policy,
            (None, journal.as_slice(), handoffs.as_slice()),
            "",
        )
    })?;
    plan_with(store, now, run_id, |run, journal| {
        periodic_review(run, owner_absent, journal)
    })
}

/// F24's re-ask rule, once per tick for every `judging` Run: a failed or
/// unconfirmed acceptance attempt re-plans under the next `:<n>` key; an
/// answered, in-flight or `too_large` family plans nothing.
pub(super) fn retry_acceptance(store: &mut Store, now: Timestamp) -> Result<(), DaemonError> {
    for run in store.unsettled_runs()? {
        if run.state == State::Judging {
            plan_with(store, now, &run.id, acceptance_retry)?;
        }
    }
    Ok(())
}

/// Apply the one effect `plan` derives from the Run's fresh row and
/// journal, if any.
fn plan_with(
    store: &mut Store,
    now: Timestamp,
    run_id: &RunId,
    plan: impl Fn(&Run, &[Effect]) -> Option<Effect>,
) -> Result<(), DaemonError> {
    apply_with_retry(store, now, |st| {
        let Some(run) = st.run(run_id).ok().flatten() else {
            return empty();
        };
        let journal = st.journal(run_id).unwrap_or_default();
        plan(&run, &journal).map_or_else(empty, |effect| Transition {
            state_changes: Vec::new(),
            events: Vec::new(),
            effects: Vec::from([effect]),
        })
    })?;
    Ok(())
}
