//! F13 — the ordered routing function itself: the six numbered steps run in
//! order and produce the immutable `Decision` the caller persists before
//! any topology effect — plus the F21 cooldown rule step 6 consumes.

use alloc::vec::Vec;

use crate::config::{Capability, Config, Provider, Qualification};
use crate::identity::Timestamp;
use crate::lifecycle::Run;
use crate::recovery::Cooldown;
use crate::task::{AbstainReason, Launch};

use super::{ChangesFiles, Decision, Evaluation, noul_yes, steps};

/// F13 — the ordered routing function. Steps run in order and the decision
/// is persisted before any topology effect (the caller owns the write):
///
/// 1. The typed evaluation is trusted only after `validate_evaluation`;
///    here the judged tier must still be a policy tier and every carried
///    probability still in contract.
/// 2. Policy adjustments: `no_change_cap` caps a Task with no file changes
///    and no security boundary; a security boundary or broad change raises
///    the floor.
/// 3. Caller uplift (`Task::tier`): at most one tier above the step-2 tier,
///    never lower (H#49). A request naming a tier outside `policy.tiers`
///    cannot be ordered — it is recorded and not applied.
/// 4. Recovery (F21): `predecessor` is the settled Run this Launch
///    continues. The start is at least one tier above the predecessor's
///    start — impossible at the top tier, or when that start no longer
///    names a policy tier, is `no_higher_tier` — and every operating point
///    on the predecessor's provider is excluded in step 6. A predecessor
///    that never started contributes only the provider exclusion.
/// 5. Exploration: only when the Task changes no files, touches no security
///    boundary and is not a recovery, and `sha256(caller ‖ idempotencyKey)`
///    falls below `policy.exploration_rate`. It lowers the start one tier,
///    never below the recovery minimum or the lowest tier.
/// 6. Candidates: catalog order filtered to points at or above the start
///    tier, outside every cooling provider and the predecessor's provider,
///    and offering each of `required` — claimed in `capabilities` AND backed
///    by a current `passed` qualification (`args_digest` of the live args);
///    ordered by cost class, then catalog order. Empty is `no_candidates`.
///
/// Call `evaluation_verdict` first — a `rejected` Launch is never routed.
/// `required` is the policy-resolved capability set for the Task's
/// judgments; the names are catalog data (N8), never literals here.
pub fn route(
    launch: &Launch,
    predecessor: Option<&Run>,
    evaluation: &Evaluation,
    config: &Config,
    required: &[Capability],
    qualifications: &[Qualification],
    cooling: &[Provider],
) -> Result<Decision, AbstainReason> {
    let policy = &config.policy;
    let tiers = policy.tiers.as_slice();
    let mut rank = steps::evaluated_rank(evaluation, tiers)?;
    let boundary = noul_yes(evaluation.security_boundary, None);
    let no_change = matches!(evaluation.changes_files, ChangesFiles::None) && !boundary;
    let (adjusted, policy_cap, policy_floor) =
        steps::policy_adjusted(evaluation, policy, no_change, boundary, rank);
    rank = adjusted;
    let (uplifted, caller_uplift) = steps::caller_uplifted(tiers, launch.task.tier.as_ref(), rank);
    rank = uplifted;
    let (floored, recovery_minimum, minimum_rank, excluded_provider) =
        steps::recovery_floored(predecessor, tiers, rank)?;
    rank = floored;
    let recovery = predecessor.is_some() || launch.task.recovery_of.is_some();
    let (explored, exploration) = steps::explore(
        &launch.caller,
        &launch.idempotency_key,
        policy.exploration_rate,
        no_change,
        recovery,
        rank,
        minimum_rank,
    );
    rank = explored;
    let Some(start_tier) = tiers.get(rank).cloned() else {
        return Err(AbstainReason::EvaluationFailed);
    };
    let candidates = steps::eligible_candidates(
        config,
        tiers,
        rank,
        excluded_provider.as_ref(),
        required,
        qualifications,
        cooling,
    )?;
    Ok(Decision {
        judged_tier: evaluation.weakest_sufficient_tier.clone(),
        requested_tier: launch.task.tier.clone(),
        policy_cap,
        policy_floor,
        caller_uplift,
        recovery_minimum,
        exploration,
        start_tier,
        candidates,
        config_version: config.version.clone(),
    })
}

/// F21/F13 step 6 — the providers currently cooling down: a cooldown holds
/// until its absolute `until`.
#[must_use]
pub fn cooling_down(cooldowns: &[Cooldown], now: Timestamp) -> Vec<Provider> {
    cooldowns
        .iter()
        .filter(|cooldown| cooldown.until > now)
        .map(|cooldown| cooldown.provider.clone())
        .collect()
}
