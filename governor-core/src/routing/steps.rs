//! F13 — the numbered steps of `route`, split so each rule is one small
//! total function. Everything here is `pub(super)` routing internals; the
//! public surface is `route` in the parent module.

use alloc::vec::Vec;

use crate::config::{Capability, Config, OperatingPoint, Policy, Provider, Qualification, Tier};
use crate::identity::{CallerKey, IdempotencyKey};
use crate::lifecycle::{Run, Settlement};
use crate::task::AbstainReason;

use super::{
    Candidate, ChangesFiles, Evaluation, Exploration, exploration_assigned, policy_tier,
    tier_index, valid_probability,
};

/// Step 1 — validate the typed evaluation and rank the judged tier. Every
/// probability must still be contract-shaped (finite, `[0,1]`) and the
/// judged tier a policy tier; a bundle failing either is a malformed answer,
/// not a routing input.
pub(super) fn evaluated_rank(
    evaluation: &Evaluation,
    tiers: &[Tier],
) -> Result<usize, AbstainReason> {
    let probabilities = [
        evaluation.done_when_verifiable,
        evaluation.security_boundary,
        evaluation.needs_external,
        evaluation.long_running,
    ];
    if !probabilities.iter().all(|p| valid_probability(*p)) {
        return Err(AbstainReason::EvaluationFailed);
    }
    tier_index(tiers, &evaluation.weakest_sufficient_tier).ok_or(AbstainReason::EvaluationFailed)
}

/// Step 2 — policy adjustments: `(rank, policy_cap, policy_floor)`. A cap is
/// `min` against the judged tier (it never raises); floors are `max`, and
/// both floors together resolve to the higher (ADR-0004).
pub(super) fn policy_adjusted(
    evaluation: &Evaluation,
    policy: &Policy,
    no_change: bool,
    boundary: bool,
    mut rank: usize,
) -> (usize, Option<Tier>, Option<Tier>) {
    let mut policy_cap = None;
    let mut policy_floor = None;
    if no_change
        && let Some((cap, cap_index)) = policy_tier(&policy.tiers, policy.no_change_cap.as_ref())
    {
        policy_cap = Some(cap.clone());
        rank = rank.min(cap_index);
    }
    let mut floor: Option<(&Tier, usize)> = None;
    if boundary {
        floor = policy_tier(&policy.tiers, policy.security_floor.as_ref());
    }
    if matches!(evaluation.changes_files, ChangesFiles::Broad)
        && let Some((tier, index)) = policy_tier(&policy.tiers, policy.broad_change_floor.as_ref())
    {
        floor = match floor {
            Some((_, lower)) if lower >= index => floor,
            Some(_) | None => Some((tier, index)),
        };
    }
    if let Some((tier, floor_index)) = floor {
        policy_floor = Some(tier.clone());
        rank = rank.max(floor_index);
    }
    (rank, policy_cap, policy_floor)
}

/// Step 3 — caller uplift (H#49): `max(floor, min(requested, next(floor)))`.
/// A request outside `policy.tiers` is recorded (`Decision::requested_tier`)
/// but cannot order, so it applies nothing.
pub(super) fn caller_uplifted(
    tiers: &[Tier],
    requested_tier: Option<&Tier>,
    rank: usize,
) -> (usize, Option<Tier>) {
    let Some(requested) = requested_tier.and_then(|tier| tier_index(tiers, tier)) else {
        return (rank, None);
    };
    let applied = requested.min(rank.saturating_add(1)).max(rank);
    (applied, tiers.get(applied).cloned())
}

/// Step 4 — `(rank, recovery_minimum, minimum_rank, excluded_provider)`. A
/// `provider_limited` predecessor recovers at its own start tier — the
/// provider was the limit, not the tier — so the minimum is the start
/// itself and the top tier still recovers through another provider.
/// Every other predecessor's start must sit one full tier below a policy
/// tier, else `no_higher_tier`; a predecessor that never started has no
/// start to raise, so only its provider is excluded.
pub(super) fn recovery_floored(
    predecessor: Option<&Run>,
    tiers: &[Tier],
    rank: usize,
) -> Result<(usize, Option<Tier>, usize, Option<Provider>), AbstainReason> {
    let Some(predecessor_run) = predecessor else {
        return Ok((rank, None, 0, None));
    };
    let excluded_provider = predecessor_run.provider.clone();
    let Some(start) = predecessor_run.tier_start.as_ref() else {
        return Ok((rank, None, 0, excluded_provider));
    };
    let Some(previous) = tier_index(tiers, start) else {
        return Err(AbstainReason::NoHigherTier);
    };
    // §14 — the one-tier lift applies to every settlement except
    // `provider_limited`: a limit names the provider (still excluded
    // above), not the tier the predecessor started at.
    let minimum_rank = match predecessor_run.settlement {
        Some(Settlement::ProviderLimited) => previous,
        Some(
            Settlement::Accepted
            | Settlement::Rejected
            | Settlement::NoHandoff
            | Settlement::PaneLost
            | Settlement::Cancelled
            | Settlement::Unresolved { reason: _ },
        )
        | None => previous.saturating_add(1),
    };
    let Some(minimum) = tiers.get(minimum_rank) else {
        return Err(AbstainReason::NoHigherTier);
    };
    Ok((
        rank.max(minimum_rank),
        Some(minimum.clone()),
        minimum_rank,
        excluded_provider,
    ))
}

/// Step 5 — `(rank, exploration)`: the lottery decides whether the cohort
/// was assigned; it executes only when the start can still go one tier down
/// without crossing the recovery minimum or the lowest tier.
pub(super) fn explore(
    caller: &CallerKey,
    key: &IdempotencyKey,
    rate: f64,
    no_change: bool,
    recovery: bool,
    rank: usize,
    minimum_rank: usize,
) -> (usize, Exploration) {
    let assigned = no_change && !recovery && exploration_assigned(caller, key, rate);
    let executed = assigned && rank > minimum_rank;
    let lowered = if executed {
        rank.saturating_sub(1)
    } else {
        rank
    };
    (lowered, Exploration { assigned, executed })
}

/// Step 6 — the filtered, ordered candidate list. `operating_points` is in
/// catalog order; `sort_by_key` is stable, so equal costs keep catalog order.
pub(super) fn eligible_candidates(
    config: &Config,
    tiers: &[Tier],
    rank: usize,
    excluded_provider: Option<&Provider>,
    required: &[Capability],
    qualifications: &[Qualification],
    cooling: &[Provider],
) -> Result<Vec<Candidate>, AbstainReason> {
    let mut eligible: Vec<&OperatingPoint> = config
        .catalog
        .operating_points
        .iter()
        .filter(|point| {
            let tiered = tier_index(tiers, &point.tier).is_some_and(|index| index >= rank);
            let allowed_provider =
                excluded_provider != Some(&point.provider) && !cooling.contains(&point.provider);
            let offers = required
                .iter()
                .all(|capability| point.has_current_pass(capability, qualifications));
            tiered && allowed_provider && offers
        })
        .collect();
    eligible.sort_by_key(|point| point.cost_class);
    if eligible.is_empty() {
        return Err(AbstainReason::NoCandidates);
    }
    Ok(eligible
        .iter()
        .map(|point| Candidate {
            operating_point: point.id.clone(),
            provider: point.provider.clone(),
            tier: point.tier.clone(),
            harness: point.harness.clone(),
            args: point.args.clone(),
        })
        .collect())
}
