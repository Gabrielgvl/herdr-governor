//! F8 — the `effect_result` lane: the journal write a resolution commits,
//! the stale-version drop, and the per-state consequences (the launch
//! pipeline's topology/start/plan chain, the task prompt's result, the
//! repair dispatch, and the supervision mapping for `active`/`judging`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildIdentity, EffectKey, Timestamp};
use crate::lifecycle::supervision::review_result;
use crate::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState,
    EffectTarget, EffectWrite, PromptCertainty, Run, Settlement, State, StateChange, Transition,
    edited, effect_key, mailbox_event, nothing, planned_effect, settle, write_run,
};
use crate::routing::{Decision, JudgmentOutcome, PlacementPlan};

use super::{repair_dispatch_in_window, triple_of};

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

pub(super) fn on_effect_result(
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
        State::Repair => repair_result(run, result, env, journal),
        // F24 — a Run back in `judging` keeps its open rejection window
        // (`rejected_at` is set for the current work generation): the
        // follow-up's in-window dispatch qualifies and the late-result
        // settle apply exactly as in `repair`.
        State::Judging if run.rejected_at.is_some() => repair_result(run, result, env, journal),
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
                started(run, result, identity, decision)
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
/// prompt (F15/F16). The candidate is identified by the acknowledged key's
/// `start:<index>` suffix — the row's position in the journal is not the
/// index (rows commit in dispatch order, not candidate order); an
/// unparsable key selects no candidate.
fn started(
    run: &Run,
    result: &EffectResult,
    identity: &ChildIdentity,
    decision: Option<&Decision>,
) -> Transition {
    let index = result
        .key
        .0
        .strip_prefix(&format!("run:{}:start:", run.id.0))
        .and_then(|suffix| suffix.parse::<usize>().ok());
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

/// Whether `key` names one of `run`'s repair follow-ups
/// (`run:<id>:outbox:<seq>`, Appendix B).
fn outbox_key(run: &Run, key: &EffectKey) -> bool {
    key.0.starts_with(&format!("run:{}:outbox:", run.id.0))
}

/// `repair` — and `judging` while its rejection window is still open
/// (`rejected_at` set) — a repair follow-up the journal proves was
/// dispatched inside the window (its row's `dispatched_at` lands in
/// `[rejected_at, repair_deadline)`) opens a new work generation and returns
/// the Run to `active` (F24): `acknowledged`, `unconfirmed` and
/// `failed/unknown` all qualify — possibly consumed — while a
/// `failed/absent` result proved the prompt never ran and does not. A
/// non-qualifying outbox result arriving past the deadline, with no
/// in-window dispatch still pending, settles `rejected` in the same
/// transition — the case where the deadline fired while that dispatch was
/// in flight. Anything else is supervision.
fn repair_result(
    run: &Run,
    result: &EffectResult,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    if result.kind == EffectKind::Prompt && outbox_key(run, &result.key) {
        let row = journal.iter().find(|e| e.key == result.key);
        if row.is_some_and(|e| repair_dispatch_in_window(run, e))
            && result_certainty(result.outcome) != Some(EffectCertainty::Absent)
        {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.work_generation = next.work_generation.saturating_add(1);
                next.repair_deadline = None;
                next.rejected_at = None;
                next.judging_digest = None;
                next.idle_since = None;
                next.idle_deadline = None;
            });
            return Transition {
                state_changes: Vec::from([write_run(run, record)]),
                events: Vec::new(),
                effects: Vec::new(),
            };
        }
        if run.repair_deadline.is_some_and(|deadline| now >= deadline)
            && !journal.iter().any(|effect| {
                effect.key != result.key
                    && effect.state == EffectState::Dispatching
                    && repair_dispatch_in_window(run, effect)
            })
        {
            return settle(run, Settlement::Rejected, now, policy);
        }
    }
    review_result(run, result, now, policy)
}
