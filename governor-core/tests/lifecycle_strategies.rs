//! F20/F22 — proptest strategies for the spec §10 Phase-3 lifecycle property
//! proofs: arbitrary but well-typed `Run`s, `Event`s, effect journals, frozen
//! handoffs and version stamps, built only on governor-core's public API.
//! `lifecycle_props.rs` includes this file as a module; cargo also compiles
//! it as its own test target, so the trailing smoke test drives every shared
//! strategy and keeps both compilations warning-free.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::Duration;

use governor_core::acceptance::HandoffReading;
use governor_core::config::{ConfigVersion, OperatingPointId, Policy, Provider, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, Digest, EffectKey,
    HerdrIncarnation, JudgmentSetId, LaunchId, NativeSession, Observation, PaneId, RunId, TabId,
    TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult, Event,
    JudgmentVerdict, PromptCertainty, Run, Settlement, State, UnresolvedReason, VersionTriple,
    Versioned,
};
use governor_core::routing::{
    Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability, Question,
    QuestionVersion,
};
use proptest::collection::vec as prop_vec;
use proptest::option;
use proptest::prelude::{Just, Strategy, any, prop_oneof};
use proptest::sample::select;

/// The Run id every generated value shares — fixed so generated effect keys
/// collide with the real journal on purpose.
pub const RUN_ID: &str = "r-1";

#[must_use]
pub fn text(value: &str) -> String {
    String::from(value)
}

/// The fixed policy the proofs drive — mirrors the in-crate test policy.
#[must_use]
pub fn test_policy() -> Policy {
    Policy {
        tiers: Vec::from([Tier(text("t0")), Tier(text("t1"))]),
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

/// Uniform pick from a finite table.
pub fn pick<T: Clone + Debug + 'static>(items: &'static [T]) -> impl Strategy<Value = T> {
    select(Vec::from(items))
}

/// Small timestamp space so generated `now`s land near armed deadlines.
pub fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
    (0_i64..10_000_000_i64).prop_map(Timestamp)
}

/// A generated digest from a deliberately tiny space — repeats collide with
/// frozen handoffs, which is what the digest dedup rules consume.
pub fn arb_digest() -> impl Strategy<Value = Digest> {
    (0_u8..4).prop_map(|byte| Digest([byte; 32]))
}

fn arb_state() -> impl Strategy<Value = State> {
    pick(&[
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ])
}

fn arb_settlement() -> impl Strategy<Value = Settlement> {
    prop_oneof![
        pick(&[
            Settlement::Accepted,
            Settlement::Rejected,
            Settlement::NoHandoff,
            Settlement::PaneLost,
            Settlement::Cancelled,
            Settlement::ProviderLimited,
        ]),
        pick(&[
            UnresolvedReason::LaunchNotStarted,
            UnresolvedReason::LaunchFailed,
            UnresolvedReason::JudgmentUnavailable,
            UnresolvedReason::IdentityUnprovable,
            UnresolvedReason::MaxAge,
        ])
        .prop_map(|reason| Settlement::Unresolved { reason }),
    ]
}

fn arb_child_status() -> impl Strategy<Value = ChildStatus> {
    pick(&[
        ChildStatus::Working,
        ChildStatus::Idle,
        ChildStatus::Done,
        ChildStatus::Blocked,
    ])
}

pub fn arb_identity() -> impl Strategy<Value = ChildIdentity> {
    (0_u8..4, option::of(0_u8..4)).prop_map(|(pane, session)| ChildIdentity {
        herdr_incarnation: HerdrIncarnation(text("inc-1")),
        terminal_id: TerminalId(text("term-1")),
        agent_kind: AgentKind(text("kind-1")),
        agent_name: AgentName(text("gov-r1")),
        native_session: session.map(|s| NativeSession(format!("sess-{s}"))),
        pane_id: PaneId(format!("w0:p{pane}")),
    })
}

/// The fixed-field `Run` the generators overlay — identifiers are shared
/// constants; `arb_run_in` sets the lifecycle-relevant fields.
fn base_run(state: State) -> Run {
    Run {
        id: RunId(text(RUN_ID)),
        launch: LaunchId(text("l-1")),
        owner: CallerKey {
            agent_kind: AgentKind(text("caller-kind")),
            native_session: NativeSession(text("caller-sess")),
        },
        owner_generation: 0,
        version: 0,
        state,
        prompt_certainty: None,
        child_name: text("gov-00000001"),
        identity: None,
        operating_point: Some(OperatingPointId(text("op-1"))),
        provider: Some(Provider(text("prov-1"))),
        tier_start: Some(Tier(text("t1"))),
        cwd: text("/repo"),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        judgment_deadline: None,
        max_age_deadline: Timestamp(0),
        nudge_episode: 0,
        nudged_episode: None,
        settlement: None,
        settled_at: None,
    }
}

fn arb_run_in(states: impl Strategy<Value = State>) -> impl Strategy<Value = Run> {
    (
        states,
        0_u64..64,                        // version
        (0_u64..8, 0_u64..8),             // work_generation, evidence_generation
        (0_u64..8, option::of(0_u64..8)), // nudge_episode, nudged_episode
        (
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            arb_timestamp(),
        ), // idle_since, idle/repair/judgment deadlines, max_age
        (
            option::of(arb_child_status()),
            option::of(pick(&[
                PromptCertainty::Acknowledged,
                PromptCertainty::Unconfirmed,
            ])),
            arb_settlement(),
            arb_timestamp(),
            arb_identity(),
            option::of((0_u8..4).prop_map(|i| Provider(format!("prov-{i}")))),
        ),
    )
        .prop_map(|(state, version, gens, episodes, times, rest)| {
            let (work_generation, evidence_generation) = gens;
            let (nudge_episode, nudged_episode) = episodes;
            let (idle_since, idle_deadline, repair_deadline, judgment_deadline, max_age) = times;
            let (
                child_status,
                gen_certainty,
                gen_settlement,
                gen_settled_at,
                gen_identity,
                provider,
            ) = rest;
            // the record invariants: identity exists once a start could have
            // been acknowledged, prompt_certainty once the prompt could have
            // resolved, settlement iff `settled` (the Appendix B checks)
            let (identity, prompt_certainty) = match state {
                State::Reserved | State::Starting => (None, None),
                State::Prompting => (Some(gen_identity), None),
                State::Active | State::Judging | State::Repair | State::Settled => {
                    (Some(gen_identity), gen_certainty)
                }
            };
            let (settlement, settled_at) = if state == State::Settled {
                (Some(gen_settlement), Some(gen_settled_at))
            } else {
                (None, None)
            };
            Run {
                version,
                work_generation,
                evidence_generation,
                nudge_episode,
                nudged_episode,
                idle_since,
                idle_deadline,
                repair_deadline,
                judgment_deadline,
                max_age_deadline: max_age,
                child_status,
                provider,
                identity,
                prompt_certainty,
                settlement,
                settled_at,
                ..base_run(state)
            }
        })
}

/// An arbitrary well-typed `Run` in any lifecycle state.
pub fn arb_run() -> impl Strategy<Value = Run> {
    arb_run_in(arb_state())
}

/// An arbitrary well-typed `Run` that has not settled — the liveness
/// property's domain.
pub fn arb_unsettled_run() -> impl Strategy<Value = Run> {
    arb_run_in(arb_state().prop_filter("unsettled", |state| *state != State::Settled))
}

/// The `reserved` Run a launch starts from: empty journal, only
/// `max_age_deadline` armed (F13/F22).
pub fn arb_reserved_run() -> impl Strategy<Value = Run> {
    (0_u64..4, arb_timestamp()).prop_map(|(version, max_age_deadline)| {
        let mut run = base_run(State::Reserved);
        run.version = version;
        run.max_age_deadline = max_age_deadline;
        run
    })
}

/// F20 — the version stamp a Run currently reads as holding.
#[must_use]
pub fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// Which field of the version triple a stale stamp corrupts.
#[derive(Debug, Clone, Copy)]
pub enum TripleField {
    /// `version` — the row version.
    Version,
    /// `work_generation`.
    WorkGeneration,
    /// `evidence_generation`.
    EvidenceGeneration,
}

/// How an event's stamp relates to the Run it is delivered to: `Fresh` is
/// stamped at delivery; `Perturbed`/`Arbitrary` model an async result
/// requested against versions that no longer hold.
#[derive(Debug, Clone, Copy)]
pub enum StampSpec {
    /// Stamp with the Run's triple at delivery time.
    Fresh,
    /// Stamp with the Run's triple plus a delta on one field.
    Perturbed(TripleField, u64),
    /// Stamp with an unrelated triple.
    Arbitrary(VersionTriple),
}

fn arb_version_triple() -> impl Strategy<Value = VersionTriple> {
    (0_u64..64, 0_u64..8, 0_u64..8).prop_map(|(version, work_generation, evidence_generation)| {
        VersionTriple {
            version,
            work_generation,
            evidence_generation,
        }
    })
}

fn arb_stamp_spec() -> impl Strategy<Value = StampSpec> {
    prop_oneof![
        6 => Just(StampSpec::Fresh),
        2 => (
            pick(&[
                TripleField::Version,
                TripleField::WorkGeneration,
                TripleField::EvidenceGeneration,
            ]),
            1_u64..=u64::MAX,
        )
            .prop_map(|(field, delta)| StampSpec::Perturbed(field, delta)),
        2 => arb_version_triple().prop_map(StampSpec::Arbitrary),
    ]
}

/// A never-fresh stamp — for the F20 staleness property.
pub fn arb_stale_spec() -> impl Strategy<Value = StampSpec> {
    arb_stamp_spec().prop_filter("never fresh", |spec| !matches!(spec, StampSpec::Fresh))
}

/// Resolve a `StampSpec` against the Run it is delivered to.
#[must_use]
pub fn stamp(run: &Run, spec: StampSpec) -> VersionTriple {
    let current = triple_of(run);
    match spec {
        StampSpec::Fresh => current,
        StampSpec::Arbitrary(triple) => triple,
        StampSpec::Perturbed(field, delta) => match field {
            TripleField::Version => VersionTriple {
                version: current.version.saturating_add(delta),
                ..current
            },
            TripleField::WorkGeneration => VersionTriple {
                work_generation: current.work_generation.saturating_add(delta),
                ..current
            },
            TripleField::EvidenceGeneration => VersionTriple {
                evidence_generation: current.evidence_generation.saturating_add(delta),
                ..current
            },
        },
    }
}

/// Stamp `event` and wrap it for the transition function.
#[must_use]
pub fn stamped(run: &Run, event: Event, spec: StampSpec) -> Versioned<Event> {
    Versioned {
        requested_against: stamp(run, spec),
        value: event,
    }
}

fn arb_observation() -> impl Strategy<Value = Observation> {
    prop_oneof![
        4 => (option::of(arb_child_status()), 0_u8..4, option::of(0_u8..4)).prop_map(
            |(status, pane, session)| Observation::Unique {
                status,
                pane: PaneId(format!("w0:p{pane}")),
                native_session: session.map(|s| NativeSession(format!("sess-{s}"))),
            },
        ),
        2 => Just(Observation::Absent),
        1 => Just(Observation::Invalid),
    ]
}

fn arb_reading() -> impl Strategy<Value = Option<HandoffReading>> {
    prop_oneof![
        5 => Just(None),
        1 => Just(Some(HandoffReading::NotWritten)),
        2 => arb_digest().prop_map(|digest| Some(HandoffReading::Valid { digest })),
    ]
}

fn arb_verdict() -> impl Strategy<Value = JudgmentVerdict> {
    pick(&[
        JudgmentVerdict::Accept,
        JudgmentVerdict::Reject,
        JudgmentVerdict::Unavailable,
    ])
}

fn arb_deadline_kind() -> impl Strategy<Value = DeadlineKind> {
    pick(&[
        DeadlineKind::Idle,
        DeadlineKind::Repair,
        DeadlineKind::Judgment,
        DeadlineKind::MaxAge,
    ])
}

fn arb_question() -> impl Strategy<Value = Question> {
    prop_oneof![
        1 => Just(Question::BlockedOnInput),
        1 => Just(Question::NoRecentProgress),
        1 => Just(Question::OutsideScope),
        2 => Just(Question::ProviderLimited),
        1 => (0_u8..3).prop_map(|item| Question::HandoffMeetsItem { item }),
    ]
}

/// An arbitrary supervision/acceptance judgment (F12/F23/F24).
pub fn arb_judgment() -> impl Strategy<Value = Judgment> {
    (
        arb_question(),
        0.0_f64..=1.0_f64,
        option::of(0.0_f64..=1.0_f64),
    )
        .prop_map(|(question, yes, threshold)| Judgment {
            question,
            probabilities: BTreeMap::from([(text("yes"), Probability(yes))]),
            answer: text(if yes >= 0.5 { "yes" } else { "no" }),
            threshold,
        })
}

/// A `JudgmentRecord` stamped `versions` — the F20 receipt the transition's
/// staleness gate reads (`set.versions`).
#[must_use]
pub fn judgment_record(
    run: &RunId,
    versions: Option<VersionTriple>,
    judgments: Vec<Judgment>,
) -> JudgmentRecord {
    JudgmentRecord {
        set: JudgmentSet {
            id: JudgmentSetId(text("set-1")),
            purpose: JudgmentPurpose::Acceptance,
            launch: None,
            run: Some(run.clone()),
            versions,
            task_digest: Digest([0; 32]),
            handoff_digest: None,
            evidence_digest: None,
            model: text("jev-1"),
            question_version: QuestionVersion(text("qv-1")),
            policy_version: ConfigVersion(text("cfg-1")),
            outcome: JudgmentOutcome::Answered,
        },
        judgments,
    }
}

fn arb_judgment_record() -> impl Strategy<Value = JudgmentRecord> {
    (
        prop_vec(arb_judgment(), 0..4),
        option::of(arb_version_triple()),
        prop_oneof![
            3 => Just(JudgmentOutcome::Answered),
            1 => pick(&[
                JudgmentOutcome::TransportFailed,
                JudgmentOutcome::AuthFailed,
                JudgmentOutcome::InvalidResponse,
                JudgmentOutcome::TooLarge,
                JudgmentOutcome::Stale,
            ]),
        ],
        pick(&[
            JudgmentPurpose::Launch,
            JudgmentPurpose::Review,
            JudgmentPurpose::Acceptance,
            JudgmentPurpose::ProviderLimit,
        ]),
    )
        .prop_map(|(judgments, versions, outcome, purpose)| {
            let mut record = judgment_record(&RunId(text(RUN_ID)), versions, judgments);
            record.set.outcome = outcome;
            record.set.purpose = purpose;
            record
        })
}

pub fn arb_certainty() -> impl Strategy<Value = EffectCertainty> {
    pick(&[EffectCertainty::Absent, EffectCertainty::Unknown])
}

pub fn arb_outcome() -> impl Strategy<Value = EffectOutcome> {
    prop_oneof![
        3 => Just(EffectOutcome::Acknowledged),
        1 => Just(EffectOutcome::PreInteractiveFailed),
        1 => arb_certainty().prop_map(|certainty| EffectOutcome::Failed { certainty }),
        1 => Just(EffectOutcome::Unconfirmed),
    ]
}

fn arb_receipt() -> impl Strategy<Value = Option<EffectReceipt>> {
    prop_oneof![
        2 => Just(None),
        2 => arb_identity().prop_map(|identity| Some(EffectReceipt::AgentStarted { identity })),
        2 => arb_judgment_record().prop_map(|record| Some(EffectReceipt::Judgments(record))),
        1 => (0_u8..4).prop_map(|tab| {
            Some(EffectReceipt::TabCreated {
                tab: TabId(format!("tab-{tab}")),
                pane: PaneId(format!("w0:p{tab}")),
            })
        }),
        1 => (0_u8..4).prop_map(|pane| {
            Some(EffectReceipt::PaneCreated {
                pane: PaneId(format!("w0:p{pane}")),
            })
        }),
    ]
}

/// A `run:<RUN_ID>:<suffix>` effect key.
#[must_use]
pub fn run_key(suffix: &str) -> EffectKey {
    EffectKey(format!("run:{RUN_ID}:{suffix}"))
}

/// The effect-key suffixes a `transition` can see — the ones the function
/// itself plans plus a stray for results that resolve nothing.
pub const RESULT_SUFFIXES: &[&str] = &[
    "tab",
    "split",
    "start:0",
    "start:1",
    "start:2",
    "prompt:task",
    "nudge:0",
    "nudge:1",
    "outbox:0",
    "outbox:1",
    "review:0",
    "review:1",
    "accept:0:1",
    "accept:1:2",
    "close",
    "stray",
];

const LAUNCH_SUFFIXES: &[&str] = &["tab", "split", "start:0", "start:1", "start:2", "stray"];
const PROMPT_SUFFIXES: &[&str] = &[
    "prompt:task",
    "nudge:0",
    "nudge:1",
    "outbox:0",
    "outbox:1",
    "close",
    "stray",
];

fn arb_effect_result() -> impl Strategy<Value = EffectResult> {
    prop_oneof![
        // launch pipeline results — the drivers that reach `prompting`
        3 => (
            pick(LAUNCH_SUFFIXES),
            pick(&[
                EffectKind::TabCreate,
                EffectKind::PaneSplit,
                EffectKind::AgentStart,
            ]),
            arb_outcome(),
            arb_receipt(),
        ),
        // prompt results — the `prompting` → `active` and repair lanes
        3 => (
            pick(PROMPT_SUFFIXES),
            Just(EffectKind::Prompt),
            arb_outcome(),
            Just(None),
        ),
        // arbitrary results over the whole space
        4 => (
            pick(RESULT_SUFFIXES),
            pick(&[
                EffectKind::JevEvaluate,
                EffectKind::TabCreate,
                EffectKind::PaneSplit,
                EffectKind::AgentStart,
                EffectKind::Prompt,
                EffectKind::Close,
            ]),
            arb_outcome(),
            arb_receipt(),
        ),
    ]
    .prop_map(|(suffix, kind, outcome, receipt)| EffectResult {
        key: run_key(suffix),
        kind,
        outcome,
        receipt,
    })
}

/// An arbitrary Appendix C event — every variant, weighted toward the
/// drivers that walk a Run deep into the lifecycle.
pub fn arb_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        4 => (arb_observation(), arb_reading()).prop_map(
            |(observation, handoff_reading)| Event::Obs {
                observation,
                handoff_reading,
            },
        ),
        2 => arb_digest().prop_map(|digest| Event::Handoff { digest }),
        2 => arb_verdict().prop_map(Event::Judgment),
        2 => arb_deadline_kind().prop_map(Event::Deadline),
        1 => any::<bool>().prop_map(|close_pane| Event::Cancel { close_pane }),
        1 => Just(Event::ProviderLimited),
        7 => arb_effect_result().prop_map(Event::EffectResult),
        1 => Just(Event::Restart),
    ]
}

/// The version-stamped subset — the async results F20's stamp gate covers.
pub fn arb_stamped_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (arb_observation(), arb_reading()).prop_map(|(observation, handoff_reading)| {
            Event::Obs {
                observation,
                handoff_reading,
            }
        }),
        arb_digest().prop_map(|digest| Event::Handoff { digest }),
        arb_verdict().prop_map(Event::Judgment),
        arb_deadline_kind().prop_map(Event::Deadline),
        Just(Event::ProviderLimited),
    ]
}

/// One step of an arbitrary event prefix: the event, how it is stamped, how
/// far `now` advances (monotonic — reconcile time never moves backwards) and
/// whether the owner is absent for the reconcile pass.
pub type PrefixStep = (Event, StampSpec, u64, bool);

/// A start timestamp plus an arbitrary event prefix.
pub fn arb_prefix() -> impl Strategy<Value = (Timestamp, Vec<PrefixStep>)> {
    (
        arb_timestamp(),
        prop_vec(
            (arb_event(), arb_stamp_spec(), 0_u64..120_000, any::<bool>()),
            0..64,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        StampSpec, arb_event, arb_prefix, arb_reserved_run, arb_run, arb_stale_spec,
        arb_stamped_event, arb_timestamp, arb_unsettled_run, judgment_record, run_key, stamp,
        stamped, test_policy, triple_of,
    };
    use std::time::Duration;

    use governor_core::lifecycle::Event;
    use proptest::strategy::{Strategy as _, ValueTree as _};
    use proptest::test_runner::TestRunner;

    /// Standalone target too — driving every shared item once keeps both
    /// compilations warning-free and smoke-checks the generators.
    #[test]
    fn strategies_produce_values() {
        let mut runner = TestRunner::deterministic();
        macro_rules! drives {
            ($strategy:expr) => {
                assert!(
                    $strategy.new_tree(&mut runner).is_ok(),
                    "strategy drives a value tree"
                )
            };
        }
        drives!(arb_timestamp());
        drives!(arb_run());
        drives!(arb_unsettled_run());
        drives!(arb_reserved_run());
        drives!(arb_event());
        drives!(arb_stamped_event());
        drives!(arb_prefix());
        drives!(arb_stale_spec());
        let generated = arb_reserved_run()
            .new_tree(&mut runner)
            .map(|tree| tree.current())
            .ok();
        assert!(generated.is_some(), "reserved run generated");
        if let Some(run) = generated {
            let triple = triple_of(&run);
            assert!(
                stamp(&run, StampSpec::Fresh) == triple,
                "fresh stamp holds the run triple"
            );
            let versioned = stamped(&run, Event::Restart, StampSpec::Fresh);
            assert!(
                versioned.requested_against == triple,
                "stamped wraps the triple"
            );
            let record = judgment_record(&run.id, Some(triple), Vec::new());
            assert_eq!(record.set.run, Some(run.id), "record binds the run");
            assert_eq!(run_key("close").0, "run:r-1:close", "key prefix");
        }
        assert_eq!(test_policy().idle_window, Duration::from_mins(15), "policy");
    }
}
