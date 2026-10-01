//! The `transition`-side inputs: the Run record in any lifecycle state, its
//! version stamp and judgment record, receipts, journal rows and the
//! version-stamped event delivered at `now`.

use std::collections::BTreeMap;

use governor_core::acceptance::HandoffReading;
use governor_core::config::{Config, ConfigVersion};
use governor_core::identity::{
    AgentKind, CallerKey, ChildStatus, Digest, EffectId, EffectKey, JudgmentSetId, LaunchId,
    NativeSession, Observation, RunId, Timestamp,
};
use governor_core::lifecycle::{
    DeadlineKind, Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult,
    EffectState, EffectTarget, Event, JudgmentVerdict, PromptCertainty, Run, Settlement, State,
    UnresolvedReason, VersionTriple, Versioned,
};
use governor_core::routing::{
    Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, PlacementPlan,
    Probability, Question, QuestionVersion,
};
use proptest::collection::vec as prop_vec;
use proptest::option::of as opt_of;
use proptest::prelude::{Just, Strategy as _, any, prop_oneof};
use proptest::sample::select;
use proptest::strategy::BoxedStrategy;

use super::input_strategies::{
    any_digest, any_pane, any_session, any_tab, child_identity, opt_timestamp, pick, probability,
};

const EFFECT_SUFFIXES: &[&str] = &[
    "start:0",
    "start:1",
    "start:2",
    "prompt:task",
    "outbox:0",
    "outbox:1",
    "nudge:0",
    "review:0",
    "close",
    "accept:0:1",
    "unrelated",
];
const STATES: [State; 7] = [
    State::Reserved,
    State::Starting,
    State::Prompting,
    State::Active,
    State::Judging,
    State::Repair,
    State::Settled,
];
const EFFECT_KINDS: [EffectKind; 6] = [
    EffectKind::JevEvaluate,
    EffectKind::TabCreate,
    EffectKind::PaneSplit,
    EffectKind::AgentStart,
    EffectKind::Prompt,
    EffectKind::Close,
];
const EFFECT_STATES: [EffectState; 5] = [
    EffectState::Planned,
    EffectState::Dispatching,
    EffectState::Acknowledged,
    EffectState::Failed,
    EffectState::Unconfirmed,
];
const CHILD_STATUSES: [ChildStatus; 4] = [
    ChildStatus::Working,
    ChildStatus::Idle,
    ChildStatus::Done,
    ChildStatus::Blocked,
];
const SETTLEMENTS: [Settlement; 7] = [
    Settlement::Accepted,
    Settlement::Rejected,
    Settlement::NoHandoff,
    Settlement::PaneLost,
    Settlement::Cancelled,
    Settlement::ProviderLimited,
    Settlement::Unresolved {
        reason: UnresolvedReason::LaunchFailed,
    },
];

pub(crate) fn base_run(state: State) -> Run {
    Run {
        id: RunId("run-0".into()),
        launch: LaunchId("launch-0".into()),
        owner: CallerKey {
            agent_kind: AgentKind("caller-alpha".into()),
            native_session: NativeSession("sess-c".into()),
        },
        owner_generation: 0,
        version: 1,
        state,
        prompt_certainty: None,
        child_name: "gov-run-0".into(),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/proj".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(3_600_000),
        nudge_episode: 0,
        nudged_episode: None,
        settlement: None,
        settled_at: None,
    }
}
pub(crate) fn run(config: &Config) -> BoxedStrategy<Run> {
    (
        pick(&STATES),
        opt_of(child_identity(config)),
        opt_of(pick(&config.catalog.operating_points)),
        opt_of(pick(&config.policy.tiers)),
        opt_of(pick(&CHILD_STATUSES)),
        opt_of(pick(&[
            PromptCertainty::Acknowledged,
            PromptCertainty::Unconfirmed,
        ])),
        (
            opt_timestamp(),
            opt_timestamp(),
            opt_timestamp(),
            opt_timestamp(),
            opt_timestamp(),
        ),
        (0u64..=8, 0u64..=3, 0u64..=3, 0u64..=3, 0u64..=3),
        pick(&SETTLEMENTS),
    )
        .prop_map(
            |(
                state,
                identity,
                point,
                tier,
                status,
                certainty,
                (idle_since, idle_deadline, repair_deadline, rejected_at, judgment_deadline),
                (version, work_gen, evidence_gen, episode, nudged),
                settlement,
            )| {
                let settled = state == State::Settled;
                Run {
                    identity,
                    operating_point: point.as_ref().map(|p| p.id.clone()),
                    provider: point.map(|p| p.provider),
                    tier_start: tier,
                    child_status: status,
                    prompt_certainty: certainty,
                    idle_since,
                    idle_deadline,
                    repair_deadline,
                    rejected_at,
                    judgment_deadline,
                    version,
                    work_generation: work_gen,
                    evidence_generation: evidence_gen,
                    nudge_episode: episode,
                    nudged_episode: (nudged != 0).then_some(episode),
                    settlement: settled.then_some(settlement),
                    settled_at: settled.then_some(Timestamp(500)),
                    ..base_run(state)
                }
            },
        )
        .boxed()
}
fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}
fn judgment_record(run: &Run) -> BoxedStrategy<JudgmentRecord> {
    let run_id = run.id.clone();
    (
        pick(&[
            JudgmentPurpose::Review,
            JudgmentPurpose::Acceptance,
            JudgmentPurpose::ProviderLimit,
        ]),
        prop_oneof![
            2 => Just(Some(triple_of(run))),
            1 => Just(None),
            1 => (0u64..=8).prop_map(|v| Some(VersionTriple {
                version: v, work_generation: 0, evidence_generation: 0,
            })),
        ],
        prop_vec(probability(), 0..=3),
    )
        .prop_map(move |(purpose, versions, probabilities)| JudgmentRecord {
            set: JudgmentSet {
                id: JudgmentSetId("set-1".into()),
                purpose,
                launch: None,
                run: Some(run_id.clone()),
                versions,
                task_digest: Digest([4; 32]),
                handoff_digest: None,
                evidence_digest: None,
                model: "jev-model".into(),
                question_version: QuestionVersion("qv-1".into()),
                policy_version: ConfigVersion("cfg-1".into()),
                outcome: JudgmentOutcome::Answered,
            },
            judgments: probabilities
                .into_iter()
                .enumerate()
                .map(|(item, yes)| Judgment {
                    question: Question::HandoffMeetsItem {
                        item: u8::try_from(item).unwrap_or(u8::MAX),
                    },
                    probabilities: BTreeMap::from([
                        ("yes".into(), yes),
                        ("no".into(), Probability(1.0 - yes.0)),
                    ]),
                    answer: if yes.0 >= 0.5 {
                        "yes".into()
                    } else {
                        "no".into()
                    },
                    threshold: None,
                })
                .collect(),
        })
        .boxed()
}

fn receipt(run: &Run, config: &Config) -> BoxedStrategy<Option<EffectReceipt>> {
    prop_oneof![
        3 => Just(None),
        4 => child_identity(config)
            .prop_map(|identity| Some(EffectReceipt::AgentStarted { identity })),
        3 => judgment_record(run).prop_map(|r| Some(EffectReceipt::Judgments(r))),
        1 => (any_tab(), any_pane())
            .prop_map(|(tab, pane)| Some(EffectReceipt::TabCreated { tab, pane })),
        1 => any_pane().prop_map(|pane| Some(EffectReceipt::PaneCreated { pane })),
    ]
    .boxed()
}
pub(crate) fn journal_effect(run: &Run, config: &Config) -> BoxedStrategy<Effect> {
    let run_id = run.id.0.clone();
    let launch_id = run.launch.clone();
    let subject_run = run.id.clone();
    (
        pick(EFFECT_SUFFIXES),
        pick(&EFFECT_KINDS),
        pick(&EFFECT_STATES),
        pick(&[EffectCertainty::Absent, EffectCertainty::Unknown]),
        prop_oneof![
            2 => Just(None),
            4 => child_identity(config).prop_map(|i| Some(EffectTarget::Child(i))),
            1 => opt_of(any_tab()).prop_map(|tab| Some(EffectTarget::AgentPane(match tab {
                Some(tab_id) => PlacementPlan::ExistingTab { tab: tab_id },
                None => PlacementPlan::NewTab,
            }))),
            1 => any_tab().prop_map(|tab| Some(EffectTarget::ExistingTab(tab))),
            1 => any_pane().prop_map(|pane| Some(EffectTarget::CallerContext(pane))),
        ],
        receipt(run, config),
        opt_timestamp(),
    )
        .prop_map(
            move |(suffix, kind, state, certainty, target, receipt, dispatched_at)| {
                let key = EffectKey(format!("run:{run_id}:{suffix}"));
                Effect {
                    id: EffectId(format!("eff:{}", key.0)),
                    key,
                    kind,
                    subject_launch: Some(launch_id.clone()),
                    subject_run: Some(subject_run.clone()),
                    target,
                    payload_digest: None,
                    state,
                    certainty: (state == EffectState::Failed).then_some(certainty),
                    receipt,
                    dispatched_at: dispatched_at.filter(|_| state != EffectState::Planned),
                }
            },
        )
        .boxed()
}
pub(crate) fn event(
    run: &Run,
    journal: &[Effect],
    config: &Config,
) -> BoxedStrategy<Versioned<Event>> {
    let mut keys = Vec::from_iter(journal.iter().map(|e| e.key.0.clone()));
    keys.extend(
        EFFECT_SUFFIXES
            .iter()
            .map(|suffix| format!("run:{}:{}", run.id.0, suffix)),
    );
    let obs = prop_oneof![
        4 => (opt_of(pick(&CHILD_STATUSES)), any_pane(), opt_of(any_session())).prop_map(
            |(status, pane, native_session)| Observation::Unique { status, pane, native_session }
        ),
        2 => Just(Observation::Absent),
        1 => Just(Observation::Invalid),
    ];
    let effect_result = (
        select(keys).prop_map(EffectKey),
        pick(&EFFECT_KINDS),
        prop_oneof![
            4 => Just(EffectOutcome::Acknowledged),
            2 => Just(EffectOutcome::PreInteractiveFailed),
            1 => Just(EffectOutcome::Failed { certainty: EffectCertainty::Absent }),
            1 => Just(EffectOutcome::Failed { certainty: EffectCertainty::Unknown }),
            2 => Just(EffectOutcome::Unconfirmed),
        ],
        receipt(run, config),
    )
        .prop_map(|(key, kind, outcome, receipt)| {
            Event::EffectResult(EffectResult {
                key,
                kind,
                outcome,
                receipt,
            })
        });
    (
        prop_oneof![
            4 => (
                obs,
                opt_of(prop_oneof![
                    1 => any_digest().prop_map(|digest| HandoffReading::Valid { digest }),
                    1 => Just(HandoffReading::NotWritten),
                ]),
            )
            .prop_map(|(observation, handoff_reading)| Event::Obs {
                observation,
                handoff_reading,
            }),
            2 => any_digest().prop_map(|digest| Event::Handoff { digest }),
            2 => pick(&[
                JudgmentVerdict::Accept, JudgmentVerdict::Reject, JudgmentVerdict::Unavailable,
            ])
            .prop_map(Event::Judgment),
            2 => pick(&[
                DeadlineKind::Idle, DeadlineKind::Repair,
                DeadlineKind::Judgment, DeadlineKind::MaxAge,
            ])
            .prop_map(Event::Deadline),
            2 => any::<bool>().prop_map(|close_pane| Event::Cancel { close_pane }),
            2 => pick(&[Event::ProviderLimited, Event::Restart]),
            4 => effect_result,
        ],
        prop_oneof![
            3 => Just(triple_of(run)),
            1 => (0u64..=8).prop_map(|v| VersionTriple {
                version: v, work_generation: 0, evidence_generation: 0,
            }),
        ],
    )
        .prop_map(|(value, requested_against)| Versioned {
            requested_against,
            value,
        })
        .boxed()
}
