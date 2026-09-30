//! N8 — the metamorphic input space (spec §7). `world` generates a
//! catalog/policy plus everything `route` (F13) and `transition` (Appendix
//! C) consume, and a random bijective `Rename` of the catalog's harness
//! kinds and operating-point ids. Shared name pools make renamings swap,
//! collide and mint fresh names; tiers, capabilities, providers, cost
//! classes, args, catalog order and caller identity all stay put. The
//! `Rename` application methods live in `n8_metamorphic.rs`.

use std::collections::BTreeMap;
use std::time::Duration;

use governor_core::acceptance::{FrozenHandoff, HandoffReading};
use governor_core::config::{
    Capability, Catalog, Config, ConfigVersion, CostClass, OperatingPoint, OperatingPointId,
    Policy, Provider, Qualification, Tier,
};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, JudgmentSetId, LaunchId, NativeSession, Observation, PaneId,
    ProjectRoot, RunId, TabId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    DeadlineKind, Effect, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult,
    EffectState, EffectTarget, Event, JudgmentVerdict, PromptCertainty, Run, Settlement, State,
    UnresolvedReason, VersionTriple, Versioned,
};
use governor_core::routing::{
    ChangesFiles, Evaluation, Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord,
    JudgmentSet, PlacementPlan, Probability, Question, QuestionVersion, TabChoice,
};
use governor_core::task::{Launch, LaunchPhase, Task};
use proptest::collection::vec as prop_vec;
use proptest::option::of as opt_of;
use proptest::prelude::{Just, Strategy, any, prop_oneof};
use proptest::sample::select;
use proptest::strategy::BoxedStrategy;

/// A bijective renaming over harness kinds and operating-point ids;
/// undeclared names pass through.
#[derive(Debug, Clone)]
pub struct Rename {
    /// `old harness name -> new harness name` / `old op id -> new id`.
    pub kinds: BTreeMap<String, String>,
    pub ops: BTreeMap<String, String>,
}

/// One metamorphic case: everything `route` and `transition` read, plus
/// the renaming.
#[derive(Debug)]
pub struct World {
    pub config: Config,
    pub launch: Launch,
    pub predecessor: Option<Run>,
    pub evaluation: Evaluation,
    pub required: Vec<Capability>,
    pub qualifications: Vec<Qualification>,
    pub cooling: Vec<Provider>,
    pub run: Run,
    pub journal: Vec<Effect>,
    pub handoffs: Vec<FrozenHandoff>,
    pub event: Versioned<Event>,
    pub now: Timestamp,
    pub freeze_path: String,
    pub owner_absent: bool,
    pub rename: Rename,
}

// Real harness names appear because the property proves they carry no
// special behaviour (I9 keeps them out of src; tests/ may name them).
const HARNESS_NAMES: &[&str] = &[
    "claude", "codex", "gemini", "devin", "alpha", "beta", "hook",
];
const PROVIDER_NAMES: &[&str] = &["pv-east", "pv-west", "pv-north"];
const CALLER_KINDS: &[&str] = &["caller-alpha", "caller-beta"];
const CAPABILITY_NAMES: &[&str] = &[
    Capability::START,
    Capability::PROMPT_ACK,
    Capability::HANDOFF_WRITE,
    Capability::FOLLOWUP_READ,
    Capability::MID_TURN_INPUT,
    Capability::HINT_CONSUMPTION,
    "cap-extra",
    "cap-side",
];
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

fn pick<T: Clone + std::fmt::Debug + 'static>(pool: &[T]) -> impl Strategy<Value = T> + use<T> {
    select(Vec::from(pool))
}
fn named<T: std::fmt::Debug + 'static>(
    pool: &[&'static str],
    wrap: fn(String) -> T,
) -> impl Strategy<Value = T> + use<T> {
    select(Vec::from(pool)).prop_map(move |name| wrap(String::from(name)))
}
fn any_session() -> impl Strategy<Value = NativeSession> {
    named(&["sess-0", "sess-1", "sess-2"], NativeSession)
}
fn any_pane() -> impl Strategy<Value = PaneId> {
    named(&["w0:p0", "w0:p1", "w1:p0"], PaneId)
}
fn any_tab() -> impl Strategy<Value = TabId> {
    named(&["w0:t0", "w0:t1"], TabId)
}
fn any_digest() -> impl Strategy<Value = Digest> {
    pick(&[1u8, 2, 3]).prop_map(|tag| Digest([tag; 32]))
}
fn opt_timestamp() -> impl Strategy<Value = Option<Timestamp>> {
    opt_of((0i64..=3_600_000).prop_map(Timestamp))
}
fn probability() -> impl Strategy<Value = Probability> {
    (0.0f64..=1.0).prop_map(Probability)
}
fn child_identity(config: &Config) -> BoxedStrategy<ChildIdentity> {
    let kinds = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|p| p.harness.clone()),
    );
    (
        prop_oneof![4 => select(kinds), 1 => named(HARNESS_NAMES, AgentKind)],
        opt_of(any_session()),
        any_pane(),
    )
        .prop_map(|(kind, session, pane)| ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: kind,
            agent_name: AgentName("gov-r0".into()),
            native_session: session,
            pane_id: pane,
        })
        .boxed()
}
fn config() -> BoxedStrategy<Config> {
    let point_params = |tiers: &Vec<Tier>| {
        (
            named(HARNESS_NAMES, AgentKind),
            prop_vec(
                pick(&["--fast", "--safe", "--verbose"]).prop_map(String::from),
                0..=3,
            ),
            pick(tiers),
            prop_vec(named(CAPABILITY_NAMES, Capability), 0..=4),
            (0u32..=3).prop_map(CostClass),
            named(PROVIDER_NAMES, Provider),
        )
    };
    (1usize..=4)
        .prop_flat_map(move |count| {
            let tiers = Vec::from_iter((0..count).map(|i| Tier(format!("t{i}"))));
            (
                prop_vec(point_params(&tiers), 1..=6),
                (
                    (
                        opt_of(pick(&tiers)),
                        opt_of(pick(&tiers)),
                        opt_of(pick(&tiers)),
                        0.0f64..=1.0,
                        0.0f64..=1.0,
                    ),
                    (
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                    ),
                )
                    .prop_map(move |(first, second)| {
                        let (cap, security, broad, threshold, rate) = first;
                        let (expiry, cool, age, repair, judge, idle) = second;
                        Policy {
                            tiers: tiers.clone(),
                            no_change_cap: cap,
                            security_floor: security,
                            broad_change_floor: broad,
                            provider_limit_threshold: threshold,
                            exploration_rate: rate,
                            recovery_expiry: expiry,
                            cooldown: cool,
                            max_age: age,
                            repair_window: repair,
                            judgment_window: judge,
                            idle_window: idle,
                        }
                    }),
            )
        })
        .prop_map(|(params, policy)| Config {
            version: ConfigVersion("cfg-1".into()),
            catalog: Catalog {
                operating_points: params
                    .into_iter()
                    .enumerate()
                    .map(
                        |(i, (harness, args, tier, capabilities, cost_class, provider))| {
                            OperatingPoint {
                                id: OperatingPointId(format!("op-{i}")),
                                harness,
                                args,
                                tier,
                                capabilities,
                                cost_class,
                                provider,
                            }
                        },
                    )
                    .collect(),
            },
            policy,
        })
        .boxed()
}
fn launch(config: &Config) -> BoxedStrategy<Launch> {
    (
        named(CALLER_KINDS, AgentKind).prop_map(|agent_kind| CallerKey {
            agent_kind,
            native_session: NativeSession("sess-c".into()),
        }),
        (
            opt_of(prop_oneof![
                4 => pick(&config.policy.tiers),
                1 => Just(Tier("tier-elsewhere".into())),
            ]),
            opt_of(named(&["run-0", "run-1"], RunId)),
            prop_vec(
                pick(&["it works", "lint green", "handoff written"]).prop_map(String::from),
                1..=3,
            ),
            prop_vec(
                pick(&["no api changes", "small diff"]).prop_map(String::from),
                0..=2,
            ),
        )
            .prop_map(|(tier, recovery_of, done_when, constraints)| Task {
                objective: "do the thing".into(),
                scope: "the repo".into(),
                done_when,
                constraints,
                tier,
                recovery_of,
                label: None,
                cwd: None,
            }),
        named(&["ik-0", "ik-1", "ik-2", "ik-3"], IdempotencyKey),
    )
        .prop_map(|(caller, work, key)| Launch {
            id: LaunchId("launch-0".into()),
            caller,
            project_root: ProjectRoot("/proj".into()),
            idempotency_key: key,
            digest_version: Task::DIGEST_VERSION,
            task_digest: work.digest(),
            task: work,
            phase: LaunchPhase::Evaluating,
            decision: None,
            config_version: None,
            outcome: None,
        })
        .boxed()
}
fn evaluation(config: &Config) -> BoxedStrategy<Evaluation> {
    (
        probability(),
        prop_oneof![9 => pick(&config.policy.tiers), 1 => Just(Tier("tier-elsewhere".into()))],
        pick(&[ChangesFiles::None, ChangesFiles::Few, ChangesFiles::Broad]),
        (probability(), probability(), probability()),
        opt_of(prop_oneof![
            3 => any_tab().prop_map(TabChoice::Tab),
            1 => Just(TabChoice::New),
        ]),
    )
        .prop_map(
            |(verifiable, tier, changes, (boundary, external, long), tab)| Evaluation {
                done_when_verifiable: verifiable,
                weakest_sufficient_tier: tier,
                changes_files: changes,
                security_boundary: boundary,
                needs_external: external,
                long_running: long,
                related_tab: tab,
            },
        )
        .boxed()
}
fn qualifications(config: &Config) -> BoxedStrategy<Vec<Qualification>> {
    let pairs = Vec::from_iter(config.catalog.operating_points.iter().flat_map(|point| {
        point
            .capabilities
            .iter()
            .map(|capability| (point.clone(), capability.clone()))
            .collect::<Vec<(OperatingPoint, Capability)>>()
    }));
    prop_vec((any::<bool>(), any::<bool>()), pairs.len())
        .prop_map(move |marks| {
            let mut rows = Vec::new();
            for ((point, capability), (pass, stale)) in pairs.iter().cloned().zip(marks) {
                rows.push(Qualification {
                    operating_point: point.id.clone(),
                    args_digest: if pass || !stale {
                        point.args_digest()
                    } else {
                        Digest([0; 32])
                    },
                    capability,
                    passed: pass || stale,
                    evidence: String::new(),
                });
            }
            rows
        })
        .boxed()
}
fn base_run(state: State) -> Run {
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
        judgment_deadline: None,
        max_age_deadline: Timestamp(3_600_000),
        nudge_episode: 0,
        nudged_episode: None,
        settlement: None,
        settled_at: None,
    }
}
fn run(config: &Config) -> BoxedStrategy<Run> {
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
                (idle_since, idle_deadline, repair_deadline, judgment_deadline),
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
fn journal_effect(run: &Run, config: &Config) -> BoxedStrategy<Effect> {
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
    )
        .prop_map(move |(suffix, kind, state, certainty, target, receipt)| {
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
            }
        })
        .boxed()
}
fn event(run: &Run, journal: &[Effect], config: &Config) -> BoxedStrategy<Versioned<Event>> {
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

// A salt orders a random permutation per element; a flag picks the image —
// permuted old name or fresh name. Bijective either way.
fn images(domain: Vec<String>, prefix: &'static str) -> BoxedStrategy<Vec<String>> {
    prop_vec((any::<u64>(), any::<bool>()), domain.len())
        .prop_map(move |marks| {
            let mut order = Vec::from_iter(0..domain.len());
            order.sort_by_key(|index| marks.get(*index).map_or(0, |(salt, _)| *salt));
            Vec::from_iter(domain.iter().enumerate().map(|(index, name)| {
                let fresh = marks.get(index).is_some_and(|(_, fresh)| *fresh);
                match (fresh, order.get(index)) {
                    (true, _) => format!("{prefix}-{index}"),
                    (false, Some(position)) => domain
                        .get(*position)
                        .map_or_else(|| name.clone(), String::clone),
                    (false, None) => name.clone(),
                }
            }))
        })
        .boxed()
}

fn rename(config: &Config) -> BoxedStrategy<Rename> {
    let mut kinds = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|point| point.harness.0.clone()),
    );
    kinds.sort();
    kinds.dedup();
    let ops = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|p| p.id.0.clone()),
    );
    (images(kinds.clone(), "rk"), images(ops.clone(), "ro"))
        .prop_map(move |(kind_images, op_images)| Rename {
            kinds: BTreeMap::from_iter(kinds.iter().cloned().zip(kind_images)),
            ops: BTreeMap::from_iter(ops.iter().cloned().zip(op_images)),
        })
        .boxed()
}

/// One metamorphic case: the generated world plus its renaming. The journal
/// draws first so the event's effect keys correlate with it.
pub fn world() -> BoxedStrategy<World> {
    config()
        .prop_flat_map(|config| {
            run(&config).prop_flat_map(move |run| {
                let cfg = config.clone();
                prop_vec(journal_effect(&run, &config), 0..=4).prop_flat_map(move |journal| {
                    let run_id = run.id.clone();
                    let predecessor = opt_of((
                        pick(&cfg.catalog.operating_points),
                        opt_of(pick(&cfg.policy.tiers)),
                    ))
                    .prop_map(|pair| {
                        pair.map(|(point, tier)| Run {
                            provider: Some(point.provider.clone()),
                            operating_point: Some(point.id.clone()),
                            tier_start: tier,
                            settlement: Some(Settlement::ProviderLimited),
                            settled_at: Some(Timestamp(1_000)),
                            ..base_run(State::Settled)
                        })
                    });
                    (
                        (
                            Just(cfg.clone()),
                            Just(run.clone()),
                            Just(journal.clone()),
                            prop_vec((0u64..=3, any_digest()), 0..=2).prop_map(move |rows| {
                                rows.into_iter()
                                    .map(|(work_generation, digest)| FrozenHandoff {
                                        run: run_id.clone(),
                                        work_generation,
                                        digest,
                                        frozen_path: String::from("/state/handoffs/a"),
                                        frozen_at: Timestamp(100),
                                    })
                                    .collect()
                            }),
                            event(&run, &journal, &cfg),
                            (0i64..=3_600_000).prop_map(Timestamp),
                            pick(&["/state/h/a", "/state/h/b"]).prop_map(String::from),
                            any::<bool>(),
                        ),
                        (
                            launch(&cfg),
                            predecessor,
                            evaluation(&cfg),
                            prop_vec(named(CAPABILITY_NAMES, Capability), 0..=3),
                            qualifications(&cfg),
                            prop_vec(named(PROVIDER_NAMES, Provider), 0..=2),
                            rename(&cfg),
                        ),
                    )
                })
            })
        })
        .prop_map(
            |(
                (config, run, journal, handoffs, event, now, freeze_path, owner_absent),
                (launch, predecessor, evaluation, required, qualifications, cooling, rename),
            )| World {
                config,
                launch,
                predecessor,
                evaluation,
                required,
                qualifications,
                cooling,
                run,
                journal,
                handoffs,
                event,
                now,
                freeze_path,
                owner_absent,
                rename,
            },
        )
        .boxed()
}
#[cfg(test)]
mod self_check {
    use super::{World, world};
    use proptest::strategy::{Strategy as _, ValueTree as _};
    use proptest::test_runner::TestRunner;

    // This file also compiles as its own test crate: generate a world to
    // keep the helpers used and pin the renaming's bijection.
    #[test]
    fn world_renaming_is_bijective_on_the_domain() {
        let mut runner = TestRunner::default();
        let world: World = world()
            .new_tree(&mut runner)
            .expect("the world strategy must produce a value")
            .current();
        let mut images: Vec<&String> = world.rename.kinds.values().collect();
        images.sort();
        images.dedup();
        assert_eq!(
            images.len(),
            world.rename.kinds.len(),
            "the renaming is a bijection on the declared harness kinds"
        );
    }
}
