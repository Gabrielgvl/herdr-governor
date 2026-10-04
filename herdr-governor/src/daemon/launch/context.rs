//! `launch/context` — the values the launch pipeline shares: the eval
//! journal-key convention, the `evaluating`-phase finish compose (with
//! the F21 recovery `blocked` write every abstention owes), the asked
//! `open_tabs` recovery from a journaled set, the caller's tab set from
//! one snapshot, the `RenderContext::Jev` `context_for` hands to the
//! runner, and the §6.2 `tools/call` response bodies.

use serde_json::{Value, json};

use governor_core::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use governor_core::identity::{
    CallerKey, EffectKey, EventId, JudgmentSetId, LaunchId, TabId, Timestamp,
};
use governor_core::lifecycle::{
    CreatedTopology, Effect, EffectReceipt, EffectState, Run, State, StateChange, Transition,
};
use governor_core::recovery::{RecoveryObligation, RecoveryStatus};
use governor_core::routing::{
    JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Question, QuestionVersion,
    TabChoice, validate_evaluation,
};
use governor_core::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase, finish};

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::adapters::herdr::SessionSnapshot;
use crate::adapters::jev::State as JevState;
use crate::daemon::{FileRef, RenderContext, questions};
use crate::store::Store;
use governor_core::config::Policy;

/// The `launch:<id>:evaluate` journal key `admit` plans under (OQ-1) —
/// the same derivation `task::launch_row` uses, so the daemon never
/// invents a second spelling.
pub(super) fn eval_key(launch_id: &LaunchId) -> EffectKey {
    EffectKey(format!("launch:{id}:evaluate", id = launch_id.0))
}

/// The judgment-set id the eval context frames and `on_evaluated`
/// re-reads — `jset:<effect_key>` per §4.2, derived from the journal
/// key so a restart finds the set without a second map.
pub(super) fn eval_set_id(key: &EffectKey) -> JudgmentSetId {
    JudgmentSetId(format!("jset:{}", key.0))
}

/// A `RecordRecovery` write as a one-change `Transition` — the
/// obligation states `pending`/`dispatched`/`blocked` arrive already
/// derived (`RecoveryObligation::{pending,dispatched,blocked}`).
pub(super) fn recovery_write(obligation: RecoveryObligation) -> Transition {
    Transition {
        state_changes: Vec::from([StateChange::RecordRecovery(obligation)]),
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// F21 — a recovery move composed into `transition`: the obligation row
/// plus, for `dispatched`/`blocked`, its `recovery_dispatched`/
/// `recovery_blocked` mailbox event bound to the predecessor Run (its
/// owner reads it). The id follows the core's `evt:<dedup_key>`
/// convention, so a recomputed compose re-derives the same row.
pub(super) fn recovery_move(transition: &mut Transition, moved: RecoveryObligation) {
    let predecessor = moved.predecessor.0.clone();
    let event = match moved.status {
        RecoveryStatus::Dispatched => Some((
            MailboxEventKind::RecoveryDispatched,
            json!({
                "predecessor": predecessor,
                "successor_launch": moved.successor_launch.as_ref().map(|id| id.0.clone()),
            }),
        )),
        RecoveryStatus::Blocked => Some((
            MailboxEventKind::RecoveryBlocked,
            json!({"predecessor": predecessor, "reason": moved.reason}),
        )),
        RecoveryStatus::Pending | RecoveryStatus::Failed => None,
    };
    if let Some((kind, body)) = event {
        let subject = MailboxSubject::Run(moved.predecessor.clone());
        if let Some(dedup) = kind.dedup_key(&subject, None)
            && let Some(mailbox) = MailboxEvent::emitted(
                EventId(format!("evt:{}", dedup.0)),
                subject,
                kind,
                None,
                body.to_string(),
            )
        {
            transition.events.push(mailbox);
        }
    }
    transition
        .state_changes
        .push(StateChange::RecordRecovery(moved));
}

/// The still-`pending` obligation a Launch's `recovery_of` names, when
/// one exists — the row `blocked`/`dispatched` rides on. Reads the
/// pending set, never a claimed successor's.
pub(super) fn pending_obligation(store: &Store, launch: &Launch) -> Option<RecoveryObligation> {
    let predecessor = launch.task.recovery_of.as_ref()?;
    store
        .recoveries_by_state(RecoveryStatus::Pending)
        .ok()?
        .into_iter()
        .find(|obligation| obligation.predecessor == *predecessor)
}

/// The `evaluating`-phase terminal compose every caller shares —
/// `finish(…)` plus the pending obligation's `blocked` write and its
/// `recovery_blocked` event when the outcome carries a reason (F21:
/// `pending` → `blocked` on the successor's abstention; a `Rejected`
/// evaluation blocks the same way — the evaluation did not yield a
/// routable answer).
pub(in crate::daemon) fn eval_finish(
    store: &Store,
    launch: &Launch,
    outcome: &LaunchOutcome,
    eval_state: Option<EffectState>,
    now: Timestamp,
    policy: &Policy,
) -> Transition {
    let mut transition = finish(launch, outcome.clone(), eval_state, None, now, policy);
    let reason = match outcome {
        LaunchOutcome::Abstained { reason } => *reason,
        LaunchOutcome::Rejected => AbstainReason::EvaluationFailed,
        LaunchOutcome::Launched { .. } | LaunchOutcome::Failed { .. } => return transition,
    };
    if let Some(blocked) = pending_obligation(store, launch).and_then(|o| o.blocked(reason)) {
        recovery_move(&mut transition, blocked);
    }
    transition
}

/// F5/F15 — the `launched` outcome for a Run that reached `prompting`:
/// the point it started on, the persisted decision as tier evidence,
/// and `requestedOperatingPointId` — the decision's first candidate —
/// only when fallback moved the point (spec F15). `None` while the Run
/// has no started point or the Launch no decision.
pub(in crate::daemon) fn launched(launch: &Launch, run: &Run) -> Option<LaunchOutcome> {
    let (Some(point), Some(decision)) = (&run.operating_point, &launch.decision) else {
        return None;
    };
    let requested = decision
        .candidates
        .first()
        .map(|first| &first.operating_point)
        .filter(|first| *first != point)
        .cloned();
    Some(LaunchOutcome::Launched {
        run: run.id.clone(),
        operating_point: point.clone(),
        requested_operating_point: requested,
        tier_evidence: decision.clone(),
    })
}

/// §4.5/F15 — `finish(Launched)` composed into the start-ack apply: when
/// `emitted` (the core's `EffectResult` transition for `before`) moves
/// the Run `starting → prompting`, the `launching` Launch finishes in
/// the same transaction — once one start succeeds no later failure
/// releases the Run, and the parked caller is answered at the ack, not
/// a reconcile tick later. `None` for every other result.
pub(in crate::daemon) fn launched_on_start(
    launch: Option<&Launch>,
    before: &Run,
    emitted: &Transition,
    now: Timestamp,
    policy: &Policy,
) -> Option<Transition> {
    let open = launch.filter(|row| row.phase == LaunchPhase::Launching)?;
    if before.state != State::Starting {
        return None;
    }
    let started = emitted.state_changes.iter().find_map(|change| {
        if let StateChange::UpdateRun(update) = change
            && update.record.id == before.id
            && update.record.state == State::Prompting
        {
            Some(&update.record)
        } else {
            None
        }
    })?;
    let outcome = launched(open, started)?;
    Some(finish(open, outcome, None, None, now, policy))
}

/// The asked `open_tabs` the journaled record answers to: the
/// `related_tab` judgment's distribution holds the offered labels —
/// `new` is the synthetic choice, the rest are the tabs asked about.
/// Restart-safe: the asked set lives only in the journal, never in a
/// snapshot or a coordinator map.
pub(super) fn asked_tabs(record: &JudgmentRecord) -> Vec<TabId> {
    record
        .judgments
        .iter()
        .find(|judgment| judgment.question == Question::RelatedTab)
        .map(|judgment| {
            judgment
                .probabilities
                .keys()
                .filter(|label| label.as_str() != "new")
                .map(|label| TabId(label.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The caller's open governor tabs with their pane counts: the tabs in
/// the workspace the caller's pane sits in, from one snapshot. The
/// caller's pane resolves by `native_session` — a caller the snapshot
/// cannot place has no attributable tabs.
pub(super) fn caller_tabs(caller: &CallerKey, snapshot: &SessionSnapshot) -> Vec<(TabId, usize)> {
    let Some(pane) = snapshot.panes.iter().find(|pane| {
        pane.agent_session
            .as_ref()
            .is_some_and(|session| session.value == caller.native_session.0)
    }) else {
        return Vec::new();
    };
    snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == pane.workspace_id)
        .map(|tab| {
            (
                TabId(tab.tab_id.clone()),
                usize::try_from(tab.pane_count).unwrap_or(usize::MAX),
            )
        })
        .collect()
}

/// The `RenderContext::Jev` for a launch `jev_evaluate` — the one place
/// the launch ask is built. `open_tabs` comes from the freshest
/// coordinator snapshot; a restart re-asks against the current view
/// (the asked set is journaled only in the answering set's
/// `related_tab` distribution — `asked_tabs` recovers it). A launch no
/// longer `evaluating` renders nothing: its eval row is already the
/// honest terminal record.
pub(in crate::daemon) fn eval_context(
    store: &Store,
    snapshot: Option<&SessionSnapshot>,
    effect: &Effect,
    loaded: &LoadedConfig,
    daemon: &DaemonSettings,
) -> Option<RenderContext> {
    let launch_id = effect.subject_launch.as_ref()?;
    let launch = store.launch(launch_id).ok().flatten()?;
    if launch.phase != LaunchPhase::Evaluating {
        return None;
    }
    let open_tabs: Vec<TabId> = snapshot
        .map(|view| {
            caller_tabs(&launch.caller, view)
                .into_iter()
                .map(|(tab, _)| tab)
                .collect()
        })
        .unwrap_or_default();
    let set = JudgmentSet {
        id: eval_set_id(&effect.key),
        purpose: JudgmentPurpose::Launch,
        launch: Some(launch_id.clone()),
        run: None,
        versions: None,
        task_digest: launch.task_digest,
        handoff_digest: None,
        evidence_digest: None,
        model: String::new(),
        question_version: QuestionVersion(questions::QUESTION_VERSION.into()),
        policy_version: loaded.version.clone(),
        // `Stale` only says "not yet answered": the wire stamps the
        // honest outcome before this frame ever journals.
        outcome: JudgmentOutcome::Stale,
    };
    Some(RenderContext::Jev {
        model: daemon.jev_model.clone(),
        state: JevState::Task((&launch.task).into()),
        questions: questions::launch_specs(&open_tabs, &loaded.config.policy.tiers),
        set: Box::new(set),
        frozen: None::<FileRef>,
    })
}

/// The §6.2 response body for a Launch's current state: the stored
/// outcome when `done`, `pending {launchId, runId?}` otherwise.
pub(super) fn launch_body(launch: &Launch, run: Option<&Run>) -> Value {
    match (&launch.phase, &launch.outcome) {
        (LaunchPhase::Done, Some(outcome)) => outcome_body(outcome),
        _ => pending_body(launch, run),
    }
}

fn pending_body(launch: &Launch, run: Option<&Run>) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("outcome".into(), json!("pending"));
    body.insert("launchId".into(), json!(launch.id.0));
    if let Some(reserved) = run {
        body.insert("runId".into(), json!(reserved.id.0));
    }
    Value::Object(body)
}

pub(super) fn outcome_body(outcome: &LaunchOutcome) -> Value {
    match outcome {
        LaunchOutcome::Launched {
            run,
            operating_point,
            requested_operating_point,
            tier_evidence,
        } => {
            let mut evidence = serde_json::Map::new();
            evidence.insert("startTier".into(), json!(tier_evidence.start_tier.0));
            evidence.insert("judgedTier".into(), json!(tier_evidence.judged_tier.0));
            if let Some(requested) = &tier_evidence.requested_tier {
                evidence.insert("requestedTier".into(), json!(requested.0));
            }
            let mut body = serde_json::Map::new();
            body.insert("outcome".into(), json!("launched"));
            body.insert("runId".into(), json!(run.0));
            body.insert("operatingPointId".into(), json!(operating_point.0));
            if let Some(requested) = requested_operating_point {
                body.insert("requestedOperatingPointId".into(), json!(requested.0));
            }
            body.insert("tierEvidence".into(), Value::Object(evidence));
            Value::Object(body)
        }
        LaunchOutcome::Abstained { reason } => json!({
            "outcome": "abstained",
            "reason": reason.as_str(),
        }),
        LaunchOutcome::Rejected => json!({"outcome": "rejected"}),
        LaunchOutcome::Failed {
            certainty,
            run,
            created_topology,
        } => {
            let mut body = serde_json::Map::new();
            body.insert("outcome".into(), json!("failed"));
            body.insert("effectCertainty".into(), json!(certainty.as_str()));
            if let Some(settled) = run {
                body.insert("runId".into(), json!(settled.0));
            }
            body.insert(
                "createdTopology".into(),
                json!({
                    "tab": created_topology.tab.as_ref().map(|tab| tab.0.clone()),
                    "panes": created_topology
                        .panes
                        .iter()
                        .map(|pane| pane.0.clone())
                        .collect::<Vec<_>>(),
                }),
            );
            Value::Object(body)
        }
    }
}

/// The `routed`-restart row's placement input: `related_tab` from the
/// journaled set (`begin_launch` needs the same evaluation-derived
/// choice the live path had — the persisted `Decision` carries tiers
/// and candidates, not placement).
pub(super) fn eval_related_tab(
    store: &Store,
    policy: &Policy,
    launch_id: &LaunchId,
) -> Option<TabChoice> {
    let eval = store.effect(&eval_key(launch_id)).ok().flatten()?;
    let Some(EffectReceipt::Judgments(record)) = eval.receipt.as_ref() else {
        return None;
    };
    let asked = asked_tabs(record);
    validate_evaluation(record, policy, &asked)
        .ok()
        .and_then(|evaluation| evaluation.related_tab)
}

/// The `CreatedTopology` an absent-caller finish writes — nothing was
/// created, so the evidence is the empty shape, not an omission.
pub(super) fn no_topology() -> CreatedTopology {
    CreatedTopology {
        tab: None,
        panes: Vec::new(),
    }
}
