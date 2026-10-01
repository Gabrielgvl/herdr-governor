//! Event-side generators: observations, readings, verdicts, deadlines,
//! judgments and effect results — everything a stamped `Event` delivers to
//! `transition` — plus the arbitrary event prefix the safety proof replays
//! and the standalone smoke test that drives every shared strategy. The
//! version-stamp items live in `stamp_strategies.rs`.

use std::collections::BTreeMap;

use governor_core::acceptance::HandoffReading;
use governor_core::config::ConfigVersion;
use governor_core::identity::{
    Digest, EffectKey, JudgmentSetId, NativeSession, Observation, PaneId, RunId, TabId, Timestamp,
};
use governor_core::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult, Event,
    JudgmentVerdict, VersionTriple,
};
use governor_core::routing::{
    Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability, Question,
    QuestionVersion,
};
use proptest::collection::vec as prop_vec;
use proptest::option;
use proptest::prelude::{Just, Strategy, any, prop_oneof};

use super::common_strategies::{RUN_ID, arb_digest, arb_timestamp, pick, text};
use super::run_strategies::{arb_child_status, arb_identity};
use super::stamp_strategies::{StampSpec, arb_stamp_spec, arb_version_triple};

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
        1 => arb_digest().prop_map(|digest| Event::Handoff { digest }),
        2 => arb_verdict().prop_map(Event::Judgment),
        2 => arb_deadline_kind().prop_map(Event::Deadline),
        1 => any::<bool>().prop_map(|close_pane| Event::Cancel { close_pane }),
        1 => Just(Event::ProviderLimited),
        1 => arb_digest().prop_map(|digest| Event::Evidence { digest }),
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
        arb_digest().prop_map(|digest| Event::Evidence { digest }),
        Just(Event::ProviderLimited),
    ]
}

/// One step of an arbitrary event prefix: the event, how it is stamped, how
/// far `now` advances (monotonic — reconcile time never moves backwards) and
/// whether the owner is absent for the reconcile pass.
pub type PrefixStep = (Event, StampSpec, u64, bool);

/// The step list of an event prefix — events, stamp specs, `now` deltas
/// (monotonic; reconcile time never moves backwards) and the reconcile
/// lane's owner-absence flag.
pub(crate) fn arb_steps() -> impl Strategy<Value = Vec<PrefixStep>> {
    prop_vec(
        (arb_event(), arb_stamp_spec(), 0_u64..120_000, any::<bool>()),
        0..64,
    )
}

/// A start timestamp plus an arbitrary event prefix.
pub fn arb_prefix() -> impl Strategy<Value = (Timestamp, Vec<PrefixStep>)> {
    (arb_timestamp(), arb_steps())
}

#[cfg(test)]
mod tests {
    use crate::strategies::{
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
