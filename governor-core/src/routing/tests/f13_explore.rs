//! F13 step 5 — the exploration lottery: deterministic on
//! `sha256(caller ‖ idempotencyKey)`, strictly below the policy rate, one
//! tier down, and never on changed, boundary, or recovery work.

use alloc::vec::Vec;

use crate::config::{OperatingPointId, Tier, args_digest};
use crate::identity::{IdempotencyKey, RunId};
use crate::routing::{ChangesFiles, Exploration, exploration_assigned, route};

use super::builders::{
    caller, config_with, decision, evaluation, launch, point, policy, predecessor_run,
};

#[test]
fn f13_exploration_assigned_lowers_one_tier() {
    let mut policy = policy();
    policy.exploration_rate = 1.0;
    let config = config_with(
        policy,
        Vec::from([
            point("one-down", "t1", 1, "p-a"),
            point("at", "t2", 1, "p-b"),
        ]),
    );
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.exploration,
        Exploration {
            assigned: true,
            executed: true,
        }
    );
    assert_eq!(
        decided.start_tier,
        Tier("t1".into()),
        "the lottery lowers the start exactly one tier"
    );
    assert_eq!(
        decided.candidates[0].operating_point,
        OperatingPointId("one-down".into())
    );
}

#[test]
fn f13_exploration_is_deterministic_on_caller_and_key() {
    // The same caller and key always land the same way; find a losing key
    // and a winning key at rate 0.5 and pin both outcomes through route.
    let mut tuned = policy();
    tuned.exploration_rate = 0.5;
    let config = config_with(
        tuned,
        Vec::from([point("lower", "t1", 1, "p-a"), point("at", "t2", 1, "p-b")]),
    );
    let wins = |key: &str| exploration_assigned(&caller(), &IdempotencyKey(key.into()), 0.5);
    let mut winner = None;
    let mut loser = None;
    for i in 0..4096_u32 {
        let key = alloc::format!("key-{i}");
        if wins(&key) {
            winner.get_or_insert(key);
        } else {
            loser.get_or_insert(key);
        }
        if winner.is_some() && loser.is_some() {
            break;
        }
    }
    let winning_key = winner.expect("a winning key within 4096 samples at rate 0.5");
    let losing_key = loser.expect("a losing key within 4096 samples at rate 0.5");
    let won = decision(route(
        &launch(None, None, &winning_key),
        None,
        &evaluation("t2", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert!(won.exploration.assigned);
    assert_eq!(won.start_tier, Tier("t1".into()));
    let lost = decision(route(
        &launch(None, None, &losing_key),
        None,
        &evaluation("t2", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert!(!lost.exploration.assigned);
    assert_eq!(lost.start_tier, Tier("t2".into()));
    // A zero rate assigns no one.
    let mut zero_rate = policy();
    zero_rate.exploration_rate = 0.0;
    let zero_config = config_with(zero_rate, Vec::from([point("at", "t2", 1, "p-b")]));
    let unrouted = decision(route(
        &launch(None, None, &winning_key),
        None,
        &evaluation("t2", ChangesFiles::None, 0.1),
        &zero_config,
        &[],
        &[],
        &[],
    ));
    assert!(!unrouted.exploration.assigned);
}

#[test]
fn f13_exploration_boundary_is_strictly_below_the_rate() {
    let caller = caller();
    let key = IdempotencyKey("boundary".into());
    let digest = args_digest(
        [
            caller.agent_kind.0.as_str(),
            caller.native_session.0.as_str(),
            key.0.as_str(),
        ]
        .into_iter(),
    );
    let [b0, b1, b2, b3, ..] = digest.0;
    let fraction = f64::from(u32::from_be_bytes([b0, b1, b2, b3])) / 4_294_967_296.0;
    assert!(
        !exploration_assigned(&caller, &key, fraction),
        "a rate equal to the caller's own fraction is not below the rate"
    );
    let next_rate = f64::from_bits(fraction.to_bits().saturating_add(1));
    assert!(
        exploration_assigned(&caller, &key, next_rate),
        "the next representable rate admits the same caller"
    );
}

#[test]
fn f13_recovery_of_without_predecessor_suppresses_exploration() {
    let mut policy = policy();
    policy.exploration_rate = 1.0;
    let config = config_with(policy, Vec::from([point("at", "t2", 1, "p-a")]));
    let decided = decision(route(
        &launch(None, Some(RunId("r-9".into())), "key"),
        None,
        &evaluation("t2", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert!(
        !decided.exploration.assigned,
        "the recovery_of marker alone — predecessor not yet loaded — suppresses exploration"
    );
    assert_eq!(decided.start_tier, Tier("t2".into()));
    assert_eq!(decided.recovery_minimum, None);
}

#[test]
fn f13_exploration_never_on_changes_boundary_or_recovery() {
    let mut policy = policy();
    policy.exploration_rate = 1.0;
    let points = Vec::from([point("at", "t2", 1, "p-a")]);
    for evaluated in [
        evaluation("t2", ChangesFiles::Few, 0.1),
        evaluation("t2", ChangesFiles::None, 0.9),
    ] {
        let config = config_with(policy.clone(), points.clone());
        let decided = decision(route(
            &launch(None, None, "key"),
            None,
            &evaluated,
            &config,
            &[],
            &[],
            &[],
        ));
        assert!(
            !decided.exploration.assigned,
            "exploration is off for file changes and security boundaries"
        );
        assert_eq!(decided.start_tier, Tier("t2".into()));
    }
    let predecessor = predecessor_run(Some("t0"), Some("p-x"));
    let config = config_with(policy, points);
    let decided = decision(route(
        &launch(None, Some(RunId("r-0".into())), "key"),
        Some(&predecessor),
        &evaluation("t2", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert!(
        !decided.exploration.assigned,
        "a recovery launch never explores"
    );
}

#[test]
fn f13_exploration_never_below_the_lowest_tier() {
    let mut policy = policy();
    policy.exploration_rate = 1.0;
    let config = config_with(policy, Vec::from([point("op", "t0", 1, "p-a")]));
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t0", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.exploration,
        Exploration {
            assigned: true,
            executed: false,
        },
        "assigned but cannot drop below the lowest tier"
    );
    assert_eq!(decided.start_tier, Tier("t0".into()));
}
