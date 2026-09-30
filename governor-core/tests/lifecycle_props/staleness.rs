//! F20 — the staleness proof: version stamps that no longer hold produce
//! nothing, and a stale Jev receipt journals `stale` and nothing else.

use governor_core::lifecycle::{
    EffectKind, EffectReceipt, EffectResult, Event, StateChange, Versioned, transition,
};
use governor_core::routing::JudgmentOutcome;
use proptest::option;
use proptest::prelude::{ProptestConfig, prop_assert, prop_assert_eq, prop_assume, proptest};

use crate::strategies as arb;
use crate::strategies::journal_strategies::{
    arb_decision, arb_handoffs, arb_jev_result, arb_journal,
};
use crate::tests::{FREEZE_PATH, is_quiet};

proptest! {
    #![proptest_config({
        let mut config = ProptestConfig::with_cases(10_000);
        config.failure_persistence = None;
        config
    })]

    /// F20 — a stale async result never applies: an `obs`, `handoff`,
    /// `judgment`, `deadline` or `provider_limited` stamped with
    /// versions that no longer hold produces the empty transition, and a
    /// stale Jev receipt journals its set `stale` and nothing else.
    #[test]
    fn f20_stale_async_result_never_applies(
        run in arb::arb_run(),
        event in arb::arb_stamped_event(),
        spec in arb::arb_stale_spec(),
        (suffix, jev_outcome, judgments, result_spec) in arb_jev_result(),
        journal in arb_journal(),
        handoffs in arb_handoffs(),
        decision in option::of(arb_decision()),
        now in arb::arb_timestamp(),
    ) {
        let policy = arb::test_policy();
        let read = (decision.as_ref(), journal.as_slice(), handoffs.as_slice());

        // the envelope stamp: a stale async result produces nothing
        let stamped = arb::stamped(&run, event, spec);
        prop_assume!(stamped.requested_against != arb::triple_of(&run));
        let outcome = transition(&run, &stamped, now, &policy, read, FREEZE_PATH);
        prop_assert!(
            is_quiet(&outcome),
            "a stale async result produces nothing"
        );

        // the receipt stamp: a stale Jev result journals `stale` and
        // applies nothing else
        let stale_versions = arb::stamp(&run, result_spec);
        prop_assume!(stale_versions != arb::triple_of(&run));
        let stale_result = EffectResult {
            key: arb::run_key(suffix),
            kind: EffectKind::JevEvaluate,
            outcome: jev_outcome,
            receipt: Some(EffectReceipt::Judgments(arb::judgment_record(
                &run.id,
                Some(stale_versions),
                judgments,
            ))),
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
        prop_assert_eq!(&write.key, &stale_result.key, "the journal row is the result's");
        match &write.receipt {
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
}
