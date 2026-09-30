use super::{
    DeadlineKind, Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult,
    EffectState, EffectTarget, Event, JudgmentVerdict, PromptCertainty, Run, Settlement, State,
    StateChange, TRANSITION_RULES, Transition, UnresolvedReason, VersionTriple, Versioned,
    periodic_review, settle, transition,
};
use crate::acceptance::{FrozenHandoff, HandoffReading};
use crate::config::{ConfigVersion, OperatingPointId, Policy, Provider, Tier};
use crate::delivery::{ExpiryReason, MailboxEventKind};
use crate::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, Digest, EffectKey, HerdrIncarnation,
    JudgmentSetId, LaunchId, NativeSession, PaneId, RunId, TabId, TerminalId, Timestamp,
};
use crate::identity::{ChildStatus, Observation};
use crate::recovery::{RecoveryOrigin, RecoveryStatus};
use crate::routing::{
    Candidate, Decision, Exploration, Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord,
    JudgmentSet, PlacementPlan, Probability, Question, QuestionVersion,
};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

#[test]
fn appendix_c_state_spellings() {
    assert_eq!(State::Reserved.as_str(), "reserved");
    assert_eq!(State::Starting.as_str(), "starting");
    assert_eq!(State::Prompting.as_str(), "prompting");
    assert_eq!(State::Active.as_str(), "active");
    assert_eq!(State::Judging.as_str(), "judging");
    assert_eq!(State::Repair.as_str(), "repair");
    assert_eq!(State::Settled.as_str(), "settled");
}

#[test]
fn f16_prompt_certainty_spellings() {
    assert_eq!(PromptCertainty::Acknowledged.as_str(), "acknowledged");
    assert_eq!(PromptCertainty::Unconfirmed.as_str(), "unconfirmed");
}

#[test]
fn appendix_c_deadline_kind_spellings() {
    assert_eq!(DeadlineKind::Idle.as_str(), "idle");
    assert_eq!(DeadlineKind::Repair.as_str(), "repair");
    assert_eq!(DeadlineKind::Judgment.as_str(), "judgment");
    assert_eq!(DeadlineKind::MaxAge.as_str(), "max_age");
}

#[test]
fn f20_unresolved_reason_spellings() {
    assert_eq!(
        UnresolvedReason::LaunchNotStarted.as_str(),
        "launch_not_started"
    );
    assert_eq!(UnresolvedReason::LaunchFailed.as_str(), "launch_failed");
    assert_eq!(
        UnresolvedReason::JudgmentUnavailable.as_str(),
        "judgment_unavailable"
    );
    assert_eq!(
        UnresolvedReason::IdentityUnprovable.as_str(),
        "identity_unprovable"
    );
    assert_eq!(UnresolvedReason::MaxAge.as_str(), "max_age");
}

#[test]
fn f20_settlement_spellings() {
    assert_eq!(Settlement::Accepted.as_str(), "accepted");
    assert_eq!(Settlement::Rejected.as_str(), "rejected");
    assert_eq!(Settlement::NoHandoff.as_str(), "no_handoff");
    assert_eq!(Settlement::PaneLost.as_str(), "pane_lost");
    assert_eq!(Settlement::Cancelled.as_str(), "cancelled");
    assert_eq!(Settlement::ProviderLimited.as_str(), "provider_limited");
    // `unresolved` never carries its reason in the settlement spelling —
    // the reason rides `runs.settlement_reason` (Appendix B).
    for reason in [
        UnresolvedReason::LaunchNotStarted,
        UnresolvedReason::LaunchFailed,
        UnresolvedReason::JudgmentUnavailable,
        UnresolvedReason::IdentityUnprovable,
        UnresolvedReason::MaxAge,
    ] {
        assert_eq!(Settlement::Unresolved { reason }.as_str(), "unresolved");
    }
}

#[test]
fn f8_effect_kind_spellings() {
    assert_eq!(EffectKind::JevEvaluate.as_str(), "jev_evaluate");
    assert_eq!(EffectKind::TabCreate.as_str(), "tab_create");
    assert_eq!(EffectKind::PaneSplit.as_str(), "pane_split");
    assert_eq!(EffectKind::AgentStart.as_str(), "agent_start");
    assert_eq!(EffectKind::Prompt.as_str(), "prompt");
    assert_eq!(EffectKind::Close.as_str(), "close");
}

#[test]
fn f8_effect_state_spellings() {
    assert_eq!(EffectState::Planned.as_str(), "planned");
    assert_eq!(EffectState::Dispatching.as_str(), "dispatching");
    assert_eq!(EffectState::Acknowledged.as_str(), "acknowledged");
    assert_eq!(EffectState::Failed.as_str(), "failed");
    assert_eq!(EffectState::Unconfirmed.as_str(), "unconfirmed");
}

#[test]
fn f8_effect_certainty_spellings() {
    assert_eq!(EffectCertainty::Absent.as_str(), "absent");
    assert_eq!(EffectCertainty::Unknown.as_str(), "unknown");
}

#[test]
fn f8_f14_effect_target_matches_kind() {
    let identity = ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-1".into()),
        agent_name: AgentName("gov-deadbeef".into()),
        native_session: Some(NativeSession("sess-1".into())),
        pane_id: PaneId("w6:p2".into()),
    };
    // Every variant pairs with the spec kind that addresses it — the
    // exhaustive match is the shape pin (F8/F10/F14).
    let kind_of = |target: &EffectTarget| match target {
        EffectTarget::ExistingTab(_) => EffectKind::PaneSplit,
        EffectTarget::CallerContext(_) => EffectKind::TabCreate,
        EffectTarget::AgentPane(_) => EffectKind::AgentStart,
        EffectTarget::Child(_) => EffectKind::Prompt,
    };
    let cases = [
        (
            EffectTarget::ExistingTab(TabId("t1".into())),
            EffectKind::PaneSplit,
        ),
        (
            EffectTarget::CallerContext(PaneId("w6:p1".into())),
            EffectKind::TabCreate,
        ),
        (
            EffectTarget::AgentPane(PlacementPlan::ExistingTab {
                tab: TabId("t1".into()),
            }),
            EffectKind::AgentStart,
        ),
        // A NewTab placement's agent pane comes from tab_create's
        // initial pane — it never maps to pane_split (F14/H#102).
        (
            EffectTarget::AgentPane(PlacementPlan::NewTab),
            EffectKind::AgentStart,
        ),
        (EffectTarget::Child(identity), EffectKind::Prompt),
    ];
    for (target, kind) in cases {
        assert_eq!(
            kind_of(&target),
            kind,
            "target must pair with its spec kind"
        );
    }
}

#[test]
fn f14_new_tab_initial_pane_is_used() {
    // F14/H#102 — tab_create's receipt reports the initial pane so a
    // NewTab plan's agent_start launches into it (no pane_split).
    let receipt = EffectReceipt::TabCreated {
        tab: TabId("t9".into()),
        pane: PaneId("t9:p1".into()),
    };
    let initial = match receipt {
        EffectReceipt::TabCreated { tab: _, pane } => Some(pane),
        EffectReceipt::AgentStarted { .. }
        | EffectReceipt::Judgments(_)
        | EffectReceipt::PaneCreated { .. } => None,
    };
    assert_eq!(initial, Some(PaneId("t9:p1".into())));
}

// ======================================================================
// The transition function — F20/F22/F23/F25.
// ======================================================================

fn test_policy() -> Policy {
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

fn identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-1".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-1".into())),
        pane_id: PaneId("w0:p1".into()),
    }
}

fn run_in(state: State) -> Run {
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

fn triple(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

fn stamped(run: &Run, value: Event) -> Versioned<Event> {
    Versioned {
        requested_against: triple(run),
        value,
    }
}

fn stale_stamped(run: &Run, value: Event) -> Versioned<Event> {
    let mut stamp = triple(run);
    stamp.version = stamp.version.saturating_add(1);
    Versioned {
        requested_against: stamp,
        value,
    }
}

fn obs_unique(status: Option<ChildStatus>) -> Event {
    Event::Obs {
        observation: Observation::Unique {
            status,
            pane: PaneId("w0:p9".into()),
            native_session: None,
        },
        handoff_reading: None,
    }
}

fn journal_effect_at(
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

fn journal_effect(run: &Run, suffix: &str, kind: EffectKind, state: EffectState) -> Effect {
    journal_effect_at(run, suffix, kind, state, None)
}

fn result_event(
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

fn run_result(
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

fn frozen(run: &Run, work_generation: u64, digest: u8) -> FrozenHandoff {
    FrozenHandoff {
        run: run.id.clone(),
        work_generation,
        digest: Digest([digest; 32]),
        frozen_path: "/state/handoffs/r-1".into(),
        frozen_at: Timestamp(0),
    }
}

fn candidate(index: usize) -> Candidate {
    Candidate {
        operating_point: OperatingPointId(format!("op-{index}")),
        provider: Provider(format!("prov-{index}")),
        tier: Tier(format!("t{index}")),
        harness: AgentKind("kind".into()),
        args: Vec::new(),
    }
}

fn decision(count: usize) -> Decision {
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

fn review_record(run: &Run, judgments: Vec<Judgment>) -> JudgmentRecord {
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

fn noul(question: Question, yes: f64) -> Judgment {
    noul_with_threshold(question, yes, None)
}

fn noul_with_threshold(question: Question, yes: f64, threshold: Option<f64>) -> Judgment {
    Judgment {
        question,
        probabilities: BTreeMap::from([(String::from("yes"), Probability(yes))]),
        answer: String::from(if yes >= 0.5 { "yes" } else { "no" }),
        threshold,
    }
}

// ---- Transition shape helpers ----

fn is_quiet(t: &Transition) -> bool {
    t.state_changes.is_empty() && t.events.is_empty() && t.effects.is_empty()
}

fn updated_records(t: &Transition) -> Vec<&Run> {
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

fn updated_run(t: &Transition) -> &Run {
    let records = updated_records(t);
    assert_eq!(records.len(), 1, "exactly one UpdateRun expected");
    records[0]
}

fn effect_writes(t: &Transition) -> Vec<(&str, EffectState)> {
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

fn frozen_writes(t: &Transition) -> Vec<&FrozenHandoff> {
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

fn event_kinds(t: &Transition) -> Vec<MailboxEventKind> {
    t.events.iter().map(|event| event.kind).collect()
}

fn event_dedups(t: &Transition) -> Vec<&str> {
    t.events
        .iter()
        .map(|event| event.dedup_key.0.as_str())
        .collect()
}

fn effect_keys(t: &Transition) -> Vec<&str> {
    t.effects
        .iter()
        .map(|effect| effect.key.0.as_str())
        .collect()
}

fn settlement_of(record: &Run) -> Option<Settlement> {
    record.settlement
}

const NOW: Timestamp = Timestamp(500);

const EMPTY_READ: (Option<&Decision>, &[Effect], &[FrozenHandoff]) = (None, &[], &[]);

fn transact(run: &Run, event: &Versioned<Event>) -> Transition {
    transition(
        run,
        event,
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/state/handoffs/new",
    )
}

// ---------------- F20 — settlement ----------------

#[test]
fn f20_settlement_first_commit_wins() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::Cancelled, NOW, &test_policy());
    let record = updated_run(&t);
    assert_eq!(record.state, State::Settled);
    assert_eq!(settlement_of(record), Some(Settlement::Cancelled));
    assert_eq!(record.settled_at, Some(NOW));
    assert_eq!(record.version, run.version.saturating_add(1));
    assert!(
        t.state_changes.iter().any(|c| matches!(
            c,
            StateChange::ExpireFollowUps {
                reason: ExpiryReason::RunSettled,
                ..
            }
        )),
        "settle must expire never-dispatched follow-ups in the same transaction"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::Settled]),
        "the terminal event rides the settle transaction"
    );
    assert_eq!(event_dedups(&t), Vec::from(["run:r-1:settled"]));

    // the losing side: a Run that is already settled produces nothing.
    let mut settled_run = run_in(State::Settled);
    settled_run.settlement = Some(Settlement::Accepted);
    settled_run.settled_at = Some(Timestamp(1));
    let loser = settle(&settled_run, Settlement::Cancelled, NOW, &test_policy());
    assert!(is_quiet(&loser), "a settled Run produces nothing");
}

#[test]
fn f20_provider_limited_settlement_records_recovery_and_cooldown() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::ProviderLimited, NOW, &test_policy());
    let record = updated_run(&t);
    assert_eq!(settlement_of(record), Some(Settlement::ProviderLimited));
    let recovery = t.state_changes.iter().find_map(|c| match c {
        StateChange::RecordRecovery(o) => Some(o),
        StateChange::BindCaller(_)
        | StateChange::RecordLaunch(_)
        | StateChange::ReserveRun(_)
        | StateChange::UpdateRun(_)
        | StateChange::ChangeOwner(_)
        | StateChange::WriteEffect(_)
        | StateChange::RecordFollowUp(_)
        | StateChange::ExpireFollowUps { .. }
        | StateChange::SetCooldown(_)
        | StateChange::FreezeHandoff(_)
        | StateChange::AckEvent(_) => None,
    });
    let obligation = recovery.expect("provider_limited must record a recovery obligation");
    assert_eq!(obligation.predecessor, run.id);
    assert_eq!(obligation.origin, RecoveryOrigin::ProviderLimit);
    assert_eq!(obligation.status, RecoveryStatus::Pending);
    assert_eq!(obligation.expires_at, Timestamp(86_400_500));
    let cooldown = t.state_changes.iter().find_map(|c| match c {
        StateChange::SetCooldown(cd) => Some(cd),
        StateChange::BindCaller(_)
        | StateChange::RecordLaunch(_)
        | StateChange::ReserveRun(_)
        | StateChange::UpdateRun(_)
        | StateChange::ChangeOwner(_)
        | StateChange::WriteEffect(_)
        | StateChange::RecordFollowUp(_)
        | StateChange::ExpireFollowUps { .. }
        | StateChange::RecordRecovery(_)
        | StateChange::FreezeHandoff(_)
        | StateChange::AckEvent(_) => None,
    });
    let cooldown_row = cooldown.expect("provider_limited must cool the provider down");
    assert_eq!(cooldown_row.provider, Provider("prov-1".into()));
    assert_eq!(cooldown_row.until, Timestamp(3_600_500));
    assert_eq!(cooldown_row.source_run, Some(run.id.clone()));
    assert_eq!(
        event_kinds(&t),
        Vec::from([
            MailboxEventKind::CooldownHit,
            MailboxEventKind::RecoveryPending,
            MailboxEventKind::Settled,
        ])
    );
}

#[test]
fn f20_provider_limited_without_provider_skips_cooldown() {
    let mut run = run_in(State::Starting);
    run.provider = None;
    let t = settle(&run, Settlement::ProviderLimited, NOW, &test_policy());
    assert!(
        !t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::SetCooldown(_))),
        "no provider → no cooldown row"
    );
    assert!(
        t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::RecordRecovery(_))),
        "the obligation is recorded regardless"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::RecoveryPending, MailboxEventKind::Settled])
    );
}

#[test]
fn f20_accepted_and_rejected_emit_their_events() {
    let run = run_in(State::Judging);
    let t = settle(&run, Settlement::Accepted, NOW, &test_policy());
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffAccepted, MailboxEventKind::Settled])
    );
    let t_reject = settle(&run, Settlement::Rejected, NOW, &test_policy());
    assert_eq!(
        event_kinds(&t_reject),
        Vec::from([MailboxEventKind::HandoffRejected, MailboxEventKind::Settled])
    );
}

#[test]
fn f20_stamped_events_drop_when_versions_moved() {
    let run = run_in(State::Active);
    let events = Vec::from([
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([7; 32]),
        },
        Event::Judgment(JudgmentVerdict::Accept),
        Event::Deadline(DeadlineKind::Idle),
        Event::ProviderLimited,
    ]);
    for event in events {
        let t = transition(
            &run,
            &stale_stamped(&run, event),
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert!(is_quiet(&t), "a stale async result must produce nothing");
    }
    // work_generation and evidence_generation mismatches drop too.
    let mut stamp = triple(&run);
    stamp.work_generation = stamp.work_generation.saturating_add(1);
    let t = transition(
        &run,
        &Versioned {
            requested_against: stamp,
            value: obs_unique(Some(ChildStatus::Working)),
        },
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t), "a stale work_generation must produce nothing");
}

#[test]
fn f20_synchronous_events_apply_regardless_of_stamp() {
    let run = run_in(State::Active);
    // cancel, restart and effect_result are not version-gated (F20: only Jev
    // results, observations and deadlines carry the triple).
    let t = transact(
        &run,
        &stale_stamped(&run, Event::Cancel { close_pane: false }),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Cancelled),
        "cancel applies even with a stale stamp"
    );
    let t_restart = transact(&run, &stale_stamped(&run, Event::Restart));
    assert!(is_quiet(&t_restart));
    let t_journal = transact(
        &run,
        &stale_stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert_eq!(
        effect_writes(&t_journal),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)]),
        "the journal write is durable fact"
    );
}

#[test]
fn f20_cancel_on_unsettled_settles_cancelled() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
    ] {
        let run = run_in(state);
        let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: false }));
        let record = updated_run(&t);
        assert_eq!(
            settlement_of(record),
            Some(Settlement::Cancelled),
            "cancel settles any unsettled Run"
        );
        assert!(t.effects.is_empty(), "no close was requested");
    }
}

#[test]
fn f20_cancel_with_close_pane_plans_one_verified_close() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: true }));
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:close"]));
    let effect = &t.effects[0];
    assert_eq!(effect.kind, EffectKind::Close);
    assert_eq!(
        effect.target,
        Some(EffectTarget::Child(identity())),
        "the close carries the captured identity to verify against"
    );
    // a second cancel never re-plans the close — the key dedups.
    let journal = Vec::from([journal_effect(
        &run,
        "close",
        EffectKind::Close,
        EffectState::Planned,
    )]);
    let t_again = transition(
        &run,
        &stamped(&run, Event::Cancel { close_pane: true }),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        t_again.effects.is_empty(),
        "an already-planned close is not re-planned"
    );
    assert_eq!(
        settlement_of(updated_run(&t_again)),
        Some(Settlement::Cancelled)
    );
}

#[test]
fn f20_settled_accepts_only_cancel_with_close_pane() {
    let mut run = run_in(State::Settled);
    run.settlement = Some(Settlement::Accepted);
    run.settled_at = Some(Timestamp(1));
    // any other event is ignored — settlement is immutable.
    for event in [
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([1; 32]),
        },
        Event::Judgment(JudgmentVerdict::Reject),
        Event::Deadline(DeadlineKind::MaxAge),
        Event::Cancel { close_pane: false },
        Event::ProviderLimited,
        Event::Restart,
    ] {
        let t = transition(
            &run,
            &Versioned {
                requested_against: triple(&run),
                value: event,
            },
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert!(
            is_quiet(&t),
            "settled ignores everything but cancel+closePane"
        );
    }
    let t = transact(&run, &stamped(&run, Event::Cancel { close_pane: true }));
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:close"]));
    assert!(
        updated_records(&t).is_empty(),
        "settled cancel writes no run row"
    );
}

// ---------------- F22 — the total transition function ----------------

#[test]
fn f22_total_transition_table() {
    // every state × every event kind returns a Transition — total, never a panic.
    let states = [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ];
    for state in states {
        let run = run_in(state);
        let events = Vec::from([
            obs_unique(Some(ChildStatus::Working)),
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
            Event::Obs {
                observation: Observation::Invalid,
                handoff_reading: None,
            },
            Event::Handoff {
                digest: Digest([3; 32]),
            },
            Event::Judgment(JudgmentVerdict::Accept),
            Event::Deadline(DeadlineKind::MaxAge),
            Event::Deadline(DeadlineKind::Idle),
            Event::Deadline(DeadlineKind::Repair),
            Event::Deadline(DeadlineKind::Judgment),
            Event::Cancel { close_pane: false },
            Event::Cancel { close_pane: true },
            Event::ProviderLimited,
            run_result(
                &run,
                "x",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
            Event::Restart,
        ]);
        for event in events {
            let _transition = transition(
                &run,
                &stamped(&run, event),
                NOW,
                &test_policy(),
                (Some(&decision(1)), &[], &[]),
                "/fp",
            );
        }
    }
}

#[test]
fn f22_rule_list_is_exposed_as_data() {
    assert!(
        TRANSITION_RULES.len() >= 30,
        "the Appendix C rule list renders the whole table"
    );
    assert!(
        TRANSITION_RULES
            .iter()
            .any(|(s, e, _)| *s == "settled" && *e == "cancel(closePane)"),
        "the settled-state close rule is listed"
    );
    assert!(
        TRANSITION_RULES
            .iter()
            .any(|(s, e, _)| *s == "*" && *e == "obs(invalid)"),
        "the all-state invalid rule is listed"
    );
}

#[test]
fn f22_obs_invalid_changes_nothing_in_every_state() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ] {
        let run = run_in(state);
        let t = transact(
            &run,
            &stamped(
                &run,
                Event::Obs {
                    observation: Observation::Invalid,
                    handoff_reading: None,
                },
            ),
        );
        assert!(is_quiet(&t), "obs(invalid) changes nothing");
    }
}

#[test]
fn f22_max_age_settles_any_unsettled_run() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
    ] {
        let run = run_in(state);
        let t = transition(
            &run,
            &stamped(&run, Event::Deadline(DeadlineKind::MaxAge)),
            Timestamp(1_000),
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert_eq!(
            settlement_of(updated_run(&t)),
            Some(Settlement::Unresolved {
                reason: UnresolvedReason::MaxAge
            }),
            "max_age settles every unsettled Run"
        );
    }
    // before the deadline it is a no-op (the scheduler fired early).
    let run = run_in(State::Active);
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::MaxAge)),
        Timestamp(999),
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t), "a premature deadline event applies nothing");
}

#[test]
fn f22_restart_converts_dispatching_to_unconfirmed() {
    let run = run_in(State::Active);
    let journal = Vec::from([
        journal_effect(
            &run,
            "prompt:task",
            EffectKind::Prompt,
            EffectState::Acknowledged,
        ),
        journal_effect(
            &run,
            "nudge:0",
            EffectKind::Prompt,
            EffectState::Dispatching,
        ),
        journal_effect(
            &run,
            "review:0",
            EffectKind::JevEvaluate,
            EffectState::Dispatching,
        ),
        journal_effect(&run, "close", EffectKind::Close, EffectState::Planned),
    ]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([
            ("run:r-1:nudge:0", EffectState::Unconfirmed),
            ("run:r-1:review:0", EffectState::Unconfirmed),
        ]),
        "dispatching effects become unconfirmed — never re-dispatched"
    );
    // planned effects keep dispatching later; acknowledged rows are durable.
}

#[test]
fn f22_restart_never_changes_deadlines() {
    let mut run = run_in(State::Judging);
    run.judgment_deadline = Some(Timestamp(800));
    run.repair_deadline = Some(Timestamp(700));
    run.idle_deadline = Some(Timestamp(600));
    let journal = Vec::from([journal_effect(
        &run,
        "accept:0:1",
        EffectKind::JevEvaluate,
        EffectState::Dispatching,
    )]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "restart writes no run row outside the prompting rule"
    );
}

#[test]
fn f22_restart_re_derives_prompting_from_the_journal() {
    let run = run_in(State::Prompting);
    let journal = Vec::from([journal_effect(
        &run,
        "prompt:task",
        EffectKind::Prompt,
        EffectState::Dispatching,
    )]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.prompt_certainty,
        Some(PromptCertainty::Unconfirmed),
        "a dispatching task prompt that went unconfirmed promotes prompting → active (F8/F16)"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::PromptUnconfirmed])
    );
    // if the prompt was only `planned`, nothing promotes.
    let planned_journal = Vec::from([journal_effect(
        &run,
        "prompt:task",
        EffectKind::Prompt,
        EffectState::Planned,
    )]);
    let t_planned = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &planned_journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_planned),
        "a still-planned prompt keeps prompting"
    );
}

#[test]
fn f22_reserved_absent_is_launch_not_started() {
    let run = run_in(State::Reserved);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchNotStarted
        }),
        "the pane that would host the child vanished before it started"
    );
}

#[test]
fn f22_reserved_ignores_the_rest() {
    let run = run_in(State::Reserved);
    for event in [
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([1; 32]),
        },
        Event::Judgment(JudgmentVerdict::Accept),
        Event::Deadline(DeadlineKind::Idle),
    ] {
        let t = transact(&run, &stamped(&run, event));
        assert!(is_quiet(&t), "reserved has no other answers");
    }
}

#[test]
fn f22_starting_start_acknowledged_goes_prompting() {
    let run = run_in(State::Starting);
    let started_receipt = EffectReceipt::AgentStarted {
        identity: identity(),
    };
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(started_receipt),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Prompting);
    assert_eq!(record.identity, Some(identity()));
    assert_eq!(
        record.operating_point,
        Some(OperatingPointId("op-0".into())),
        "the started candidate's point is recorded"
    );
    assert_eq!(record.provider, Some(Provider("prov-0".into())));
    assert_eq!(record.tier_start, Some(Tier("t0".into())));
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:start:0", EffectState::Acknowledged)])
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:prompt:task"]));
    assert_eq!(t.effects[0].kind, EffectKind::Prompt);
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::Child(identity())),
        "the task prompt targets the captured identity"
    );
}

#[test]
fn f22_starting_topology_acknowledgement_plans_first_start() {
    let run = run_in(State::Reserved);
    // a new tab's initial pane hosts the child — no split is planned (H#102).
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "tab:create",
                EffectKind::TabCreate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::TabCreated {
                    tab: TabId("t9".into()),
                    pane: PaneId("t9:p1".into()),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &[], &[]),
        "/fp",
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:start:0"]));
    assert_eq!(t.effects[0].kind, EffectKind::AgentStart);
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab))
    );

    // a split into a picked tab starts into the resulting pane plan.
    let journal = Vec::from([journal_effect_at(
        &run,
        "split",
        EffectKind::PaneSplit,
        EffectState::Dispatching,
        Some(EffectTarget::ExistingTab(TabId("t3".into()))),
    )]);
    let t_split = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "split",
                EffectKind::PaneSplit,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::PaneCreated {
                    pane: PaneId("t3:p4".into()),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    assert_eq!(
        t_split.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::ExistingTab {
            tab: TabId("t3".into())
        })),
        "the start plans into the tab the split ran in"
    );
}

#[test]
fn f22_starting_pre_interactive_failure_tries_next_candidate() {
    let run = run_in(State::Starting);
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::PreInteractiveFailed,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:start:1"]));
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        "the next candidate tries the same pane (F15)"
    );
    // the failed start journals absent.
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:start:0", EffectState::Failed)])
    );
    assert!(
        updated_records(&t).is_empty(),
        "the Run stays starting for the retry"
    );
}

#[test]
fn f22_starting_failure_with_no_candidates_stays() {
    let run = run_in(State::Starting);
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::PreInteractiveFailed,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(1)), &journal, &[]),
        "/fp",
    );
    assert!(
        t.effects.is_empty(),
        "no next candidate → nothing is planned; the Run waits on obs(absent) or max_age"
    );
    assert!(updated_records(&t).is_empty());
}

#[test]
fn f22_starting_unconfirmed_and_failed_stay_starting() {
    let run = run_in(State::Starting);
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent,
        },
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let journal = Vec::from([journal_effect_at(
            &run,
            "start:0",
            EffectKind::AgentStart,
            EffectState::Dispatching,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        )]);
        let t = transition(
            &run,
            &stamped(
                &run,
                run_result(&run, "start:0", EffectKind::AgentStart, outcome, None),
            ),
            NOW,
            &test_policy(),
            (Some(&decision(2)), &journal, &[]),
            "/fp",
        );
        assert!(t.effects.is_empty(), "no fallback without proof");
        assert!(updated_records(&t).is_empty(), "the Run stays starting");
        assert_eq!(effect_writes(&t).len(), 1, "the result still journals");
    }
}

#[test]
fn f22_starting_absent_is_launch_failed() {
    let run = run_in(State::Starting);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed
        })
    );
}

#[test]
fn f22_prompting_acknowledged_goes_active() {
    let run = run_in(State::Prompting);
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(record.prompt_certainty, Some(PromptCertainty::Acknowledged));
}

#[test]
fn f22_prompting_unconfirmed_goes_active_unconfirmed() {
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::PreInteractiveFailed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let run = run_in(State::Prompting);
        let t = transact(
            &run,
            &stamped(
                &run,
                run_result(&run, "prompt:task", EffectKind::Prompt, outcome, None),
            ),
        );
        let record = updated_run(&t);
        assert_eq!(record.state, State::Active);
        assert_eq!(
            record.prompt_certainty,
            Some(PromptCertainty::Unconfirmed),
            "a possibly-consumed prompt records unconfirmed and never resubmits"
        );
        assert_eq!(
            event_kinds(&t),
            Vec::from([MailboxEventKind::PromptUnconfirmed])
        );
    }
}

#[test]
fn f22_prompting_absent_is_pane_lost() {
    let run = run_in(State::Prompting);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::PaneLost));
}

#[test]
fn f22_active_working_ends_the_episode() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    run.nudged_episode = Some(0);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    let record = updated_run(&t);
    assert_eq!(record.child_status, Some(ChildStatus::Working));
    assert_eq!(record.idle_since, None, "the episode closes");
    assert_eq!(record.idle_deadline, None);
    assert_eq!(
        record.nudge_episode, 1,
        "the next stall opens a new episode"
    );
    // and the identity's locator follows the pane the observation read.
    assert_eq!(
        record.identity.as_ref().map(|i| i.pane_id.clone()),
        Some(PaneId("w0:p9".into()))
    );
}

#[test]
fn f22_active_working_without_episode_only_records_status() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    let record = updated_run(&t);
    assert_eq!(
        record.nudge_episode, 0,
        "no episode was open — nothing bumps"
    );
    assert_eq!(record.child_status, Some(ChildStatus::Working));
    // an identical repeated observation is a true no-op: no version bump, so
    // in-flight stamped results stay valid (F20).
    let repeat = transact(
        record,
        &stamped(record, obs_unique(Some(ChildStatus::Working))),
    );
    assert!(is_quiet(&repeat), "an unchanged observation writes nothing");
}

#[test]
fn f22_active_blocked_asks_the_supervision_questions() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Blocked))));
    assert_eq!(
        effect_keys(&t),
        Vec::from(["run:r-1:review:0"]),
        "a blocked child is asked blocked_on_input and provider_limited"
    );
    assert_eq!(t.effects[0].kind, EffectKind::JevEvaluate);
    // the same evidence is never re-asked.
    let journal = Vec::from([journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    )]);
    let t_repeat = transition(
        &run,
        &stamped(&run, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        t_repeat.effects.is_empty(),
        "unchanged evidence is never re-asked"
    );
}

#[test]
fn f22_active_handoff_freezes_and_judges() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/state/handoffs/frozen-1",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    assert_eq!(
        record.evidence_generation, 1,
        "the freeze bumps evidence_generation"
    );
    assert_eq!(
        record.judgment_deadline,
        Some(Timestamp(1_800_500)),
        "judgment_deadline arms at the freeze"
    );
    assert_eq!(
        record.idle_since, None,
        "the handoff moots the idle episode"
    );
    let handoffs = frozen_writes(&t);
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].digest, Digest([9; 32]));
    assert_eq!(handoffs[0].work_generation, 0);
    assert_eq!(handoffs[0].frozen_path, "/state/handoffs/frozen-1");
    assert_eq!(handoffs[0].frozen_at, NOW);
    assert_eq!(
        effect_keys(&t),
        Vec::from(["run:r-1:accept:0:1"]),
        "the acceptance assessment is planned for the new binding"
    );
}

#[test]
fn f22_active_absent_reads_the_handoff_once() {
    let run = run_in(State::Active);
    // a valid marked file on the one-shot read freezes and judges (F25).
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: Some(HandoffReading::Valid {
                    digest: Digest([4; 32]),
                }),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(updated_run(&t).state, State::Judging);
    assert_eq!(frozen_writes(&t).len(), 1);
}

#[test]
fn f22_active_absent_without_handoff_is_pane_lost() {
    let run = run_in(State::Active);
    for reading in [None, Some(HandoffReading::NotWritten)] {
        let t = transition(
            &run,
            &stamped(
                &run,
                Event::Obs {
                    observation: Observation::Absent,
                    handoff_reading: reading,
                },
            ),
            NOW,
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert_eq!(
            settlement_of(updated_run(&t)),
            Some(Settlement::PaneLost),
            "no frozen handoff and no valid reading → pane_lost"
        );
    }
}

#[test]
fn f22_active_absent_with_frozen_handoff_judges() {
    let run = run_in(State::Active);
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.state,
        State::Judging,
        "an unjudged frozen handoff still goes to judgment"
    );
    assert!(
        frozen_writes(&t).is_empty(),
        "the handoff is already frozen — no second freeze row"
    );
    assert!(
        t.effects.is_empty(),
        "the assessment was planned at freeze time"
    );
}

#[test]
fn f22_judging_accept_settles_accepted() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
    );
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::Accepted));
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffAccepted, MailboxEventKind::Settled])
    );
}

#[test]
fn f22_judging_reject_enters_repair_and_arms_deadline_once() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Reject)),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Repair);
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(900_500)),
        "the repair window arms on the first rejection"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffRejected])
    );
    assert_eq!(
        event_dedups(&t),
        Vec::from(["run:r-1:handoff_rejected:0:1"]),
        "the verdict event is scoped to the rejected binding"
    );

    // a second rejection in the same work generation never extends it (F24).
    let mut again = run_in(State::Judging);
    again.evidence_generation = 2;
    again.repair_deadline = Some(Timestamp(600));
    let t_again = transition(
        &again,
        &stamped(&again, Event::Judgment(JudgmentVerdict::Reject)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_again).repair_deadline,
        Some(Timestamp(600)),
        "an armed repair_deadline is preserved, never reset"
    );
}

#[test]
fn f22_judging_unavailable_stays_until_deadline() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Unavailable)),
    );
    assert!(is_quiet(&t), "an armed deadline already bounds the wait");
    // with no deadline armed the transition arms it.
    let mut unarmed = run_in(State::Judging);
    unarmed.evidence_generation = 1;
    let t_unarmed = transition(
        &unarmed,
        &stamped(&unarmed, Event::Judgment(JudgmentVerdict::Unavailable)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_unarmed).judgment_deadline,
        Some(Timestamp(1_800_500))
    );
}

#[test]
fn f22_judging_deadlines() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable
        }),
        "the bound expires → unresolved(judgment_unavailable)"
    );
    // a not-yet-passed judgment deadline is a no-op.
    run.judgment_deadline = Some(Timestamp(600));
    let t_early = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t_early));
    // the repair deadline keeps running through judging (F24).
    run.judgment_deadline = Some(Timestamp(600));
    run.repair_deadline = Some(Timestamp(400));
    let t_repair = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t_repair)),
        Some(Settlement::Rejected),
        "a re-frozen handoff does not extend the repair deadline"
    );
}

#[test]
fn f22_judging_absent_stays_judging() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert!(is_quiet(&t), "judging waits on the judgment, not the pane");
}

#[test]
fn f22_judging_new_digest_refreezes_same_digest_is_ignored() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    // a digest already frozen for this generation is never re-judged (F24).
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert!(is_quiet(&t), "the known digest is not re-frozen");
    // a rewritten handoff is new evidence → freeze again, stay judging.
    let t_rewrite = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([10; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t_rewrite);
    assert_eq!(record.state, State::Judging);
    assert_eq!(record.evidence_generation, 2);
    assert_eq!(frozen_writes(&t_rewrite).len(), 1);
}

#[test]
fn f22_repair_dispatch_before_deadline_advances_generation() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.work_generation, 1,
        "a dispatched repair opens a new work generation"
    );
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.repair_deadline, None,
        "the window resets for the new generation"
    );
}

#[test]
fn f22_repair_dispatch_after_deadline_does_not_count() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(400)); // before NOW
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "a late dispatch does not reopen the generation"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:outbox:3", EffectState::Acknowledged)]),
        "the journal still records what happened"
    );
}

#[test]
fn f22_repair_deadline_settles_rejected() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "the binding expires → rejected (F24)"
    );
    // not yet passed → nothing.
    run.repair_deadline = Some(Timestamp(900));
    let t_early = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t_early));
}

#[test]
fn f22_repair_new_handoff_freezes_keeping_deadline() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([5; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(700)),
        "the repair deadline survives a re-freeze"
    );
    // a digest already judged (it is frozen for this generation) stays repair.
    let handoffs = Vec::from([frozen(&run, 0, 5)]);
    let t_repeat = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([5; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert!(
        is_quiet(&t_repeat),
        "the rejected digest never re-enters judgment"
    );
}

#[test]
fn f22_repair_absent_stays_repair() {
    let run = run_in(State::Repair);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert!(is_quiet(&t), "repair waits out its deadline");
}

#[test]
fn f22_unique_observation_refreshes_the_locator() {
    let run = run_in(State::Judging);
    let session = Some(NativeSession("sess-2".into()));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Unique {
                    status: Some(ChildStatus::Done),
                    pane: PaneId("w1:p2".into()),
                    native_session: session,
                },
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    let identity = record.identity.clone().expect("identity kept");
    assert_eq!(
        identity.pane_id,
        PaneId("w1:p2".into()),
        "a move is followed"
    );
    assert_eq!(
        identity.native_session,
        Some(NativeSession("sess-2".into())),
        "the reported session is captured"
    );
    assert_eq!(record.child_status, Some(ChildStatus::Done));
    assert_eq!(
        record.state,
        State::Judging,
        "status alone never leaves judging"
    );
}

// ---------------- F23 — supervision mapping ----------------

#[test]
fn f23_blocked_on_input_produces_an_event() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::BlockedOnInput])
    );
    assert_eq!(event_dedups(&t), Vec::from(["run:r-1:blocked_on_input:0"]));
    assert!(t.effects.is_empty(), "blocked_on_input never nudges");
}

#[test]
fn f23_blocked_on_input_below_threshold_is_silent() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.2)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert!(t.events.is_empty());
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:review:0", EffectState::Acknowledged)]),
        "the answered set still journals"
    );
}

#[test]
fn f23_no_recent_progress_nudges_once_per_episode() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::NoRecentProgress, 0.9)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:nudge:0"]));
    assert_eq!(t.effects[0].kind, EffectKind::Prompt);
    assert_eq!(t.effects[0].target, Some(EffectTarget::Child(identity())));
    assert_eq!(
        updated_run(&t).nudged_episode,
        Some(0),
        "the episode's one nudge is spent"
    );
    assert!(
        t.events.is_empty(),
        "the first stall nudges, it does not report"
    );

    // the same episode never nudges again — a repeated stall reports stalled.
    let mut run_stalled = run_in(State::Active);
    run_stalled.nudged_episode = Some(0);
    let stalled_record = review_record(
        &run_stalled,
        Vec::from([noul(Question::NoRecentProgress, 0.9)]),
    );
    let t_stalled = transact(
        &run_stalled,
        &stamped(
            &run_stalled,
            run_result(
                &run_stalled,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(stalled_record)),
            ),
        ),
    );
    assert!(
        t_stalled.effects.is_empty(),
        "no second nudge in one episode"
    );
    assert_eq!(
        event_kinds(&t_stalled),
        Vec::from([MailboxEventKind::Stalled])
    );
    assert_eq!(event_dedups(&t_stalled), Vec::from(["run:r-1:stalled:0"]));
}

#[test]
fn f23_provider_limited_above_threshold_settles() {
    let run = run_in(State::Active);
    let record = review_record(
        &run,
        Vec::from([
            noul(Question::BlockedOnInput, 0.9),
            noul(Question::ProviderLimited, 0.9),
        ]),
    );
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::ProviderLimited),
        "a cleared provider_limited settles via the F21 transaction"
    );
    assert!(
        t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::RecordRecovery(_))),
        "the recovery obligation rides the settle"
    );
    // the advisory answers of a terminal settle never emit.
    assert_eq!(
        event_kinds(&t),
        Vec::from([
            MailboxEventKind::CooldownHit,
            MailboxEventKind::RecoveryPending,
            MailboxEventKind::Settled,
        ])
    );
}

#[test]
fn f23_provider_limited_below_threshold_does_not_settle() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::ProviderLimited, 0.5)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert!(updated_records(&t).is_empty());
    assert!(t.events.is_empty());
}

#[test]
fn f23_outside_scope_produces_an_event() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::OutsideScope, 0.9)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(event_kinds(&t), Vec::from([MailboxEventKind::OutsideScope]));
}

#[test]
fn f23_stale_judgment_set_journals_stale_and_applies_nothing() {
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
    let mut stale_versions = triple(&run);
    stale_versions.evidence_generation = stale_versions.evidence_generation.saturating_add(1);
    record.set.versions = Some(stale_versions);
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert!(t.events.is_empty(), "stale answers apply nothing");
    assert!(t.effects.is_empty());
    assert!(
        updated_records(&t).is_empty(),
        "no run write — the version guard is the checkpoint"
    );
    // and the journaled set is marked stale.
    let receipt_is_stale = t.state_changes.iter().any(|c| match c {
        StateChange::WriteEffect(w) => matches!(
            &w.receipt,
            Some(EffectReceipt::Judgments(r)) if r.set.outcome == JudgmentOutcome::Stale
        ),
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
        | StateChange::AckEvent(_) => false,
    });
    assert!(
        receipt_is_stale,
        "the set journals with outcome stale (F20)"
    );
}

#[test]
fn f23_periodic_reviews_pause_while_the_owner_is_absent() {
    let run = run_in(State::Active);
    assert!(
        periodic_review(&run, true, &[]).is_none(),
        "no review while the owner's session is absent"
    );
    let effect = periodic_review(&run, false, &[]).expect("a present owner gets reviews");
    assert_eq!(effect.kind, EffectKind::JevEvaluate);
    assert_eq!(effect.key.0, "run:r-1:review:0");
    // acceptance and deadlines never pause — they run through `transition`,
    // which takes no owner-presence input at all. A deadline fires regardless.
    let mut judging = run_in(State::Judging);
    judging.evidence_generation = 1;
    judging.judgment_deadline = Some(Timestamp(400));
    let t = transact(
        &judging,
        &stamped(&judging, Event::Deadline(DeadlineKind::Judgment)),
    );
    assert!(
        settlement_of(updated_run(&t)).is_some(),
        "deadlines are never paused"
    );
}

#[test]
fn f23_unchanged_evidence_is_never_re_asked() {
    let run = run_in(State::Active);
    // the ask for this generation exists in any state — no second one.
    for state in [
        EffectState::Planned,
        EffectState::Dispatching,
        EffectState::Acknowledged,
        EffectState::Failed,
        EffectState::Unconfirmed,
    ] {
        let journal = Vec::from([journal_effect(
            &run,
            "review:0",
            EffectKind::JevEvaluate,
            state,
        )]);
        assert!(
            periodic_review(&run, false, &journal).is_none(),
            "one review per evidence_generation, whatever its outcome"
        );
    }
    // and never while the run is not being supervised.
    let run_judging = run_in(State::Judging);
    assert!(periodic_review(&run_judging, false, &[]).is_none());
}

// ---------------- F25 — idle and loss ----------------

#[test]
fn f25_idle_episode_opens_nudge_and_deadline() {
    let run = run_in(State::Active);
    for status in [ChildStatus::Idle, ChildStatus::Done] {
        let t = transact(&run, &stamped(&run, obs_unique(Some(status))));
        let record = updated_run(&t);
        assert_eq!(
            record.idle_since,
            Some(NOW),
            "the episode opens at the observation"
        );
        assert_eq!(
            record.idle_deadline,
            Some(Timestamp(900_500)),
            "idle_deadline is the episode start + 15 minutes"
        );
        assert_eq!(
            effect_keys(&t),
            Vec::from(["run:r-1:nudge:0"]),
            "the child gets the episode's one nudge"
        );
        assert_eq!(record.nudged_episode, Some(0));
    }
}

#[test]
fn f25_repeated_idle_does_not_renudge_or_extend() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    run.nudged_episode = Some(0);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Idle))));
    let record = updated_run(&t);
    assert_eq!(
        record.idle_since,
        Some(Timestamp(10)),
        "the episode does not restart"
    );
    assert_eq!(record.idle_deadline, Some(Timestamp(910)));
    assert!(t.effects.is_empty(), "one nudge per episode (F23)");
}

#[test]
fn f25_stall_then_idle_shares_one_episode() {
    let mut run = run_in(State::Active);
    run.nudged_episode = Some(0); // a stall already spent the episode's nudge
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Idle))));
    let record = updated_run(&t);
    assert_eq!(record.idle_since, Some(NOW));
    assert!(
        t.effects.is_empty(),
        "stall and idle share the episode's one nudge"
    );
}

#[test]
fn f25_idle_deadline_settles_no_handoff() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(400));
    let t = transact(&run, &stamped(&run, Event::Deadline(DeadlineKind::Idle)));
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::NoHandoff),
        "the child that never wrote a handoff settles no_handoff"
    );
    // before the deadline → nothing.
    let mut run_early = run_in(State::Active);
    run_early.idle_deadline = Some(Timestamp(600));
    let t_early = transact(
        &run_early,
        &stamped(&run_early, Event::Deadline(DeadlineKind::Idle)),
    );
    assert!(is_quiet(&t_early));
    // idle deadlines never fire outside active.
    let mut run_judging = run_in(State::Judging);
    run_judging.evidence_generation = 1;
    run_judging.idle_deadline = Some(Timestamp(400));
    let t_judging = transact(
        &run_judging,
        &stamped(&run_judging, Event::Deadline(DeadlineKind::Idle)),
    );
    assert!(
        is_quiet(&t_judging),
        "judging answers to judgment_deadline only"
    );
}

// ---------------- mutation-coverage strengthening ----------------

#[test]
fn f20_settled_event_body_carries_the_spelling() {
    let run = run_in(State::Active);
    let t = settle(&run, Settlement::Cancelled, NOW, &test_policy());
    assert_eq!(
        t.events.last().map(|e| e.body.as_str()),
        Some("{\"settlement\":\"cancelled\"}"),
        "the terminal event body names the settlement"
    );
    let t_unresolved = settle(
        &run,
        Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
        NOW,
        &test_policy(),
    );
    assert_eq!(
        t_unresolved.events.last().map(|e| e.body.as_str()),
        Some("{\"settlement\":\"unresolved\",\"reason\":\"max_age\"}"),
        "unresolved carries settlement_reason"
    );
}

#[test]
fn f22_working_after_a_consumed_stall_nudge_ends_the_episode() {
    // the episode's nudge was spent on a stall while the child kept working —
    // no idle episode is open, yet the episode still ends on work.
    let mut run = run_in(State::Active);
    run.nudged_episode = Some(0);
    run.idle_since = None;
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Working))));
    assert_eq!(
        updated_run(&t).nudge_episode,
        1,
        "either arm of the episode-open check closes the episode"
    );
}

#[test]
fn f22_blocked_observation_records_status_and_writes_once() {
    let run = run_in(State::Active);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Blocked))));
    assert_eq!(
        updated_run(&t).child_status,
        Some(ChildStatus::Blocked),
        "a status change writes the run row"
    );
    // a repeated identical blocked observation is a true no-op — the review
    // exists and the row is unchanged.
    let mut run_repeat = run_in(State::Active);
    run_repeat.child_status = Some(ChildStatus::Blocked);
    run_repeat
        .identity
        .as_mut()
        .expect("active has identity")
        .pane_id = PaneId("w0:p9".into());
    let journal = Vec::from([journal_effect(
        &run_repeat,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    )]);
    let t_repeat = transition(
        &run_repeat,
        &stamped(&run_repeat, obs_unique(Some(ChildStatus::Blocked))),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_repeat),
        "nothing changed, nothing to ask — no write"
    );
}

#[test]
fn f22_failed_results_journal_their_certainty() {
    let run = run_in(State::Active);
    for (outcome, certainty) in [
        (
            EffectOutcome::PreInteractiveFailed,
            Some(EffectCertainty::Absent),
        ),
        (
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
            Some(EffectCertainty::Unknown),
        ),
        (EffectOutcome::Unconfirmed, None),
        (EffectOutcome::Acknowledged, None),
    ] {
        let t = transact(
            &run,
            &stamped(
                &run,
                run_result(&run, "nudge:0", EffectKind::Prompt, outcome, None),
            ),
        );
        let write = t
            .state_changes
            .iter()
            .find_map(|c| match c {
                StateChange::WriteEffect(w) => Some(w),
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
            .expect("one journal write per result");
        assert_eq!(write.certainty, certainty);
    }
}

#[test]
fn f22_prompting_ignores_non_task_prompt_results() {
    let run = run_in(State::Prompting);
    // a prompt effect that is not the task prompt (kind matches, key does
    // not) must not promote the Run — the `||` guard keeps it out.
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(
        updated_records(&t).is_empty(),
        "only the task prompt's resolution promotes"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)]),
        "the journal write still commits"
    );
    // and a non-prompt effect under the task key stays out too.
    let t_kind = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Close,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(updated_records(&t_kind).is_empty());
}

#[test]
fn f22_repair_dispatch_at_the_deadline_does_not_count() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 1;
    run.repair_deadline = Some(NOW);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "the follow-up must land strictly before the deadline"
    );
    // no armed deadline at all → the dispatch counts.
    let mut run_unarmed = run_in(State::Repair);
    run_unarmed.evidence_generation = 1;
    let t_unarmed = transition(
        &run_unarmed,
        &stamped(
            &run_unarmed,
            run_result(
                &run_unarmed,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(updated_run(&t_unarmed).work_generation, 1);
}

#[test]
fn f22_unanswered_set_applies_nothing() {
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
    record.set.outcome = JudgmentOutcome::TransportFailed;
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert!(
        t.events.is_empty() && t.effects.is_empty() && updated_records(&t).is_empty(),
        "only an answered set maps answers (H#83 — incomplete evidence proves nothing)"
    );
}

#[test]
fn f23_noul_threshold_boundary() {
    let run = run_in(State::Active);
    // exactly at the policy threshold → cleared.
    let record = review_record(&run, Vec::from([noul(Question::ProviderLimited, 0.7)]));
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::ProviderLimited),
        "p == threshold clears"
    );
    // a judgment carrying its own recorded threshold applies that.
    let thresholded = review_record(
        &run,
        Vec::from([noul_with_threshold(
            Question::BlockedOnInput,
            0.6,
            Some(0.8),
        )]),
    );
    let t_thresholded = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(thresholded)),
            ),
        ),
    );
    assert!(
        t_thresholded.events.is_empty(),
        "0.6 < the recorded 0.8 threshold → not cleared"
    );
    // and a judgment without the "yes" probability never clears.
    let mut missing = noul(Question::BlockedOnInput, 0.9);
    missing.probabilities = BTreeMap::from([(String::from("no"), Probability(0.1))]);
    let missing_record = review_record(&run, Vec::from([missing]));
    let t_missing = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(missing_record)),
            ),
        ),
    );
    assert!(t_missing.events.is_empty(), "no P(yes) → never cleared");
}

#[test]
fn f23_launch_bound_set_is_not_stale_but_still_maps() {
    // a set bound to the Launch (versions=None) is never stale for a Run —
    // its F23 answers still map (the supervisor read it for this Run).
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::OutsideScope, 0.9)]));
    record.set.versions = None;
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert_eq!(event_kinds(&t), Vec::from([MailboxEventKind::OutsideScope]));
}

#[test]
fn f22_starting_ack_uses_the_matched_candidates_index() {
    let run = run_in(State::Starting);
    let journal = Vec::from([
        journal_effect_at(
            &run,
            "start:0",
            EffectKind::AgentStart,
            EffectState::Failed,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
        journal_effect_at(
            &run,
            "start:1",
            EffectKind::AgentStart,
            EffectState::Dispatching,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
    ]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:1",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::AgentStarted {
                    identity: identity(),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(3)), &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.operating_point,
        Some(OperatingPointId("op-1".into())),
        "the run records the candidate that actually started"
    );
    // without a persisted decision the identity still lands, but the
    // candidate fields cannot be recovered — they stay unset.
    let mut run_nodecision = run_in(State::Starting);
    run_nodecision.operating_point = None;
    run_nodecision.provider = None;
    run_nodecision.tier_start = None;
    let t_nodecision = transition(
        &run_nodecision,
        &stamped(
            &run_nodecision,
            run_result(
                &run_nodecision,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::AgentStarted {
                    identity: identity(),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    let record_nodecision = updated_run(&t_nodecision);
    assert_eq!(record_nodecision.identity, Some(identity()));
    assert_eq!(
        record_nodecision.operating_point, None,
        "no decision → the started candidate cannot be recovered"
    );
}

#[test]
fn f22_active_absent_with_frozen_handoff_arms_deadline() {
    let mut run = run_in(State::Active);
    run.judgment_deadline = None;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert_eq!(
        updated_run(&t).judgment_deadline,
        Some(Timestamp(1_800_500)),
        "a missing judgment deadline is armed on entering judging"
    );
    // and an already-armed one is preserved.
    let mut run_armed = run_in(State::Active);
    run_armed.judgment_deadline = Some(Timestamp(700));
    let t_armed = transition(
        &run_armed,
        &stamped(
            &run_armed,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert_eq!(
        updated_run(&t_armed).judgment_deadline,
        Some(Timestamp(700))
    );
}

#[test]
fn f22_refreeze_preserves_an_armed_judgment_deadline() {
    let mut run = run_in(State::Active);
    run.judgment_deadline = Some(Timestamp(700));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t).judgment_deadline,
        Some(Timestamp(700)),
        "judgment_deadline is set on the first freeze only"
    );
}
