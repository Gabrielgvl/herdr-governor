//! F23/F25 — supervision: the periodic review ask, the `unique`/`blocked`
//! observation lanes (idle episodes, the one nudge per episode) and the
//! supervision mapping applied to answered review sets.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, EffectKey, NativeSession, PaneId};
use crate::routing::{Judgment, JudgmentOutcome, JudgmentRecord, Question, noul_yes};

use super::{
    Effect, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState, EffectTarget, Run,
    Settlement, State, Timestamp, Transition, effect_key, mailbox_event, nothing, op_digest,
    planned_effect, settle, update_if_changed, write_run,
};

/// The fields a `unique` observation refreshes: the last seen status, the
/// current locator (a move is followed — H#75) and the native session once
/// Herdr reports it (F2). A `blocked` report after a non-blocked one opens
/// the next blocked episode — `blocked_episode+1` (F23); an observation
/// reporting `working`, `idle`, `done` or unreadable ends it.
fn observe_fields(
    record: &mut Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
) {
    if status == Some(ChildStatus::Blocked) && record.child_status != status {
        record.blocked_episode = record.blocked_episode.saturating_add(1);
    }
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

/// Whether a journaled row records a completed review — only an
/// `answered` judgment set counts (F23/H#79): a `failed`,
/// transport-failed, `stale` or invalid outcome is re-askable on the next
/// qualifying trigger.
fn answered(effect: &Effect) -> bool {
    matches!(
        &effect.receipt,
        Some(EffectReceipt::Judgments(record))
            if record.set.outcome == JudgmentOutcome::Answered
    )
}

/// The next key in an ask family — `base`, then `base:1`, `base:2`, … —
/// or `None` when a row in the family is still in flight
/// (`planned`/`dispatching`) or already answered: at most one ask in
/// flight per key, and only a completed review suppresses it (F23).
fn next_ask_key(journal: &[Effect], base: &EffectKey) -> Option<EffectKey> {
    let prefix = format!("{}:", base.0);
    let mut attempts = 0_u64;
    for effect in journal {
        if effect.key != *base && !effect.key.0.starts_with(&prefix) {
            continue;
        }
        match effect.state {
            EffectState::Planned | EffectState::Dispatching => return None,
            EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed => {
                if answered(effect) {
                    return None;
                }
                attempts = attempts.saturating_add(1);
            }
        }
    }
    Some(match attempts {
        0 => base.clone(),
        count => EffectKey(format!("{}:{count}", base.0)),
    })
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
    // H#79 — unchanged evidence is never re-asked after a completed
    // review: the family's key carries the generation, so an answered or
    // in-flight attempt suppresses this one while a failed one re-asks.
    let base = effect_key(run, &format!("review:{}", run.evidence_generation));
    let key = next_ask_key(journal, &base)?;
    // `jev_evaluate` renders on dispatch-time state — no plan-time digest.
    Some(planned_effect(
        run,
        EffectKind::JevEvaluate,
        key,
        None,
        None,
    ))
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
        State::Prompting => locate(run, status, pane, native_session),
        // F23/F21 — a blocked child is asked `provider_limited` in every
        // supervised state, not only `active`.
        State::Judging | State::Repair => match status {
            Some(ChildStatus::Blocked) => {
                blocked_observed(run, status, pane, native_session, journal)
            }
            Some(ChildStatus::Working | ChildStatus::Idle | ChildStatus::Done) | None => {
                locate(run, status, pane, native_session)
            }
        },
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
        record.idle_deadline = Some(now.after(policy.idle_window));
    }
    let mut effects = Vec::new();
    if record.nudged_episode != Some(record.nudge_episode)
        && let Some(identity) = record.identity.clone()
    {
        // OQ-15 — the nudge's params are the effect key: the persisted
        // selector naming the episode's nudge recipe the dispatcher renders.
        let key = effect_key(run, &format!("nudge:{}", run.nudge_episode));
        let target = EffectTarget::Child(identity);
        let digest = op_digest(EffectKind::Prompt, Some(&target), key.0.as_bytes());
        effects.push(planned_effect(
            run,
            EffectKind::Prompt,
            key,
            Some(target),
            Some(digest),
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

/// `obs(blocked)` in `active`, `repair` or `judging` — F23/F21: ask
/// `provider_limited` (the ask carries `blocked_on_input` too), once per
/// blocked episode — `blocked:<episode>` — with a failed, transport-failed,
/// stale or invalid attempt re-asked on the next blocked observation of
/// the same episode. Never prompt a blocked child — no `prompt` effect is
/// ever planned here (F17).
fn blocked_observed(
    run: &Run,
    status: Option<ChildStatus>,
    pane: &PaneId,
    native_session: Option<&NativeSession>,
    journal: &[Effect],
) -> Transition {
    let mut record = run.clone();
    observe_fields(&mut record, status, pane, native_session);
    let base = effect_key(run, &format!("blocked:{}", record.blocked_episode));
    let mut effects = Vec::new();
    if let Some(key) = next_ask_key(journal, &base) {
        effects.push(planned_effect(
            run,
            EffectKind::JevEvaluate,
            key,
            None,
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
                    } else if next.child_status == Some(ChildStatus::Blocked) {
                        // F17 — a blocked child is never prompted (the F9
                        // eligibility rule): the episode's one nudge stays
                        // unspent, so an unblocked stall still nudges.
                    } else if let Some(identity) = next.identity.clone() {
                        let key = effect_key(run, &format!("nudge:{}", run.nudge_episode));
                        let target = EffectTarget::Child(identity);
                        let digest = op_digest(EffectKind::Prompt, Some(&target), key.0.as_bytes());
                        effects.push(planned_effect(
                            run,
                            EffectKind::Prompt,
                            key,
                            Some(target),
                            Some(digest),
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
