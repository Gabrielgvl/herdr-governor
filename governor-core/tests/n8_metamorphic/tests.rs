//! The N8 metamorphic property — the renamed world must behave identically
//! on every observable output (routing decision, lifecycle transition,
//! periodic review ask).

use governor_core::lifecycle::{periodic_review, transition};
use governor_core::routing::route;
use proptest::prelude::{ProptestConfig, prop_assert, prop_assert_eq, proptest};

use crate::strategies::world;

proptest! {
    // spec §7 N8 — the metamorphic property, >= 10_000 cases.
    #![proptest_config(ProptestConfig::with_cases(10_000))]
    #[test]
    fn n8_renaming_opaque_ids_changes_no_behaviour(case in world()) {
        let renamed = case.rename.world(&case);

        // F13 — the routing decision is identical modulo the renaming.
        let routed = route(
            &case.launch,
            case.predecessor.as_ref(),
            &case.evaluation,
            &case.config,
            &case.required,
            &case.qualifications,
            &case.cooling,
        );
        let routed_renamed = route(
            &renamed.launch,
            renamed.predecessor.as_ref(),
            &renamed.evaluation,
            &renamed.config,
            &renamed.required,
            &renamed.qualifications,
            &renamed.cooling,
        );
        match (routed.as_ref(), routed_renamed.as_ref()) {
            (Ok(decision), Ok(decision_renamed)) => prop_assert_eq!(
                case.rename.decision(decision),
                decision_renamed.clone(),
                "the routing decision must be identical modulo the renaming"
            ),
            (Err(reason), Err(reason_renamed)) => prop_assert_eq!(
                reason,
                reason_renamed,
                "the abstention must be identical under the renaming"
            ),
            (Ok(_), Err(_)) => prop_assert!(
                false,
                "renaming a valid decision produced an abstention"
            ),
            (Err(_), Ok(_)) => prop_assert!(
                false,
                "renaming an abstention produced a decision"
            ),
        }

        // Appendix C — the lifecycle transition is identical modulo the
        // renaming. The decision read is the one the route produced;
        // when the route abstains the launch never happened and the
        // transition reads no decision.
        let output = transition(
            &case.run,
            &case.event,
            case.now,
            &case.config.policy,
            (routed.as_ref().ok(), &case.journal, &case.handoffs),
            &case.freeze_path,
        );
        let output_renamed = transition(
            &renamed.run,
            &renamed.event,
            renamed.now,
            &renamed.config.policy,
            (
                routed_renamed.as_ref().ok(),
                &renamed.journal,
                &renamed.handoffs,
            ),
            &renamed.freeze_path,
        );
        prop_assert_eq!(
            case.rename.transition(&output),
            output_renamed,
            "the lifecycle transition must be identical modulo the renaming"
        );

        // F23 — the periodic review ask obeys the same opacity.
        prop_assert_eq!(
            case.rename.maybe_effect(periodic_review(
                &case.run,
                case.owner_absent,
                &case.journal,
            )
            .as_ref()),
            periodic_review(&renamed.run, renamed.owner_absent, &renamed.journal),
            "the periodic review must be identical modulo the renaming"
        );
    }
}
