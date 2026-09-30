//! F22 — the total Appendix C transition function: every `State` against
//! every `Event`, matched exhaustively — the `on_*` lanes and the
//! `effect_result` consequences (launch pipeline, prompt, repair).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::acceptance::{FrozenHandoff, HandoffReading};
use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildIdentity, Digest, Observation, Timestamp};
use crate::routing::{Decision, JudgmentOutcome, PlacementPlan};

use super::supervision::{on_unique, review_result};
use super::{
    DeadlineKind, Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult,
    EffectState, EffectTarget, EffectWrite, Event, JudgmentVerdict, PromptCertainty, Run,
    Settlement, State, StateChange, Transition, UnresolvedReason, VersionTriple, Versioned,
    deadline_after, edited, effect_key, journaled, mailbox_event, nothing, planned_effect, settle,
    update_if_changed, write_run,
};

/// F20 — the version stamp a Run reads as "still holding".
fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// F22 — the total Appendix C transition function: every `State` against
/// every `Event`, matched exhaustively. `read` is the persisted context the
/// transition consults beyond the Run row — `(decision, journal, handoffs)`:
/// the Launch's routing decision, the Run's effect journal, and its frozen
/// handoffs. `freeze_path` is the coordinator-supplied destination a new
/// freeze writes (F24).
///
/// Async results — `obs`, `handoff`, `judgment`, `deadline` and the
/// `provider_limited` judgment — apply only while the
/// `(version, work_generation, evidence_generation)` they were requested
/// against still hold (F20); a stale stamp produces nothing. `cancel`,
/// `restart` and `effect_result` are synchronous or journal-bound and apply
/// unconditionally (the journal write is durable fact).
#[must_use]
pub fn transition(
    run: &Run,
    event: &Versioned<Event>,
    now: Timestamp,
    policy: &Policy,
    read: (Option<&Decision>, &[Effect], &[FrozenHandoff]),
    freeze_path: &str,
) -> Transition {
    let (decision, journal, handoffs) = read;
    if carries_versions(&event.value) && event.requested_against != triple_of(run) {
        return nothing();
    }
    match &event.value {
        Event::Obs {
            observation,
            handoff_reading,
        } => on_observation(
            run,
            observation,
            handoff_reading.as_ref(),
            (now, policy),
            journal,
            (handoffs, freeze_path),
        ),
        Event::Handoff { digest } => {
            on_handoff(run, *digest, (now, policy), (handoffs, freeze_path))
        }
        Event::Judgment(verdict) => on_judgment(run, *verdict, (now, policy)),
        Event::Deadline(kind) => on_deadline(run, *kind, (now, policy)),
        Event::Cancel { close_pane } => on_cancel(run, *close_pane, (now, policy), journal),
        Event::ProviderLimited => settle(run, Settlement::ProviderLimited, now, policy),
        Event::EffectResult(result) => {
            on_effect_result(run, result, (now, policy), decision, journal)
        }
        Event::Restart => on_restart(run, journal),
    }
}

/// F20 — the event kinds that carry the version stamp: Jev results,
/// observations and deadlines. `cancel`, `restart` and `effect_result` are
/// not async results — the conditional writes guard them at apply time.
fn carries_versions(event: &Event) -> bool {
    match event {
        Event::Obs { .. }
        | Event::Handoff { .. }
        | Event::Judgment(..)
        | Event::Deadline(..)
        | Event::ProviderLimited => true,
        Event::Cancel { .. } | Event::EffectResult(..) | Event::Restart => false,
    }
}

fn on_observation(
    run: &Run,
    observation: &Observation,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    journal: &[Effect],
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    match observation {
        // obs(invalid) changes nothing in any state (F3/H#74).
        Observation::Invalid => nothing(),
        Observation::Absent => on_absent(run, reading, env, handoffs),
        Observation::Unique {
            status,
            pane,
            native_session,
        } => on_unique(run, *status, pane, native_session.as_ref(), env, journal),
    }
}

fn on_absent(
    run: &Run,
    reading: Option<&HandoffReading>,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let (now, policy) = env;
    match run.state {
        // The pane that would host the child is gone before it existed.
        State::Reserved => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchNotStarted,
            },
            now,
            policy,
        ),
        State::Starting => settle(
            run,
            Settlement::Unresolved {
                reason: UnresolvedReason::LaunchFailed,
            },
            now,
            policy,
        ),
        State::Prompting => settle(run, Settlement::PaneLost, now, policy),
        State::Active => {
            // F25 — a frozen handoff that has not been judged goes to
            // judgment first; otherwise the marked file is read once.
            if handoffs
                .0
                .iter()
                .any(|h| h.work_generation == run.work_generation)
            {
                enter_judging(run, env)
            } else {
                match reading {
                    Some(HandoffReading::Valid { digest }) => {
                        freeze_and_judge(run, *digest, env, handoffs.1)
                    }
                    Some(HandoffReading::NotWritten) | None => {
                        settle(run, Settlement::PaneLost, now, policy)
                    }
                }
            }
        }
        // `judging` still judges the frozen handoff; `repair` waits out its
        // deadline; `settled` ignores everything but the close rule.
        State::Judging | State::Repair | State::Settled => nothing(),
    }
}

fn on_handoff(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    handoffs: (&[FrozenHandoff], &str),
) -> Transition {
    let frozen_for_generation = handoffs
        .0
        .iter()
        .any(|h| h.work_generation == run.work_generation && h.digest == digest);
    match run.state {
        State::Active => {
            if frozen_for_generation {
                // already frozen for this generation — judging, no re-freeze
                enter_judging(run, env)
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        // a digest not yet judged freezes and judges; one already frozen was
        // already judged (F24 — unchanged digests are never re-judged).
        State::Judging | State::Repair => {
            if frozen_for_generation {
                nothing()
            } else {
                freeze_and_judge(run, digest, env, handoffs.1)
            }
        }
        State::Reserved | State::Starting | State::Prompting | State::Settled => nothing(),
    }
}

/// Move to `judging` when the handoff was already frozen (`obs(absent)` in
/// `active`, or a repeated `handoff` event for a known digest). The freeze
/// transaction already planned the assessment, so nothing is re-planned
/// here; `judgment_deadline` is armed defensively if unset.
fn enter_judging(run: &Run, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    update_if_changed(run, |next| {
        next.state = State::Judging;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
    })
}

/// Freeze a new handoff digest and move to `judging` — the Appendix B freeze
/// transaction: the `handoffs` row, `evidence_generation+1`, and
/// `judgment_deadline` if not already set (it is never re-armed).
fn freeze_and_judge(
    run: &Run,
    digest: Digest,
    env: (Timestamp, &Policy),
    freeze_path: &str,
) -> Transition {
    let (now, policy) = env;
    let generation = run.evidence_generation.saturating_add(1);
    let record = edited(run, |next| {
        next.state = State::Judging;
        next.evidence_generation = generation;
        if next.judgment_deadline.is_none() {
            next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
        }
        next.idle_since = None;
        next.idle_deadline = None;
    });
    Transition {
        state_changes: Vec::from([
            StateChange::FreezeHandoff(FrozenHandoff {
                run: run.id.clone(),
                work_generation: run.work_generation,
                digest,
                frozen_path: String::from(freeze_path),
                frozen_at: now,
            }),
            write_run(run, record),
        ]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::JevEvaluate,
            effect_key(
                run,
                &format!("accept:{}:{}", run.work_generation, generation),
            ),
            None,
        )]),
    }
}

fn on_judgment(run: &Run, verdict: JudgmentVerdict, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    match run.state {
        State::Judging => match verdict {
            JudgmentVerdict::Accept => settle(run, Settlement::Accepted, now, policy),
            JudgmentVerdict::Reject => {
                // repair_deadline arms on the first rejection of a work
                // generation and is never extended (F24).
                let deadline = run
                    .repair_deadline
                    .unwrap_or_else(|| deadline_after(now, policy.repair_window));
                let record = edited(run, |next| {
                    next.state = State::Repair;
                    next.repair_deadline = Some(deadline);
                    next.idle_since = None;
                    next.idle_deadline = None;
                });
                Transition {
                    state_changes: Vec::from([write_run(run, record)]),
                    events: Vec::from([mailbox_event(
                        run,
                        MailboxEventKind::HandoffRejected,
                        &format!(
                            "handoff_rejected:{}:{}",
                            run.work_generation, run.evidence_generation
                        ),
                        String::from("{\"verdict\":\"reject\"}"),
                    )]),
                    effects: Vec::new(),
                }
            }
            // Jev could not complete: the Run waits on judgment_deadline
            // (armed at the freeze; defensive re-arm here).
            JudgmentVerdict::Unavailable => update_if_changed(run, |next| {
                if next.judgment_deadline.is_none() {
                    next.judgment_deadline = Some(deadline_after(now, policy.judgment_window));
                }
            }),
        },
        // A fresh verdict is meaningful only while a frozen handoff is being
        // judged; everywhere else it is ignored.
        State::Reserved
        | State::Starting
        | State::Prompting
        | State::Active
        | State::Repair
        | State::Settled => nothing(),
    }
}

fn on_deadline(run: &Run, kind: DeadlineKind, env: (Timestamp, &Policy)) -> Transition {
    let (now, policy) = env;
    if run.state == State::Settled {
        return nothing();
    }
    let overdue = |deadline: Option<Timestamp>| deadline.is_some_and(|d| now >= d);
    match kind {
        DeadlineKind::MaxAge => {
            if now >= run.max_age_deadline {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::MaxAge,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
        DeadlineKind::Idle => {
            if run.state == State::Active && overdue(run.idle_deadline) {
                settle(run, Settlement::NoHandoff, now, policy)
            } else {
                nothing()
            }
        }
        // The repair deadline keeps running through `judging` — a re-frozen
        // handoff does not extend it (F24).
        DeadlineKind::Repair => {
            if (run.state == State::Repair || run.state == State::Judging)
                && overdue(run.repair_deadline)
            {
                settle(run, Settlement::Rejected, now, policy)
            } else {
                nothing()
            }
        }
        DeadlineKind::Judgment => {
            if run.state == State::Judging && overdue(run.judgment_deadline) {
                settle(
                    run,
                    Settlement::Unresolved {
                        reason: UnresolvedReason::JudgmentUnavailable,
                    },
                    now,
                    policy,
                )
            } else {
                nothing()
            }
        }
    }
}

fn on_cancel(
    run: &Run,
    close_pane: bool,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    // `settled` accepts only `cancel` with `closePane` — and only the pane
    // close is planned (F20).
    if run.state == State::Settled {
        if close_pane && let Some(effect) = close_effect(run, journal) {
            return Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: Vec::from([effect]),
            };
        }
        return nothing();
    }
    let mut transition = settle(run, Settlement::Cancelled, now, policy);
    if close_pane && let Some(effect) = close_effect(run, journal) {
        transition.effects.push(effect);
    }
    transition
}

/// The verified close effect (F10) — planned once (`run:<id>:close` is the
/// dedup) and only while a captured identity exists to verify against.
fn close_effect(run: &Run, journal: &[Effect]) -> Option<Effect> {
    let key = effect_key(run, "close");
    if journaled(journal, &key) {
        return None;
    }
    run.identity.clone().map(|identity| {
        planned_effect(
            run,
            EffectKind::Close,
            key,
            Some(EffectTarget::Child(identity)),
        )
    })
}

/// F8 — how a resolution journals.
fn journal_state(outcome: EffectOutcome) -> EffectState {
    match outcome {
        EffectOutcome::Acknowledged => EffectState::Acknowledged,
        // pre-interactive failures provably never ran → failed/absent (F15)
        EffectOutcome::PreInteractiveFailed
        | EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        } => EffectState::Failed,
        EffectOutcome::Unconfirmed => EffectState::Unconfirmed,
    }
}

/// F8 — the certainty a result commit records (required on `failed`).
fn result_certainty(outcome: EffectOutcome) -> Option<EffectCertainty> {
    match outcome {
        EffectOutcome::PreInteractiveFailed => Some(EffectCertainty::Absent),
        EffectOutcome::Failed { certainty } => Some(certainty),
        EffectOutcome::Acknowledged | EffectOutcome::Unconfirmed => None,
    }
}

fn on_restart(run: &Run, journal: &[Effect]) -> Transition {
    // F8 — dispatching without a receipt becomes unconfirmed and is never
    // dispatched again; `planned` effects still may. No deadline changes.
    let mut state_changes = Vec::new();
    for effect in journal {
        if effect.state == EffectState::Dispatching {
            state_changes.push(StateChange::WriteEffect(EffectWrite {
                key: effect.key.clone(),
                state: EffectState::Unconfirmed,
                certainty: None,
                receipt: None,
            }));
        }
    }
    let mut events = Vec::new();
    // F16 — a task prompt that was dispatching becomes unconfirmed: possibly
    // consumed, no resubmission, the Run goes `active` with certainty
    // recorded and the caller is notified.
    if run.state == State::Prompting {
        let prompt_key = effect_key(run, "prompt:task");
        let interrupted = journal
            .iter()
            .any(|e| e.key == prompt_key && e.state == EffectState::Dispatching);
        if interrupted {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Unconfirmed);
            });
            state_changes.push(write_run(run, record));
            events.push(mailbox_event(
                run,
                MailboxEventKind::PromptUnconfirmed,
                "prompt_unconfirmed",
                String::from("{\"prompt\":\"unconfirmed\"}"),
            ));
        }
    }
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}

fn on_effect_result(
    run: &Run,
    result: &EffectResult,
    env: (Timestamp, &Policy),
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    // F20 — a Jev result applies only while its versions still hold; a stale
    // one journals its set with outcome `stale` and does nothing else.
    let stale = match &result.receipt {
        Some(EffectReceipt::Judgments(record)) => match record.set.versions {
            Some(versions) => versions != triple_of(run),
            None => false,
        },
        Some(
            EffectReceipt::AgentStarted { .. }
            | EffectReceipt::TabCreated { .. }
            | EffectReceipt::PaneCreated { .. },
        )
        | None => false,
    };
    let receipt = if stale {
        match &result.receipt {
            Some(EffectReceipt::Judgments(record)) => {
                let mut marked = record.clone();
                marked.set.outcome = JudgmentOutcome::Stale;
                Some(EffectReceipt::Judgments(marked))
            }
            Some(
                EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => result.receipt.clone(),
        }
    } else {
        result.receipt.clone()
    };
    let mut transition = Transition {
        state_changes: Vec::from([StateChange::WriteEffect(EffectWrite {
            key: result.key.clone(),
            state: journal_state(result.outcome),
            certainty: result_certainty(result.outcome),
            receipt,
        })]),
        events: Vec::new(),
        effects: Vec::new(),
    };
    if stale {
        return transition;
    }
    let consequences = match run.state {
        State::Reserved | State::Starting => launch_result(run, result, decision, journal),
        State::Prompting => prompt_result(run, result),
        State::Repair => repair_result(run, result, env),
        State::Active | State::Judging => review_result(run, result, now, policy),
        State::Settled => nothing(),
    };
    transition.state_changes.extend(consequences.state_changes);
    transition.events.extend(consequences.events);
    transition.effects.extend(consequences.effects);
    transition
}

/// `reserved`/`starting` — the launch pipeline's effect results (F14/F15):
/// topology acknowledgements plan the first `agent_start`; a start result
/// captures identity or walks the persisted candidates.
fn launch_result(
    run: &Run,
    result: &EffectResult,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    match result.kind {
        EffectKind::TabCreate => match result.outcome {
            // a new tab's initial pane hosts the child — never split (H#102)
            EffectOutcome::Acknowledged => plan_agent_start(run, PlacementPlan::NewTab, 0),
            EffectOutcome::PreInteractiveFailed
            | EffectOutcome::Failed {
                certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
            }
            | EffectOutcome::Unconfirmed => nothing(),
        },
        EffectKind::PaneSplit => match result.outcome {
            EffectOutcome::Acknowledged => {
                let plan = journal
                    .iter()
                    .find(|e| e.key == result.key)
                    .and_then(|e| e.target.clone())
                    .and_then(|target| match target {
                        EffectTarget::ExistingTab(tab) => Some(PlacementPlan::ExistingTab { tab }),
                        EffectTarget::CallerContext(_)
                        | EffectTarget::AgentPane(_)
                        | EffectTarget::Child(_) => None,
                    });
                match plan {
                    Some(placement) => plan_agent_start(run, placement, 0),
                    None => nothing(),
                }
            }
            EffectOutcome::PreInteractiveFailed
            | EffectOutcome::Failed {
                certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
            }
            | EffectOutcome::Unconfirmed => nothing(),
        },
        EffectKind::AgentStart => agent_start_result(run, result, decision, journal),
        // launch evaluation, stray prompts, closes: the journal write stands
        // on its own — other lanes consume the rows.
        EffectKind::JevEvaluate | EffectKind::Prompt | EffectKind::Close => nothing(),
    }
}

/// The `agent_start` effect for candidate `index` into `plan`'s pane.
fn plan_agent_start(run: &Run, plan: PlacementPlan, index: usize) -> Transition {
    Transition {
        state_changes: Vec::new(),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::AgentStart,
            effect_key(run, &format!("start:{index}")),
            Some(EffectTarget::AgentPane(plan)),
        )]),
    }
}

fn agent_start_result(
    run: &Run,
    result: &EffectResult,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    match result.outcome {
        EffectOutcome::Acknowledged => match &result.receipt {
            Some(EffectReceipt::AgentStarted { identity }) => {
                started(run, result, identity, decision, journal)
            }
            // an acknowledgement without the captured identity is not a
            // start — the Run waits on obs(absent) or max_age
            Some(
                EffectReceipt::Judgments(_)
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => nothing(),
        },
        // F15 — the pane is provably back at its shell: try the next
        // candidate in the same pane; with none left, wait for obs(absent)
        // or max_age.
        EffectOutcome::PreInteractiveFailed => {
            let tried = journal
                .iter()
                .filter(|e| e.kind == EffectKind::AgentStart)
                .count();
            let target = journal
                .iter()
                .find(|e| e.key == result.key)
                .and_then(|e| e.target.clone());
            let has_next = decision.is_some_and(|d| tried < d.candidates.len());
            if has_next {
                match target {
                    Some(EffectTarget::AgentPane(plan)) => {
                        return plan_agent_start(run, plan, tried);
                    }
                    Some(
                        EffectTarget::ExistingTab(_)
                        | EffectTarget::CallerContext(_)
                        | EffectTarget::Child(_),
                    )
                    | None => {}
                }
            }
            nothing()
        }
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        }
        | EffectOutcome::Unconfirmed => nothing(),
    }
}

/// `agent_start` acknowledged — capture the F2 identity, record the started
/// candidate's point/provider/tier, move to `prompting` and plan the Task
/// prompt (F15/F16). The candidate is identified by the effect's position
/// among the journal's `agent_start` entries (keys are `start:<index>`).
fn started(
    run: &Run,
    result: &EffectResult,
    identity: &ChildIdentity,
    decision: Option<&Decision>,
    journal: &[Effect],
) -> Transition {
    let index = journal
        .iter()
        .filter(|e| e.kind == EffectKind::AgentStart)
        .position(|e| e.key == result.key);
    let candidate = index.and_then(|i| decision.and_then(|d| d.candidates.get(i)));
    let record = edited(run, |next| {
        next.state = State::Prompting;
        next.identity = Some(identity.clone());
        if let Some(c) = candidate {
            next.operating_point = Some(c.operating_point.clone());
            next.provider = Some(c.provider.clone());
            next.tier_start = Some(c.tier.clone());
        }
    });
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            EffectKind::Prompt,
            effect_key(run, "prompt:task"),
            Some(EffectTarget::Child(identity.clone())),
        )]),
    }
}

/// `prompting` — the Task prompt's result (F16): acknowledged means the ack
/// matched the captured identity; anything else means possibly consumed —
/// `prompt_certainty = unconfirmed`, `active`, and the caller is notified.
fn prompt_result(run: &Run, result: &EffectResult) -> Transition {
    if result.kind != EffectKind::Prompt || result.key != effect_key(run, "prompt:task") {
        return nothing();
    }
    match result.outcome {
        EffectOutcome::Acknowledged => {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Acknowledged);
            });
            Transition {
                state_changes: Vec::from([write_run(run, record)]),
                events: Vec::new(),
                effects: Vec::new(),
            }
        }
        EffectOutcome::PreInteractiveFailed
        | EffectOutcome::Failed {
            certainty: EffectCertainty::Absent | EffectCertainty::Unknown,
        }
        | EffectOutcome::Unconfirmed => {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Unconfirmed);
            });
            Transition {
                state_changes: Vec::from([write_run(run, record)]),
                events: Vec::from([mailbox_event(
                    run,
                    MailboxEventKind::PromptUnconfirmed,
                    "prompt_unconfirmed",
                    String::from("{\"prompt\":\"unconfirmed\"}"),
                )]),
                effects: Vec::new(),
            }
        }
    }
}

/// `repair` — a repair follow-up (`outbox:<seq>`) dispatched before
/// `repair_deadline` opens a new work generation and returns the Run to
/// `active` (F24); anything else is supervision.
fn repair_result(run: &Run, result: &EffectResult, env: (Timestamp, &Policy)) -> Transition {
    if result.kind == EffectKind::Prompt
        && result.outcome == EffectOutcome::Acknowledged
        && result
            .key
            .0
            .starts_with(&format!("run:{}:outbox:", run.id.0))
        && run.repair_deadline.is_none_or(|d| env.0 < d)
    {
        let record = edited(run, |next| {
            next.state = State::Active;
            next.work_generation = next.work_generation.saturating_add(1);
            next.repair_deadline = None;
            next.idle_since = None;
            next.idle_deadline = None;
        });
        return Transition {
            state_changes: Vec::from([write_run(run, record)]),
            events: Vec::new(),
            effects: Vec::new(),
        };
    }
    review_result(run, result, env.0, env.1)
}
