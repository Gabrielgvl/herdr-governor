//! F23/F25 — supervision: the periodic review ask, the `unique`/`blocked`
//! observation lanes (idle episodes, the one nudge per episode) and the
//! supervision mapping applied to answered review sets.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildIdentity, ChildStatus, EffectKey, NativeSession, PaneId};
use crate::routing::{JudgmentOutcome, JudgmentRecord, Question, noul_cleared};

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
/// (`planned`/`dispatching`) or, with `answered_terminal`, already
/// answered: at most one ask in flight per key, and only a completed
/// review suppresses it (F23). The acceptance family passes `false`
/// (F24/OQ-K): the run leaves `judging` the moment a verdict lands, so an
/// answered row while still `judging` is a set `acceptance_verdict`
/// refused — it counts as an attempt and the family re-asks.
fn next_ask_key(
    journal: &[Effect],
    base: &EffectKey,
    answered_terminal: bool,
) -> Option<EffectKey> {
    let prefix = format!("{}:", base.0);
    let mut attempts = 0_u64;
    for effect in journal {
        if effect.key != *base && !effect.key.0.starts_with(&prefix) {
            continue;
        }
        match effect.state {
            EffectState::Planned | EffectState::Dispatching => return None,
            EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed => {
                if answered_terminal && answered(effect) {
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
    let key = next_ask_key(journal, &base, true)?;
    // `jev_evaluate` renders on dispatch-time state — no plan-time digest.
    Some(planned_effect(
        run,
        EffectKind::JevEvaluate,
        key,
        None,
        None,
    ))
}

/// F24 — the acceptance ask's re-ask rule: the `accept:<wg>:<gen>` family
/// retries a failed, unconfirmed, stale or otherwise unresolved attempt
/// under the next attempt key, only while the Run is `judging` — an
/// in-flight attempt suppresses it; an `answered` one does not. The run
/// leaves `judging` when a verdict lands, so an answered row here is a
/// partial or malformed set `acceptance_verdict` refused: it counts as
/// an attempt and the family re-asks (OQ-K). One outcome ends the
/// family: a `too_large` attempt is terminal because the frozen request
/// cannot shrink, so the wait falls to `judgment_deadline` →
/// `unresolved(judgment_unavailable)` (OQ-I). `Some` is the effect to
/// plan.
#[must_use]
pub fn acceptance_retry(run: &Run, journal: &[Effect]) -> Option<Effect> {
    if run.state != State::Judging {
        return None;
    }
    let base = effect_key(
        run,
        &format!("accept:{}:{}", run.work_generation, run.evidence_generation),
    );
    let prefix = format!("{}:", base.0);
    let too_large = journal.iter().any(|effect| {
        (effect.key == base || effect.key.0.starts_with(&prefix))
            && matches!(
                &effect.receipt,
                Some(EffectReceipt::Judgments(record))
                    if record.set.outcome == JudgmentOutcome::TooLarge
            )
    });
    if too_large {
        return None;
    }
    let key = next_ask_key(journal, &base, false)?;
    Some(planned_effect(
        run,
        EffectKind::JevEvaluate,
        key,
        None,
        None,
    ))
}

/// F31 — the typed provider-limit record's dedup identity:
/// `<source>:<observed_at_ms>` minted by the daemon from the parsed
/// record (§4.17). The `run:<id>:limit:<record_id>` family carries no
/// generation component, so a record asks exactly once for the Run's
/// lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitRecordKey(pub String);

/// F31 — one `jev_evaluate` under `run:<id>:limit:<record_id>` once per
/// record for the Run's lifetime (§4.17): the ask rides `active`,
/// `judging` and `repair` only, never while the same record's ask is in
/// flight, and a `failed`/`unconfirmed` attempt re-asks under `:<n>`
/// (`next_ask_key`, like the other families). While the current blocked
/// episode's ask is in flight it already carries the record as evidence
/// — one ask per pass, no double ask. `Some` is the effect to plan.
#[must_use]
pub fn limit_observed(run: &Run, record: &LimitRecordKey, journal: &[Effect]) -> Option<Effect> {
    match run.state {
        State::Active | State::Judging | State::Repair => {}
        State::Reserved | State::Starting | State::Prompting | State::Settled => return None,
    }
    let blocked = effect_key(run, &format!("blocked:{}", run.blocked_episode));
    let blocked_prefix = format!("{}:", blocked.0);
    if journal.iter().any(|effect| {
        (effect.key == blocked || effect.key.0.starts_with(&blocked_prefix))
            && matches!(
                effect.state,
                EffectState::Planned | EffectState::Dispatching
            )
    }) {
        return None;
    }
    let base = effect_key(run, &format!("limit:{}", record.0));
    let key = next_ask_key(journal, &base, true)?;
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

/// F9/F25 — the episode's one nudge, shared by the idle and review paths so
/// its persisted descriptor cannot drift between them. OQ-15 — the nudge's
/// params are the effect key: the persisted selector naming the episode's
/// nudge recipe the dispatcher renders.
fn nudge_effect(run: &Run, identity: ChildIdentity) -> Effect {
    let key = effect_key(run, &format!("nudge:{}", run.nudge_episode));
    let target = EffectTarget::Child(identity);
    let digest = op_digest(EffectKind::Prompt, Some(&target), key.0.as_bytes());
    planned_effect(run, EffectKind::Prompt, key, Some(target), Some(digest))
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
        effects.push(nudge_effect(run, identity));
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
    if let Some(key) = next_ask_key(journal, &base, true) {
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
    match (result.resolution.receipt(), result.resolution.outcome()) {
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
                        effects.push(nudge_effect(run, identity));
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
