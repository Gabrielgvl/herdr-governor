//! F22 — the generator-coverage floor: the seeded-prefix strategy must
//! actually reach every unsettled state and produce every settlement. A
//! deterministic runner draws `DRAWS` worlds from the same strategy the
//! safety property uses and asserts each unsettled state is the deepest
//! reached in at least 8% of the draws — a future narrowing of the
//! generator fails loudly here.

use std::collections::BTreeMap;

use governor_core::identity::Timestamp;
use governor_core::lifecycle::{State, periodic_review, transition};
use proptest::strategy::{Strategy as _, ValueTree as _};
use proptest::test_runner::TestRunner;

use crate::strategies::{self as arb, FREEZE_PATH, Sim};

/// Draws per run — the distribution is deterministic under
/// `TestRunner::deterministic`.
const DRAWS: usize = 2_000;

/// 8% of `DRAWS` — the deepest-state floor per unsettled state.
const MIN_DEEPEST: usize = 160;

/// The unsettled states the floor covers — `settled` is terminal, never a
/// depth.
const UNSETTLED: [State; 6] = [
    State::Reserved,
    State::Starting,
    State::Prompting,
    State::Active,
    State::Judging,
    State::Repair,
];

/// F22 — the seeded-prefix generator's coverage floor. Each draw replays
/// the prefix against a Run seeded by the real transitions (the same
/// strategy `f22_safety_over_event_prefixes` uses) and records the
/// deepest unsettled state reached and the settlement, if any.
#[test]
fn f22_safety_prefixes_reach_every_state() {
    let policy = arb::test_policy();
    let mut runner = TestRunner::deterministic();
    let mut deepest: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut settlements: BTreeMap<&'static str, usize> = BTreeMap::new();
    for _ in 0..DRAWS {
        let Ok(tree) = arb::arb_seeded_prefix().new_tree(&mut runner) else {
            panic!("the seeded-prefix strategy must always generate");
        };
        let world = tree.current();
        let mut sim = Sim {
            run: world.run,
            journal: world.journal,
            handoffs: world.handoffs,
            decision: world.decision,
        };
        let mut now = world.start;
        let mut depth = sim.run.state;
        for (event, spec, delta, owner_absent) in world.steps {
            now = Timestamp(
                now.0
                    .saturating_add(i64::try_from(delta).unwrap_or(i64::MAX)),
            );
            let stamped = arb::stamped(&sim.run, event, spec);
            let outcome = transition(
                &sim.run,
                &stamped,
                now,
                &policy,
                (sim.decision.as_ref(), &sim.journal, &sim.handoffs),
                FREEZE_PATH,
            );
            sim.apply(&outcome);
            if let Some(effect) = periodic_review(&sim.run, owner_absent, &sim.journal) {
                sim.journal.push(effect);
            }
            if sim.run.state != State::Settled && sim.run.state > depth {
                depth = sim.run.state;
            }
            if sim.run.state == State::Settled {
                break;
            }
        }
        let depth_count = deepest.entry(depth.as_str()).or_default();
        *depth_count = (*depth_count).saturating_add(1);
        if let Some(settlement) = sim.run.settlement {
            let settled = settlements.entry(settlement.as_str()).or_default();
            *settled = (*settled).saturating_add(1);
        }
    }
    eprintln!("deepest state per draw (of {DRAWS}):");
    for (state, count) in &deepest {
        eprintln!("  {state}: {count}");
    }
    eprintln!("settlements:");
    for (settlement, count) in &settlements {
        eprintln!("  {settlement}: {count}");
    }
    for state in UNSETTLED {
        let count = deepest.get(state.as_str()).copied().unwrap_or_default();
        assert!(
            count >= MIN_DEEPEST,
            "{} must be the deepest state reached in at least 8% of {DRAWS} draws",
            state.as_str()
        );
    }
    for settlement in [
        "accepted",
        "rejected",
        "no_handoff",
        "pane_lost",
        "cancelled",
        "provider_limited",
        "unresolved",
    ] {
        assert!(
            settlements.contains_key(settlement),
            "settlement {settlement} must occur in {DRAWS} draws"
        );
    }
}
