//! F23/F25 — supervision: the periodic review ask, the `unique`/`blocked`
//! observation lanes (idle episodes, the one nudge per episode) and the
//! supervision mapping applied to answered review sets.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, NativeSession, PaneId};
use crate::routing::{Judgment, JudgmentOutcome, JudgmentRecord, Question, noul_yes};

use super::{
    Effect, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectTarget, Run, Settlement,
    State, Timestamp, Transition, deadline_after, effect_key, journaled, mailbox_event, nothing,
    planned_effect, settle, update_if_changed, write_run,
};

/// The fields a `unique` observation refreshes: the last seen status, the
/// current locator (a move is followed — H#75) and the native session once
/// Herdr reports it (F2).
fn observe_fields(
    record: &mut Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) {
    record.child_status = status;
    if let Some(identity) = &mut record.identity {
        identity.pane_id = pane.clone();
        if let Some(session) = native_session {
            identity.native_session = Some(session.clone());
        }
    }
}

/// F23/F21 — a noul judgment clears `threshold` when its affirmative
/// probability reaches it. `Judgment.probabilities` carries the calibrated
/// distribution over the question's answer space; the Phase-4 contract is
/// that the Jev adapter maps a noul's wire `noul` scalar to P(yes) under the
/// `"yes"` key. The comparison itself is `routing::noul_yes` — one
/// threshold rule for routing and supervision, so a recorded `None`
/// resolves at the same calibrated majority there.
fn noul_cleared(judgment: &Judgment, threshold: Option<f64>) -> bool {
    judgment
        .probabilities
        .get("yes")
        .is_some_and(|p| noul_yes(*p, threshold))
}

/// F23 — the periodic review ask for a Run: one `jev_evaluate` per
/// `evidence_generation`, and only while the Run is `active`. Periodic
/// progress reviews pause while the owner's session is absent; acceptance
/// judgments and deadlines never pause (those run through `transition`,
/// which takes no owner-presence input). `Some` is the effect to plan.
#[must_use]
pub fn periodic_review(run: &Run, owner_absent: bool, journal: &[Effect]) -> Option<Effect> {
    if owner_absent || run.state != State::Active {
        return None;
    }
    // H#79 — unchanged evidence is never re-asked: the key carries the
    // generation, so an earlier ask (whatever its outcome) suppresses this
    // one.
    let key = effect_key(run, &format!("review:{}", run.evidence_generation));
    if journaled(journal, &key) {
        return None;
    }
    Some(planned_effect(run, EffectKind::JevEvaluate, key, None))
}

pub(super) fn on_unique(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    match run.state {
        // No captured identity can produce a unique observation yet (F2/F3),
        // and `settled` answers nothing.
        State::Reserved | State::Starting | State::Settled => nothing(),
        State::Prompting | State::Judging | State::Repair => {
            locate(run, status, pane, native_session)
        }
        State::Active => match status {
            Some(ChildStatus::Working) => work_resumed(run, status, pane, native_session),
            Some(ChildStatus::Idle | ChildStatus::Done) => {
                idle_observed(run, status, pane, native_session, env.0, env.1)
            }
            Some(ChildStatus::Blocked) => {
                blocked_observed(run, status, pane, native_session, journal)
            }
            None => locate(run, status, pane, native_session),
        },
    }
}

/// A `unique` observation's field refresh — child status, locator, native
/// session — with no other consequence.
fn locate(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) -> Transition {
    update_if_changed(run, |next| {
        observe_fields(next, status, pane, native_session);
    })
}

/// `active` + `obs(working)` — the episode ends when the child works again
/// (F23); the next stall or idle opens a fresh one.
fn work_resumed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) -> Transition {
    update_if_changed(run, |next| {
        observe_fields(next, status, pane, native_session);
        if next.idle_since.is_some() || next.nudged_episode == Some(next.nudge_episode) {
            next.nudge_episode = next.nudge_episode.saturating_add(1);
            next.idle_since = None;
            next.idle_deadline = None;
        }
    })
}

/// `active` + `obs(idle|done)` with no handoff — F25: the idle episode opens
/// at the observation (`idle_deadline` is the episode start + the policy
/// window) and the child gets the episode's one nudge (F23).
fn idle_observed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    let mut record = run.clone();
    observe_fields(&mut record, status, pane, native_session);
    if record.idle_since.is_none() {
        record.idle_since = Some(now);
        record.idle_deadline = Some(deadline_after(now, policy.idle_window));
    }
    let mut effects = Vec::new();
    if record.nudged_episode != Some(record.nudge_episode)
        && let Some(identity) = record.identity.clone()
    {
        effects.push(planned_effect(
            run,
            EffectKind::Prompt,
            effect_key(run, &format!("nudge:{}", run.nudge_episode)),
            Some(EffectTarget::Child(identity)),
        ));
        record.nudged_episode = Some(record.nudge_episode);
    }
    if record == *run {
        return nothing();
    }
    record.version = run.version.saturating_add(1);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects,
    }
}

/// `active` + `obs(blocked)` — F23: ask `blocked_on_input` and
/// `provider_limited` (the review set carries both). Never prompt a blocked
/// child — no `prompt` effect is ever planned here (F17).
fn blocked_observed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    journal: &[Effect],
) -> Transition {
    let mut record = run.clone();
    observe_fields(&mut record, status, pane, native_session);
    let review_key = effect_key(run, &format!("review:{}", run.evidence_generation));
    let mut effects = Vec::new();
    if !journaled(journal, &review_key) {
        effects.push(planned_effect(
            run,
            EffectKind::JevEvaluate,
            review_key,
            None,
        ));
    }
    let mut state_changes = Vec::new();
    if record != *run {
        record.version = run.version.saturating_add(1);
        state_changes.push(write_run(run, record));
    }
    Transition {
        state_changes,
        events: Vec::new(),
        effects,
    }
}

/// An answered judgment set → the F23 supervision mapping; anything else is
/// journal-only here (acceptance verdicts arrive as `judgment` events).
pub(super) fn review_result(
    run: &Run,
    result: &EffectResult,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    match (&result.receipt, result.outcome) {
        (Some(EffectReceipt::Judgments(record)), EffectOutcome::Acknowledged)
            if record.set.outcome == JudgmentOutcome::Answered =>
        {
            apply_review(run, record, now, policy)
        }
        (
            Some(
                EffectReceipt::Judgments(_)
                | EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None,
            _,
        ) => nothing(),
    }
}

/// F23 — the supervision answers of an answered review/provider-limit set,
/// applied while their versions still hold. `provider_limited` is terminal
/// and is checked before the advisory answers; `no_recent_progress` nudges
/// once per episode (the second stall in one episode reports `stalled`).
fn apply_review(run: &Run, record: &JudgmentRecord, now: Timestamp, policy: &Policy) -> Transition {
    for judgment in &record.judgments {
        if judgment.question == Question::ProviderLimited
            && noul_cleared(judgment, Some(policy.provider_limit_threshold))
        {
            return settle(run, Settlement::ProviderLimited, now, policy);
        }
    }
    let mut next = run.clone();
    let mut events = Vec::new();
    let mut effects = Vec::new();
    for judgment in &record.judgments {
        match judgment.question {
            Question::BlockedOnInput => {
                if noul_cleared(judgment, judgment.threshold) {
                    events.push(mailbox_event(
                        run,
                        MailboxEventKind::BlockedOnInput,
                        &format!("blocked_on_input:{}", run.evidence_generation),
                        String::from("{\"question\":\"blocked_on_input\"}"),
                    ));
                }
            }
            Question::OutsideScope => {
                if noul_cleared(judgment, judgment.threshold) {
                    events.push(mailbox_event(
                        run,
                        MailboxEventKind::OutsideScope,
                        &format!("outside_scope:{}", run.evidence_generation),
                        String::from("{\"question\":\"outside_scope\"}"),
                    ));
                }
            }
            Question::NoRecentProgress => {
                if noul_cleared(judgment, judgment.threshold) {
                    if next.nudged_episode == Some(next.nudge_episode) {
                        // stalled after the nudge — actionable (F18)
                        events.push(mailbox_event(
                            run,
                            MailboxEventKind::Stalled,
                            &format!("stalled:{}", run.nudge_episode),
                            String::from("{\"stalled\":true}"),
                        ));
                    } else if let Some(identity) = next.identity.clone() {
                        effects.push(planned_effect(
                            run,
                            EffectKind::Prompt,
                            effect_key(run, &format!("nudge:{}", run.nudge_episode)),
                            Some(EffectTarget::Child(identity)),
                        ));
                        next.nudged_episode = Some(next.nudge_episode);
                    } else {
                        // no captured identity: the episode's nudge stays
                        // unconsumed — there is no pane to send it to.
                    }
                }
            }
            Question::DoneWhenVerifiable
            | Question::WeakestSufficientTier
            | Question::ChangesFiles
            | Question::SecurityBoundary
            | Question::NeedsExternal
            | Question::LongRunning
            | Question::RelatedTab
            | Question::ProviderLimited
            | Question::HandoffMeetsItem { item: _ } => {}
        }
    }
    let mut state_changes = Vec::new();
    if next != *run {
        next.version = run.version.saturating_add(1);
        state_changes.push(write_run(run, next));
    }
    Transition {
        state_changes,
        events,
        effects,
    }
}
