//! Appendix C — the lifecycle transition vocabulary: `State`, `Event`,
//! `Settlement`, the version triple and deadline kinds, the F8 effect-journal
//! types, and the `Transition` value the transition function returns
//! (§9/F22), plus the total function itself: `transition`, `settle` and
//! `periodic_review` implement F20, F22, F23 and F25.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{DedupKey, EffectId, EffectKey, EventId, Timestamp};

mod change;
mod effect;
mod event;
mod record;
mod rules;
mod settle;
mod state;
mod supervision;
mod transition;

pub use change::{StateChange, Transition};
pub use effect::{
    Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState,
    EffectTarget, EffectWrite,
};
pub use event::{Event, JudgmentVerdict, VersionTriple, Versioned};
pub use record::{CreatedTopology, OwnerChange, Run, RunUpdate};
pub use rules::TRANSITION_RULES;
pub use settle::settle;
pub use state::{DeadlineKind, PromptCertainty, Settlement, State, UnresolvedReason};
pub use supervision::periodic_review;
pub use transition::transition;

/// An absolute deadline `window` after `at` (F22 — deadlines are stored
/// absolute and never reset). Saturating: a pathological window means
/// "effectively never" rather than wrapping into the past.
fn deadline_after(at: Timestamp, window: Duration) -> Timestamp {
    let millis = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
    Timestamp(at.0.saturating_add(millis))
}

/// The empty transition — losing transitions and no-op events commit nothing
/// (F20).
fn nothing() -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// Effect keys are `run:<id>:<suffix>` (Appendix B `effect_key` examples).
fn effect_key(run: &Run, suffix: &str) -> EffectKey {
    EffectKey(format!("run:{}:{}", run.id.0, suffix))
}

/// `true` when the journal already holds an effect under `key` — the unique
/// key is the dedup (N1): re-planning the same work names the same row.
fn journaled(journal: &[Effect], key: &EffectKey) -> bool {
    journal.iter().any(|effect| effect.key == *key)
}

/// Ids are derived deterministically from the unique key they belong to, so
/// a re-planned effect or re-emitted event names the same row.
fn planned_effect(
    run: &Run,
    kind: EffectKind,
    key: EffectKey,
    target: Option<EffectTarget>,
) -> Effect {
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind,
        subject_launch: Some(run.launch.clone()),
        subject_run: Some(run.id.clone()),
        target,
        payload_digest: None,
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// One mailbox event for this Run: `dedup_key` is `run:<id>:<suffix>` (F18 —
/// stable keys make repeats no-ops).
fn mailbox_event(run: &Run, kind: MailboxEventKind, suffix: &str, body: String) -> MailboxEvent {
    let dedup_key = DedupKey(format!("run:{}:{}", run.id.0, suffix));
    MailboxEvent {
        id: EventId(format!("evt:{}", dedup_key.0)),
        dedup_key,
        subject: MailboxSubject::Run(run.id.clone()),
        kind,
        body,
    }
}

/// The conditional run write (Appendix B): `apply` asserts
/// `version = expected_version` — and `settlement IS NULL` when the record
/// settles — then stores `record`.
fn write_run(run: &Run, record: Run) -> StateChange {
    StateChange::UpdateRun(RunUpdate {
        expected_version: run.version,
        record,
    })
}

/// `run` with `edit` applied and `version` bumped — every row write bumps it
/// (Appendix B `runs.version`).
fn edited(run: &Run, edit: impl FnOnce(&mut Run)) -> Run {
    let mut next = run.clone();
    edit(&mut next);
    next.version = run.version.saturating_add(1);
    next
}

/// An `UpdateRun` transition when `edit` changed the row, nothing when it
/// did not — a no-change observation must not bump `version`, or every
/// reconcile read would invalidate in-flight stamped results (F20).
fn update_if_changed(run: &Run, edit: impl FnOnce(&mut Run)) -> Transition {
    let mut record = run.clone();
    edit(&mut record);
    if record == *run {
        return nothing();
    }
    record.version = run.version.saturating_add(1);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
