//! Test fixtures for the lifecycle module — constructed inputs, no I/O.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::acceptance::FrozenHandoff;
use crate::config::{ConfigVersion, OperatingPointId, Policy, Provider, Tier};
use crate::delivery::MailboxEventKind;
use crate::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, Digest, EffectKey,
    HerdrIncarnation, JudgmentSetId, LaunchId, NativeSession, Observation, PaneId, RunId,
    TerminalId, Timestamp,
};
use crate::lifecycle::{
    Effect, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState, EffectTarget,
    Event, Run, Settlement, State, StateChange, Transition, VersionTriple, Versioned, transition,
};
use crate::routing::{
    Candidate, Decision, Exploration, Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord,
    JudgmentSet, Probability, Question, QuestionVersion,
};

pub(super) fn test_policy() -> Policy {
    Policy {
        tiers: Vec::from([Tier("t0".into()), Tier("t1".into())]),
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.7,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

pub(super) fn identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-1".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-1".into())),
        pane_id: PaneId("w0:p1".into()),
    }
}

pub(super) fn run_in(state: State) -> Run {
    let identity = match state {
        State::Reserved | State::Starting => None,
        State::Prompting | State::Active | State::Judging | State::Repair | State::Settled => {
            Some(identity())
        }
    };
    Run {
        id: RunId("r-1".into()),
        launch: LaunchId("l-1".into()),
        owner: CallerKey {
            agent_kind: AgentKind("caller-kind".into()),
            native_session: NativeSession("caller-sess".into()),
        },
        owner_generation: 0,
        version: 5,
        state,
        prompt_certainty: None,
        child_name: "gov-00000001".into(),
        identity,
        operating_point: Some(OperatingPointId("op-1".into())),
        provider: Some(Provider("prov-1".into())),
        tier_start: Some(Tier("t1".into())),
        cwd: "/repo".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        judgment_deadline: None,
        max_age_deadline: Timestamp(1_000),
        nudge_episode: 0,
        nudged_episode: None,
        settlement: None,
        settled_at: None,
    }
}

pub(super) fn triple(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

pub(super) fn stamped(run: &Run, value: Event) -> Versioned<Event> {
    Versioned {
        requested_against: triple(run),
        value,
    }
}

pub(super) fn stale_stamped(run: &Run, value: Event) -> Versioned<Event> {
    let mut stamp = triple(run);
    stamp.version = stamp.version.saturating_add(1);
    Versioned {
        requested_against: stamp,
        value,
    }
}

pub(super) fn obs_unique(status: Option<ChildStatus>) -> Event {
    Event::Obs {
        observation: Observation::Unique {
            status,
            pane: PaneId("w0:p9".into()),
            native_session: None,
        },
        handoff_reading: None,
    }
}

pub(super) fn journal_effect_at(
    run: &Run,
    suffix: &str,
    kind: EffectKind,
    state: EffectState,
    target: Option<EffectTarget>,
) -> Effect {
    let key = EffectKey(format!("run:{}:{}", run.id.0, suffix));
    Effect {
        id: crate::identity::EffectId(format!("eff:{}", key.0)),
        key,
        kind,
        subject_launch: Some(run.launch.clone()),
        subject_run: Some(run.id.clone()),
        target,
        payload_digest: None,
        state,
        certainty: None,
        receipt: None,
    }
}

pub(super) fn journal_effect(
    run: &Run,
    suffix: &str,
    kind: EffectKind,
    state: EffectState,
) -> Effect {
    journal_effect_at(run, suffix, kind, state, None)
}

pub(super) fn result_event(
    key: EffectKey,
    kind: EffectKind,
    outcome: EffectOutcome,
    receipt: Option<EffectReceipt>,
) -> Event {
    Event::EffectResult(EffectResult {
        key,
        kind,
        outcome,
        receipt,
    })
}

pub(super) fn run_result(
    run: &Run,
    suffix: &str,
    kind: EffectKind,
    outcome: EffectOutcome,
    receipt: Option<EffectReceipt>,
) -> Event {
    result_event(
        EffectKey(format!("run:{}:{}", run.id.0, suffix)),
        kind,
        outcome,
        receipt,
    )
}

pub(super) fn frozen(run: &Run, work_generation: u64, digest: u8) -> FrozenHandoff {
    FrozenHandoff {
        run: run.id.clone(),
        work_generation,
        digest: Digest([digest; 32]),
        frozen_path: "/state/handoffs/r-1".into(),
        frozen_at: Timestamp(0),
    }
}

pub(super) fn candidate(index: usize) -> Candidate {
    Candidate {
        operating_point: OperatingPointId(format!("op-{index}")),
        provider: Provider(format!("prov-{index}")),
        tier: Tier(format!("t{index}")),
        harness: AgentKind("kind".into()),
        args: Vec::new(),
    }
}

pub(super) fn decision(count: usize) -> Decision {
    Decision {
        judged_tier: Tier("t0".into()),
        requested_tier: None,
        policy_cap: None,
        policy_floor: None,
        caller_uplift: None,
        recovery_minimum: None,
        exploration: Exploration {
            assigned: false,
            executed: false,
        },
        start_tier: Tier("t0".into()),
        candidates: (0..count).map(candidate).collect(),
        config_version: ConfigVersion("cfg-1".into()),
    }
}

pub(super) fn review_record(run: &Run, judgments: Vec<Judgment>) -> JudgmentRecord {
    JudgmentRecord {
        set: JudgmentSet {
            id: JudgmentSetId("set-1".into()),
            purpose: JudgmentPurpose::Review,
            launch: None,
            run: Some(run.id.clone()),
            versions: Some(triple(run)),
            task_digest: Digest([0; 32]),
            handoff_digest: None,
            evidence_digest: None,
            model: "jev-1".into(),
            question_version: QuestionVersion("qv-1".into()),
            policy_version: ConfigVersion("cfg-1".into()),
            outcome: JudgmentOutcome::Answered,
        },
        judgments,
    }
}

pub(super) fn noul(question: Question, yes: f64) -> Judgment {
    noul_with_threshold(question, yes, None)
}

pub(super) fn noul_with_threshold(
    question: Question,
    yes: f64,
    threshold: Option<f64>,
) -> Judgment {
    Judgment {
        question,
        probabilities: BTreeMap::from([(String::from("yes"), Probability(yes))]),
        answer: String::from(if yes >= 0.5 { "yes" } else { "no" }),
        threshold,
    }
}

// ---- Transition shape helpers ----

pub(super) fn is_quiet(t: &Transition) -> bool {
    t.state_changes.is_empty() && t.events.is_empty() && t.effects.is_empty()
}

pub(super) fn updated_records(t: &Transition) -> Vec<&Run> {
    t.state_changes
        .iter()
        .filter_map(|change| match change {
            StateChange::UpdateRun(update) => Some(&update.record),
            StateChange::BindCaller(_)
            | StateChange::RecordLaunch(_)
            | StateChange::ReserveRun(_)
            | StateChange::ChangeOwner(_)
            | StateChange::WriteEffect(_)
            | StateChange::RecordFollowUp(_)
            | StateChange::ExpireFollowUps { .. }
            | StateChange::RecordRecovery(_)
            | StateChange::SetCooldown(_)
            | StateChange::FreezeHandoff(_)
            | StateChange::AckEvent(_) => None,
        })
        .collect()
}

pub(super) fn updated_run(t: &Transition) -> &Run {
    let records = updated_records(t);
    assert_eq!(records.len(), 1, "exactly one UpdateRun expected");
    records[0]
}

pub(super) fn effect_writes(t: &Transition) -> Vec<(&str, EffectState)> {
    t.state_changes
        .iter()
        .filter_map(|change| match change {
            StateChange::WriteEffect(write) => Some((write.key.0.as_str(), write.state)),
            StateChange::BindCaller(_)
            | StateChange::RecordLaunch(_)
            | StateChange::ReserveRun(_)
            | StateChange::UpdateRun(_)
            | StateChange::ChangeOwner(_)
            | StateChange::RecordFollowUp(_)
            | StateChange::ExpireFollowUps { .. }
            | StateChange::RecordRecovery(_)
            | StateChange::SetCooldown(_)
            | StateChange::FreezeHandoff(_)
            | StateChange::AckEvent(_) => None,
        })
        .collect()
}

pub(super) fn frozen_writes(t: &Transition) -> Vec<&FrozenHandoff> {
    t.state_changes
        .iter()
        .filter_map(|change| match change {
            StateChange::FreezeHandoff(handoff) => Some(handoff),
            StateChange::BindCaller(_)
            | StateChange::RecordLaunch(_)
            | StateChange::ReserveRun(_)
            | StateChange::UpdateRun(_)
            | StateChange::ChangeOwner(_)
            | StateChange::WriteEffect(_)
            | StateChange::RecordFollowUp(_)
            | StateChange::ExpireFollowUps { .. }
            | StateChange::RecordRecovery(_)
            | StateChange::SetCooldown(_)
            | StateChange::AckEvent(_) => None,
        })
        .collect()
}

pub(super) fn event_kinds(t: &Transition) -> Vec<MailboxEventKind> {
    t.events.iter().map(|event| event.kind).collect()
}

pub(super) fn event_dedups(t: &Transition) -> Vec<&str> {
    t.events
        .iter()
        .map(|event| event.dedup_key.0.as_str())
        .collect()
}

pub(super) fn effect_keys(t: &Transition) -> Vec<&str> {
    t.effects
        .iter()
        .map(|effect| effect.key.0.as_str())
        .collect()
}

pub(super) fn settlement_of(record: &Run) -> Option<Settlement> {
    record.settlement
}

pub(super) const NOW: Timestamp = Timestamp(500);

pub(super) const EMPTY_READ: (Option<&Decision>, &[Effect], &[FrozenHandoff]) = (None, &[], &[]);

pub(super) fn transact(run: &Run, event: &Versioned<Event>) -> Transition {
    transition(
        run,
        event,
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/state/handoffs/new",
    )
}
