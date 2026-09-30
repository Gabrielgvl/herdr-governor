//! F13 — the ordered routing function: judgment validation, policy
//! adjustments, caller uplift, the recovery minimum, exploration, and the
//! candidate chain — plus the F26 qualification digest and F21 cooldown
//! rules step 6 depends on.

use alloc::borrow::ToOwned as _;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{Capability, ConfigVersion, OperatingPointId, Provider, Tier, args_digest};
use crate::identity::{RunId, Timestamp};
use crate::recovery::Cooldown;
use crate::routing::{
    Candidate, ChangesFiles, Decision, Exploration, Probability, cooling_down, route,
};
use crate::task::AbstainReason;

use super::builders::{
    abstention, config_with, decision, evaluation, launch, point, policy, predecessor_run,
    qualification,
};

// ---- the ordered steps ----------------------------------------------------

#[test]
fn f13_candidates_ordered_cost_then_catalog() {
    let points = Vec::from([
        point("expensive-first", "t2", 9, "p-a"),
        point("cheap", "t3", 1, "p-b"),
        point("catalog-earlier", "t3", 1, "p-c"),
        point("below-start", "t1", 0, "p-d"),
    ]);
    let config = config_with(policy(), points);
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    let ids: Vec<String> = decided
        .candidates
        .iter()
        .map(|candidate| candidate.operating_point.0.clone())
        .collect();
    assert_eq!(
        ids,
        Vec::from([
            "cheap".to_owned(),
            "catalog-earlier".to_owned(),
            "expensive-first".to_owned()
        ]),
        "cost class first, catalog order on ties, tiers below the start excluded"
    );
    assert_eq!(decided.start_tier, Tier("t2".into()));
}

#[test]
fn f13_candidate_carries_exact_harness_and_args() {
    let mut start = point("exact", "t2", 1, "p-a");
    start.harness = crate::identity::AgentKind("kind-x".into());
    start.args = Vec::from(["--one".into(), "--two".into()]);
    let config = config_with(policy(), Vec::from([start]));
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    let candidate = &decided.candidates[0];
    assert_eq!(
        candidate.harness,
        crate::identity::AgentKind("kind-x".into())
    );
    assert_eq!(
        candidate.args,
        Vec::from(["--one".to_owned(), "--two".to_owned()]),
        "the decision persists the point's exact launch arguments verbatim"
    );
    assert_eq!(decided.config_version, ConfigVersion("cfg-1".into()));
}

#[test]
fn f13_candidate_requires_current_qualification() {
    let mut claimed = point("claimed", "t2", 1, "p-a");
    claimed.capabilities = Vec::from([Capability("web".into())]);
    let unclaimed = point("unclaimed", "t2", 1, "p-b");
    let mut other_args = point("other-args", "t2", 1, "p-c");
    other_args.capabilities = Vec::from([Capability("web".into())]);
    let mut failed = point("failed", "t2", 1, "p-d");
    failed.capabilities = Vec::from([Capability("web".into())]);
    // `other_args` qualified against different args — its pass is stale.
    let stale_args = {
        let mut qualification = qualification(&other_args, "web", true);
        qualification.args_digest = args_digest(["--old"].into_iter());
        qualification
    };
    let qualifications = Vec::from([
        qualification(&claimed, "web", true),
        stale_args,
        qualification(&failed, "web", false),
    ]);
    let required = Vec::from([Capability("web".into())]);
    let config = config_with(
        policy(),
        Vec::from([claimed, unclaimed, other_args, failed]),
    );
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &required,
        &qualifications,
        &[],
    ));
    let ids: Vec<String> = decided
        .candidates
        .iter()
        .map(|candidate| candidate.operating_point.0.clone())
        .collect();
    assert_eq!(
        ids,
        Vec::from(["claimed".to_owned()]),
        "claimed and current-passed qualifies; unclaimed, stale-args and failed do not"
    );
}

#[test]
fn f13_no_candidates_abstains() {
    let config = config_with(policy(), Vec::from([point("low", "t0", 1, "p-a")]));
    assert_eq!(
        abstention(route(
            &launch(None, None, "key"),
            None,
            &evaluation("t3", ChangesFiles::Few, 0.1),
            &config,
            &[],
            &[],
            &[],
        )),
        AbstainReason::NoCandidates
    );
}

#[test]
fn f13_cooling_provider_is_not_a_candidate() {
    let points = Vec::from([
        point("cooling", "t2", 1, "p-hot"),
        point("fine", "t2", 2, "p-cold"),
    ]);
    let config = config_with(policy(), points);
    let cooling = Vec::from([Provider("p-hot".into())]);
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &cooling,
    ));
    assert_eq!(decided.candidates.len(), 1);
    assert_eq!(
        decided.candidates[0].provider,
        Provider("p-cold".into()),
        "a provider cooling down is skipped even when cheaper"
    );
    // The last candidate cooling means abstain.
    assert_eq!(
        abstention(route(
            &launch(None, None, "key"),
            None,
            &evaluation("t2", ChangesFiles::Few, 0.1),
            &config,
            &[],
            &[],
            &[Provider("p-hot".into()), Provider("p-cold".into())],
        )),
        AbstainReason::NoCandidates
    );
}

#[test]
fn f13_no_change_cap_lowers_the_start() {
    let mut policy = policy();
    policy.no_change_cap = Some(Tier("t1".into()));
    let config = config_with(policy, Vec::from([point("op", "t1", 1, "p-a")]));
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t4", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.start_tier,
        Tier("t1".into()),
        "a no-change, no-boundary task is capped by policy"
    );
    assert_eq!(decided.policy_cap, Some(Tier("t1".into())));
    assert_eq!(decided.judged_tier, Tier("t4".into()));
}

#[test]
fn f13_cap_never_raises_and_is_skipped_by_change_or_boundary() {
    let mut policy = policy();
    policy.no_change_cap = Some(Tier("t3".into()));
    // Judged below the cap — the cap does not raise.
    let points = Vec::from([point("low", "t0", 1, "p-a"), point("top", "t5", 1, "p-b")]);
    let config = config_with(policy.clone(), points);
    let below_cap = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t0", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(below_cap.start_tier, Tier("t0".into()));
    assert_eq!(
        below_cap.policy_cap,
        Some(Tier("t3".into())),
        "the applied cap is recorded even when it does not bind"
    );
    // File changes — the cap does not apply.
    let changed = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t5", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(changed.start_tier, Tier("t5".into()));
    assert_eq!(changed.policy_cap, None);
    // A security boundary — the cap does not apply.
    let bounded = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t5", ChangesFiles::None, 0.9),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(bounded.start_tier, Tier("t5".into()));
    assert_eq!(bounded.policy_cap, None);
}

#[test]
fn f13_security_and_broad_floors_raise_the_start() {
    let mut policy = policy();
    policy.security_floor = Some(Tier("t4".into()));
    policy.broad_change_floor = Some(Tier("t2".into()));
    let config = config_with(
        policy,
        Vec::from([point("op", "t4", 1, "p-a"), point("top", "t5", 2, "p-b")]),
    );
    // Security boundary — the higher floor wins (ADR-0004).
    let both_floors = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t1", ChangesFiles::Broad, 0.9),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        both_floors.start_tier,
        Tier("t4".into()),
        "both floors apply — the higher wins"
    );
    assert_eq!(both_floors.policy_floor, Some(Tier("t4".into())));
    // Broad alone — only its floor.
    let broad_only = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t1", ChangesFiles::Broad, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(broad_only.start_tier, Tier("t2".into()));
    // Floors never lower a judged tier already above them.
    let above = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t5", ChangesFiles::Broad, 0.9),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(above.start_tier, Tier("t5".into()));
    assert_eq!(above.policy_floor, Some(Tier("t4".into())));
}

#[test]
fn f13_the_higher_floor_wins_when_broad_exceeds_security() {
    let mut policy = policy();
    policy.security_floor = Some(Tier("t1".into()));
    policy.broad_change_floor = Some(Tier("t4".into()));
    let config = config_with(
        policy,
        Vec::from([point("op", "t4", 1, "p-a"), point("top", "t5", 1, "p-b")]),
    );
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t0", ChangesFiles::Broad, 0.9),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.start_tier,
        Tier("t4".into()),
        "both floors apply — the broad-change floor sits above the security floor here"
    );
    assert_eq!(decided.policy_floor, Some(Tier("t4".into())));
}

#[test]
fn f13_caller_uplift_is_bounded_to_one_tier() {
    let config = config_with(
        policy(),
        Vec::from([
            point("at-start", "t3", 1, "p-a"),
            point("higher", "t5", 2, "p-b"),
        ]),
    );
    let decided = decision(route(
        &launch(Some("t5"), None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.start_tier,
        Tier("t3".into()),
        "uplift reaches at most one tier above the floor"
    );
    assert_eq!(decided.caller_uplift, Some(Tier("t3".into())));
    assert_eq!(decided.requested_tier, Some(Tier("t5".into())));
}

#[test]
fn f13_caller_uplift_never_lowers() {
    let config = config_with(policy(), Vec::from([point("op", "t4", 1, "p-a")]));
    let decided = decision(route(
        &launch(Some("t0"), None, "key"),
        None,
        &evaluation("t4", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.start_tier,
        Tier("t4".into()),
        "a lower request does not lower the start"
    );
    assert_eq!(decided.caller_uplift, Some(Tier("t4".into())));
    // A request outside the policy order cannot be applied, only recorded.
    let unrankable = decision(route(
        &launch(Some("platinum"), None, "key"),
        None,
        &evaluation("t4", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(unrankable.start_tier, Tier("t4".into()));
    assert_eq!(unrankable.caller_uplift, None);
    assert_eq!(unrankable.requested_tier, Some(Tier("platinum".into())));
}

#[test]
fn f13_recovery_minimum_one_tier_above_predecessor() {
    let config = config_with(
        policy(),
        Vec::from([point("low", "t1", 1, "p-b"), point("min", "t3", 1, "p-a")]),
    );
    let predecessor = predecessor_run(Some("t2"), Some("p-x"));
    let decided = decision(route(
        &launch(None, Some(RunId("r-0".into())), "key"),
        Some(&predecessor),
        &evaluation("t1", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.start_tier,
        Tier("t3".into()),
        "the start is at least one tier above the predecessor's start"
    );
    assert_eq!(decided.recovery_minimum, Some(Tier("t3".into())));
}

#[test]
fn f13_recovery_excludes_predecessor_provider() {
    let config = config_with(
        policy(),
        Vec::from([
            point("same-provider", "t3", 1, "p-x"),
            point("other", "t3", 2, "p-y"),
        ]),
    );
    let predecessor = predecessor_run(Some("t2"), Some("p-x"));
    let decided = decision(route(
        &launch(None, Some(RunId("r-0".into())), "key"),
        Some(&predecessor),
        &evaluation("t3", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(decided.candidates.len(), 1);
    assert_eq!(
        decided.candidates[0].provider,
        Provider("p-y".into()),
        "the failed provider's points are excluded even when cheaper"
    );
}

#[test]
fn f13_recovery_at_top_abstains_no_higher_tier() {
    let config = config_with(policy(), Vec::from([point("op", "t5", 1, "p-a")]));
    let predecessor = predecessor_run(Some("t5"), Some("p-x"));
    assert_eq!(
        abstention(route(
            &launch(None, Some(RunId("r-0".into())), "key"),
            Some(&predecessor),
            &evaluation("t5", ChangesFiles::Few, 0.1),
            &config,
            &[],
            &[],
            &[],
        )),
        AbstainReason::NoHigherTier,
        "no policy tier sits above the predecessor's start"
    );
    // A start that no longer names a policy tier cannot be raised either.
    let stale = predecessor_run(Some("retired"), Some("p-x"));
    assert_eq!(
        abstention(route(
            &launch(None, Some(RunId("r-0".into())), "key"),
            Some(&stale),
            &evaluation("t1", ChangesFiles::Few, 0.1),
            &config,
            &[],
            &[],
            &[],
        )),
        AbstainReason::NoHigherTier
    );
}

#[test]
fn f13_recovery_without_start_excludes_provider_only() {
    let config = config_with(policy(), Vec::from([point("op", "t1", 1, "p-y")]));
    let predecessor = predecessor_run(None, Some("p-x"));
    let decided = decision(route(
        &launch(None, Some(RunId("r-0".into())), "key"),
        Some(&predecessor),
        &evaluation("t1", ChangesFiles::Few, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided.recovery_minimum, None,
        "a predecessor that never started has no start to raise"
    );
    assert_eq!(decided.start_tier, Tier("t1".into()));
}
#[test]
fn f13_decision_records_the_routing_evidence() {
    let mut policy = policy();
    policy.no_change_cap = Some(Tier("t2".into()));
    let config = config_with(policy, Vec::from([point("op", "t3", 1, "p-a")]));
    let decided = decision(route(
        &launch(Some("t3"), None, "key"),
        None,
        &evaluation("t4", ChangesFiles::None, 0.1),
        &config,
        &[],
        &[],
        &[],
    ));
    assert_eq!(
        decided,
        Decision {
            judged_tier: Tier("t4".into()),
            requested_tier: Some(Tier("t3".into())),
            policy_cap: Some(Tier("t2".into())),
            policy_floor: None,
            caller_uplift: Some(Tier("t3".into())),
            recovery_minimum: None,
            exploration: Exploration {
                assigned: false,
                executed: false,
            },
            start_tier: Tier("t3".into()),
            candidates: Vec::from([Candidate {
                operating_point: OperatingPointId("op".into()),
                provider: Provider("p-a".into()),
                tier: Tier("t3".into()),
                harness: crate::identity::AgentKind("kind".into()),
                args: Vec::from(["--serve".into()]),
            }]),
            config_version: ConfigVersion("cfg-1".into()),
        },
        "the immutable decision records every stage the spec persists"
    );
}

#[test]
fn f13_route_rejects_a_malformed_evaluation() {
    let config = config_with(policy(), Vec::from([point("op", "t1", 1, "p-a")]));
    let mut bad_probability = evaluation("t1", ChangesFiles::Few, 0.1);
    bad_probability.needs_external = Probability(1.5);
    assert_eq!(
        abstention(route(
            &launch(None, None, "key"),
            None,
            &bad_probability,
            &config,
            &[],
            &[],
            &[],
        )),
        AbstainReason::EvaluationFailed,
        "a probability outside [0,1] is a malformed answer"
    );
    let bad_tier = evaluation("outside", ChangesFiles::Few, 0.1);
    assert_eq!(
        abstention(route(
            &launch(None, None, "key"),
            None,
            &bad_tier,
            &config,
            &[],
            &[],
            &[],
        )),
        AbstainReason::EvaluationFailed,
        "a judged tier outside the policy order is a malformed answer"
    );
}

// ---- supporting domain rules step 6 depends on -----------------------------

#[test]
fn f26_args_digest_rekeys_on_change() {
    let digest = args_digest(["--one", "--two"].into_iter());
    assert_eq!(
        digest,
        args_digest(["--one", "--two"].into_iter()),
        "the digest is deterministic"
    );
    assert_ne!(
        digest,
        args_digest(["--two", "--one"].into_iter()),
        "argument order is part of the qualification key"
    );
    assert_ne!(
        digest,
        args_digest(["--one--two"].into_iter()),
        "the length-prefixed encoding cannot concatenate-collide"
    );
    assert_ne!(
        digest,
        args_digest(["--one", "--two", "--three"].into_iter()),
        "an added argument re-keys the qualification"
    );
}

#[test]
fn f26_routing_and_qualification_share_args_digest() {
    // `qualify` persists `OperatingPoint::args_digest`; step 6 must consult
    // the same digest or a recorded pass could never match the live point.
    let mut qualified_point = point("qualified", "t2", 1, "p-a");
    qualified_point.capabilities = Vec::from([Capability("web".into())]);
    let recorded = qualification(&qualified_point, "web", true);
    assert_eq!(
        recorded.args_digest,
        qualified_point.args_digest(),
        "the qualification key is the point's own args digest"
    );
    let config = config_with(policy(), Vec::from([qualified_point]));
    let decided = decision(route(
        &launch(None, None, "key"),
        None,
        &evaluation("t2", ChangesFiles::Few, 0.1),
        &config,
        &[Capability("web".into())],
        &[recorded],
        &[],
    ));
    assert_eq!(
        decided.candidates[0].operating_point,
        OperatingPointId("qualified".into()),
        "a pass recorded under OperatingPoint::args_digest satisfies step 6"
    );
}

#[test]
fn f21_cooling_down_until_boundary() {
    let cooldowns = Vec::from([
        Cooldown {
            provider: Provider("hot".into()),
            until: Timestamp(100),
            reason: "provider_limited".into(),
            source_run: None,
        },
        Cooldown {
            provider: Provider("expired".into()),
            until: Timestamp(50),
            reason: "provider_limited".into(),
            source_run: None,
        },
    ]);
    let cooling = cooling_down(&cooldowns, Timestamp(50));
    assert_eq!(
        cooling,
        Vec::from([Provider("hot".into())]),
        "a cooldown holds strictly until its `until`"
    );
    assert_eq!(
        cooling_down(&cooldowns, Timestamp(100)),
        Vec::<Provider>::new(),
        "at `until` the cooldown has expired"
    );
}
