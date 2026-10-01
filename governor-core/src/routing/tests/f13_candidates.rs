//! F13 step 6 — the candidate chain: cost-class order over catalog order,
//! the exact harness and launch arguments persisted with the decision, the
//! claimed-and-currently-qualified gate, and the cooling-provider skip —
//! plus the F26 qualification digest and F21 cooldown rules it depends on.

use alloc::borrow::ToOwned as _;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{Capability, ConfigVersion, OperatingPointId, Provider, Tier, args_digest};
use crate::identity::Timestamp;
use crate::recovery::Cooldown;
use crate::routing::{ChangesFiles, cooling_down, route};
use crate::task::AbstainReason;

use super::builders::{
    abstention, config_with, decision, evaluation, launch, point, policy, qualification,
};

// ---- step 6: the candidate chain -------------------------------------------

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
