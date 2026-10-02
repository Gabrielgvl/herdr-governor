//! F20 — the staleness proof: stamps that no longer hold produce nothing.
//! For Jev results (`judgment`, `provider_limited`, `JevEvaluate` receipts)
//! "hold" means `work_generation` and `evidence_generation` — `version` is
//! the conditional write's compare-and-swap guard, never a staleness test.
//! Every other stamped result keeps the full triple.

use governor_core::lifecycle::{
    EffectKind, EffectReceipt, EffectResolution, EffectResult, EffectWrite, Event, StateChange,
    Versioned, transition,
};
use governor_core::routing::JudgmentOutcome;
use proptest::option;
use proptest::prelude::{ProptestConfig, prop_assert, prop_assert_eq, prop_assume, proptest};
use proptest::strategy::Strategy as _;

use crate::strategies as arb;
use crate::strategies::FREEZE_PATH;
use crate::strategies::journal_strategies::{
    arb_decision, arb_handoffs, arb_jev_result, arb_journal,
};
use crate::tests::is_quiet;

proptest! {
    #![proptest_config({
        let mut config = ProptestConfig::with_cases(10_000);
        config.failure_persistence = None;
        config
    })]

    /// F20 — a stale async result never applies: an `obs`, `handoff`,
    /// `deadline` or `evidence` stamped with versions that no longer hold
    /// produces the empty transition; a `judgment` or `provider_limited`
    /// does so only when a generation moved. A stale Jev receipt journals
    /// its set `stale` and nothing else.
    #[test]
    fn f20_stale_async_result_never_applies(
        run in arb::arb_run(),
        event in arb::arb_stamped_event(),
        spec in arb::arb_stale_spec(),
        field in arb::pick(&[
            arb::TripleField::WorkGeneration,
            arb::TripleField::EvidenceGeneration,
        ]),
        delta in 1_u64..=8,
        (suffix, _jev_outcome, judgments, _result_spec) in arb_jev_result(),
        journal in arb_journal(),
        handoffs in arb_handoffs(),
        decision in option::of(arb_decision()),
        now in arb::arb_timestamp(),
    ) {
        let policy = arb::test_policy();
        let read = (decision.as_ref(), journal.as_slice(), handoffs.as_slice());

        // the envelope stamp: a stale async result produces nothing — Jev
        // results stale on generations only, so theirs is perturbed there.
        let stamped = if matches!(
            event,
            Event::Judgment(..) | Event::ProviderLimited
        ) {
            arb::stamped(&run, event, arb::StampSpec::Perturbed(field, delta))
        } else {
            arb::stamped(&run, event, spec)
        };
        prop_assume!(stamped.requested_against != arb::triple_of(&run));
        let outcome = transition(&run, &stamped, now, &policy, read, FREEZE_PATH);
        prop_assert!(
            is_quiet(&outcome),
            "a stale async result produces nothing"
        );

        // the receipt stamp: a generation-moved Jev result journals
        // `stale` and applies nothing else. A `Judgments` receipt rides
        // only an acknowledgement — the typed resolution leaves no other
        // place for the stamp (OQ-11/F13).
        let stale_versions = arb::stamp(&run, arb::StampSpec::Perturbed(field, delta));
        let stale_result = EffectResult {
            key: arb::run_key(suffix),
            kind: EffectKind::JevEvaluate,
            resolution: EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::Judgments(arb::judgment_record(
                    &run.id,
                    Some(stale_versions),
                    judgments,
                ))),
            },
        };
        let current = Versioned {
            requested_against: arb::triple_of(&run),
            value: Event::EffectResult(stale_result.clone()),
        };
        let applied = transition(&run, &current, now, &policy, read, FREEZE_PATH);
        prop_assert!(applied.events.is_empty(), "a stale result emits no event");
        prop_assert!(applied.effects.is_empty(), "a stale result plans nothing");
        prop_assert_eq!(
            applied.state_changes.len(),
            1,
            "a stale result writes only its journal row"
        );
        let Some(StateChange::WriteEffect(write)) = applied.state_changes.first() else {
            panic!("a stale Jev result still journals its row");
        };
        prop_assert_eq!(write.key(), &stale_result.key, "the journal row is the result's");
        let EffectWrite::Result { resolution, .. } = write else {
            panic!("a result journals as a result commit");
        };
        match resolution.receipt() {
            Some(EffectReceipt::Judgments(marked)) => {
                prop_assert_eq!(
                    marked.set.outcome,
                    JudgmentOutcome::Stale,
                    "the journaled set records outcome stale"
                );
                prop_assert_eq!(
                    marked.set.versions,
                    Some(stale_versions),
                    "the journaled set keeps the stale stamp"
                );
            }
            Some(
                EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => {
                panic!("the stale arm builds a Judgments receipt");
            }
        }
    }

    /// F20 — `version` is the conditional write's compare-and-swap guard,
    /// never a staleness test for Jev results: a `judgment` or
    /// `provider_limited` stamped against a moved `version` applies exactly
    /// as a fresh stamp, and a `JevEvaluate` receipt stamped the same way
    /// journals its own outcome — never `stale` — and applies its
    /// consequences.
    #[test]
    fn f20_version_only_stamps_still_apply_jev_results(
        run in arb::arb_run(),
        event in arb::arb_stamped_event().prop_filter(
            "Jev results only",
            |event| matches!(event, Event::Judgment(..) | Event::ProviderLimited),
        ),
        delta in 1_u64..=u64::MAX,
        (suffix, _jev_outcome, judgments, _result_spec) in arb_jev_result(),
        journal in arb_journal(),
        handoffs in arb_handoffs(),
        decision in option::of(arb_decision()),
        now in arb::arb_timestamp(),
    ) {
        let policy = arb::test_policy();
        let read = (decision.as_ref(), journal.as_slice(), handoffs.as_slice());
        let mut moved = arb::triple_of(&run);
        moved.version = moved.version.saturating_add(delta);

        // the envelope stamp: identical to a fresh stamp.
        let perturbed = Versioned {
            requested_against: moved,
            value: event.clone(),
        };
        let fresh = Versioned {
            requested_against: arb::triple_of(&run),
            value: event,
        };
        let perturbed_t = transition(&run, &perturbed, now, &policy, read, FREEZE_PATH);
        let fresh_t = transition(&run, &fresh, now, &policy, read, FREEZE_PATH);
        prop_assert_eq!(
            perturbed_t.state_changes,
            fresh_t.state_changes,
            "a version-only stamp applies the Jev event exactly as fresh"
        );
        prop_assert_eq!(perturbed_t.events, fresh_t.events);
        prop_assert_eq!(perturbed_t.effects, fresh_t.effects);

        // the receipt stamp: not stale — the row journals the set's own
        // outcome and everything after the journal row is identical.
        let result = EffectResult {
            key: arb::run_key(suffix),
            kind: EffectKind::JevEvaluate,
            resolution: EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::Judgments(arb::judgment_record(
                    &run.id,
                    Some(moved),
                    judgments.clone(),
                ))),
            },
        };
        let reference = EffectResult {
            resolution: EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::Judgments(arb::judgment_record(
                    &run.id,
                    Some(arb::triple_of(&run)),
                    judgments,
                ))),
            },
            ..result.clone()
        };
        let wrap = |value| Versioned {
            requested_against: arb::triple_of(&run),
            value,
        };
        let applied = transition(
            &run,
            &wrap(Event::EffectResult(result)),
            now,
            &policy,
            read,
            FREEZE_PATH,
        );
        let expected = transition(
            &run,
            &wrap(Event::EffectResult(reference)),
            now,
            &policy,
            read,
            FREEZE_PATH,
        );
        prop_assert_eq!(applied.events, expected.events);
        prop_assert_eq!(applied.effects, expected.effects);
        prop_assert_eq!(
            applied.state_changes.len(),
            expected.state_changes.len(),
            "the same transition runs"
        );
        prop_assert_eq!(
            &applied.state_changes[1..],
            &expected.state_changes[1..],
            "every consequence is identical — only the journaled stamp differs"
        );
        let Some(StateChange::WriteEffect(write)) = applied.state_changes.first() else {
            panic!("a Jev result still journals its row");
        };
        let EffectWrite::Result { resolution, .. } = write else {
            panic!("a result journals as a result commit");
        };
        match resolution.receipt() {
            Some(EffectReceipt::Judgments(marked)) => {
                prop_assert_eq!(
                    marked.set.outcome,
                    JudgmentOutcome::Answered,
                    "a version-only stamp never marks the set stale"
                );
                prop_assert_eq!(
                    marked.set.versions,
                    Some(moved),
                    "the asked stamp journals verbatim"
                );
            }
            Some(
                EffectReceipt::AgentStarted { .. }
                | EffectReceipt::TabCreated { .. }
                | EffectReceipt::PaneCreated { .. },
            )
            | None => {
                panic!("the result carries a Judgments receipt");
            }
        }
    }
}
