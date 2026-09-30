//! F12 — Jev's judgments and the questions it answers; F13 — the ordered
//! routing function's persisted decision; F14 — the placement plan. Jev
//! judges Tasks, Runs and Handoffs, never models (ADR-0001); Jev never sees
//! operating points (F12).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{
    Capability, Config, ConfigVersion, OperatingPointId, Provider, Qualification, Tier, args_digest,
};
use crate::identity::{
    AgentKind, CallerKey, Digest, IdempotencyKey, JudgmentSetId, LaunchId, RunId, TabId, Timestamp,
};
use crate::lifecycle::{Run, VersionTriple};
use crate::recovery::Cooldown;
use crate::task::{AbstainReason, Launch};

mod eval;
mod steps;

pub use eval::{
    evaluation_abstention, evaluation_effect_abstention, evaluation_questions, evaluation_verdict,
    request_size_outcome, validate_evaluation,
};

/// N5 — a Jev request over 96 KiB abstains (it is never sent).
pub const JEV_REQUEST_MAX_BYTES: usize = 96 * 1024;

/// N5 — the transcript evidence window: the tail, taken deterministically.
pub const TRANSCRIPT_WINDOW_MAX_BYTES: usize = 32 * 1024;

/// F14 — the Jev-picked tab is used only while it holds fewer than this many
/// panes; otherwise a new tab is opened.
pub const TAB_PANE_MAX: usize = 4;

/// A calibrated probability as Jev reports it, in `[0, 1]`
/// (contract: `jev_noul_answer_shape`/`jev_choice_answer_shape`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probability(pub f64);

/// The question-set version that produced a judgment — judgments are bound to
/// it (F24; Appendix B `judgment_sets.question_version`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuestionVersion(pub String);

/// F12/F23/F24 — the questions Jev answers. The wire name is `as_str()`
/// (`handoff_meets_item_k` renders as `"handoff_meets_item_<item>"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Question {
    /// F12 — `done_when_verifiable` (noul): is the Task's doneWhen verifiable.
    DoneWhenVerifiable,
    /// F12 — `weakest_sufficient_tier`: a choice over the policy tiers.
    WeakestSufficientTier,
    /// F12 — `changes_files`: `none`, `few` or `broad`.
    ChangesFiles,
    /// F12 — `security_boundary` (noul).
    SecurityBoundary,
    /// F12 — `needs_external` (noul).
    NeedsExternal,
    /// F12 — `long_running` (noul).
    LongRunning,
    /// F12 — `related_tab`: a choice over the caller's open governor tabs
    /// plus `new`; asked only when such tabs exist.
    RelatedTab,
    /// F23 — `blocked_on_input` (noul): the child waits on input → event.
    BlockedOnInput,
    /// F23 — `no_recent_progress` (noul): stall evidence → one nudge per
    /// episode.
    NoRecentProgress,
    /// F23 — `outside_scope` (noul; also receives `scope`) → event.
    OutsideScope,
    /// F23 — `provider_limited` (noul), asked when Herdr reports `blocked` →
    /// F21 when it clears the policy threshold.
    ProviderLimited,
    /// F24 — `handoff_meets_item_k`: does the frozen handoff plus transcript
    /// and git evidence satisfy doneWhen item `item`.
    HandoffMeetsItem {
        /// Which `done_when` item this judgment covers.
        item: u8,
    },
}

impl Question {
    /// The spec spelling of the question; for `HandoffMeetsItem` this is the
    /// family prefix — the stored name appends `_<item>` (F24
    /// `handoff_meets_item_k`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DoneWhenVerifiable => "done_when_verifiable",
            Self::WeakestSufficientTier => "weakest_sufficient_tier",
            Self::ChangesFiles => "changes_files",
            Self::SecurityBoundary => "security_boundary",
            Self::NeedsExternal => "needs_external",
            Self::LongRunning => "long_running",
            Self::RelatedTab => "related_tab",
            Self::BlockedOnInput => "blocked_on_input",
            Self::NoRecentProgress => "no_recent_progress",
            Self::OutsideScope => "outside_scope",
            Self::ProviderLimited => "provider_limited",
            Self::HandoffMeetsItem { item: _ } => "handoff_meets_item",
        }
    }
}

/// F12 — the `changes_files` answer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangesFiles {
    /// `none` — the Task changes no files.
    None,
    /// `few` — a small number of files.
    Few,
    /// `broad` — broad change; raises the policy floor (F13 step 2).
    Broad,
}

impl ChangesFiles {
    /// F12 — the spec spelling of the answer.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Few => "few",
            Self::Broad => "broad",
        }
    }
}

/// F12 — the `related_tab` answer: one of the caller's open governor tabs, or
/// `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabChoice {
    /// An existing caller tab the pane may join (subject to `TAB_PANE_MAX`).
    Tab(TabId),
    /// `new` — open a new tab.
    New,
}

/// Appendix B `judgment_sets.purpose` — what a judgment set was requested for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JudgmentPurpose {
    /// `launch` — the F12 evaluation of a Task.
    Launch,
    /// `review` — an F23 supervision pass over fresh evidence.
    Review,
    /// `acceptance` — the F24 per-item handoff assessment.
    Acceptance,
    /// `provider_limit` — the F21 `provider_limited` question.
    ProviderLimit,
}

impl JudgmentPurpose {
    /// Appendix B — the stored spelling of the purpose.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Review => "review",
            Self::Acceptance => "acceptance",
            Self::ProviderLimit => "provider_limit",
        }
    }
}

/// Appendix B `judgment_sets.outcome` — how a Jev request resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JudgmentOutcome {
    /// `answered` — a valid response arrived in bounds.
    Answered,
    /// `transport_failed` — the request never got a response (F12).
    TransportFailed,
    /// `auth_failed` — the Jev credential was refused (F12).
    AuthFailed,
    /// `invalid_response` — the response did not satisfy the contract.
    InvalidResponse,
    /// `too_large` — the request exceeded `JEV_REQUEST_MAX_BYTES` (N5/F12).
    TooLarge,
    /// `stale` — the result arrived after the versions it was requested
    /// against stopped holding (F20).
    Stale,
}

impl JudgmentOutcome {
    /// Appendix B — the stored spelling of the outcome.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::TransportFailed => "transport_failed",
            Self::AuthFailed => "auth_failed",
            Self::InvalidResponse => "invalid_response",
            Self::TooLarge => "too_large",
            Self::Stale => "stale",
        }
    }
}

/// Appendix B `judgments` — one answered question inside a set: the full
/// probability distribution plus the resolved answer value.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgment {
    /// The question answered.
    pub question: Question,
    /// `probabilities_json` — the calibrated distribution over the answer
    /// space.
    pub probabilities: BTreeMap<String, Probability>,
    /// `answer` — the resolved value (a label, or the noul verdict).
    pub answer: String,
    /// `threshold` — the policy threshold applied to this judgment, when one
    /// applies.
    pub threshold: Option<f64>,
}

/// Appendix B `judgment_sets` — one bounded Jev request: what it was for,
/// which subject and evidence it bound, and how it resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgmentSet {
    /// `set_id`.
    pub id: JudgmentSetId,
    /// `purpose`.
    pub purpose: JudgmentPurpose,
    /// `launch_id` — set for launch evaluations (a set binds a launch or a
    /// run; Appendix B CHECK).
    pub launch: Option<LaunchId>,
    /// `run_id` — set for run-bound purposes.
    pub run: Option<RunId>,
    /// `run_version`/`work_generation`/`evidence_generation` — the versions
    /// the set was requested against (F20); `None` for `launch` sets, which
    /// predate the Run.
    pub versions: Option<VersionTriple>,
    /// `task_digest` — the canonical Task the judgment is bound to.
    pub task_digest: Digest,
    /// `handoff_digest` — the frozen handoff judged (acceptance sets).
    pub handoff_digest: Option<Digest>,
    /// `evidence_digest` — the transcript/git evidence window judged.
    pub evidence_digest: Option<Digest>,
    /// `model` — the Jev model that answered.
    pub model: String,
    /// `question_version` — the question-set version bound in the assessment
    /// key (F24).
    pub question_version: QuestionVersion,
    /// `policy_version` — the policy bound in the assessment key (F24).
    pub policy_version: ConfigVersion,
    /// `outcome` — how the request resolved.
    pub outcome: JudgmentOutcome,
}

/// A completed judgment set together with its `judgments` rows — the unit an
/// `effect_result` receipt carries (Appendix B `judgment_sets` +
/// `judgments`).
#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentRecord {
    /// The set row.
    pub set: JudgmentSet,
    /// Its per-question answers (present when `set.outcome` is `answered`).
    pub judgments: Vec<Judgment>,
}

/// F12 — the typed bundle of the seven evaluation judgments a launch set
/// answered; the F13 function reads these, not the raw rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    /// `done_when_verifiable` — calibrated P(the doneWhen is verifiable).
    pub done_when_verifiable: Probability,
    /// `weakest_sufficient_tier` — Jev's tier choice before adjustments.
    pub weakest_sufficient_tier: Tier,
    /// `changes_files`.
    pub changes_files: ChangesFiles,
    /// `security_boundary`.
    pub security_boundary: Probability,
    /// `needs_external`.
    pub needs_external: Probability,
    /// `long_running`.
    pub long_running: Probability,
    /// `related_tab` — `None` when the caller had no open governor tabs and
    /// the question was not asked (F12).
    pub related_tab: Option<TabChoice>,
}

/// F13 step 5 — the exploration record inside `decision_json`
/// (`$.exploration.assigned`/`$.exploration.executed`, Appendix B `outcomes`
/// view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exploration {
    /// Whether exploration was assigned to this Launch.
    pub assigned: bool,
    /// Whether it actually executed (the start tier was lowered one tier).
    pub executed: bool,
}

/// F13 — one ordered candidate: the operating point, its provider and the
/// exact arguments persisted with the decision (F15 replays them verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The operating point to try.
    pub operating_point: OperatingPointId,
    /// The provider whose cooldown gates it (F13 step 6 / F15 recheck).
    pub provider: Provider,
    /// Its tier — at or above `Decision::start_tier` (F13 step 6).
    pub tier: Tier,
    /// The harness kind `agent.start {kind, args}` launches (F15); persisted
    /// with the decision so a later catalog edit cannot rename it (F13/F27).
    pub harness: AgentKind,
    /// The exact `agent.start` arguments (F13/F15).
    pub args: Vec<String>,
}

/// F13 — the routing decision, persisted immutably with the Launch before any
/// topology effect: every floor, the requested tier, the exploration
/// assignment, the ordered candidates with their exact arguments, and the
/// config version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Jev's `weakest_sufficient_tier` answer before adjustments.
    pub judged_tier: Tier,
    /// The caller's requested tier — the uplift input (F13 step 3).
    pub requested_tier: Option<Tier>,
    /// The step-2 cap applied to a no-change, no-boundary Task.
    pub policy_cap: Option<Tier>,
    /// The step-2 floor a security boundary or broad change applied.
    pub policy_floor: Option<Tier>,
    /// The step-3 caller uplift applied — at most one tier above the floor,
    /// never lower (H#49).
    pub caller_uplift: Option<Tier>,
    /// The step-4 recovery minimum applied — at least one tier above the
    /// predecessor's start, with the predecessor's provider excluded (F21).
    pub recovery_minimum: Option<Tier>,
    /// The step-5 exploration assignment (`$.exploration` in decision_json).
    pub exploration: Exploration,
    /// The start tier after every adjustment.
    pub start_tier: Tier,
    /// The ordered candidates with their exact arguments (step 6).
    pub candidates: Vec<Candidate>,
    /// The config version this decision was made under (F13/F27).
    pub config_version: ConfigVersion,
}

/// F14 — where the new pane goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementPlan {
    /// Right-split into the tab Jev picked — only while it holds fewer than
    /// `TAB_PANE_MAX` panes — without focus (H#53).
    ExistingTab {
        /// The caller's open governor tab to join.
        tab: TabId,
    },
    /// A new tab; its initial pane is used for the child, not orphaned
    /// (H#102).
    NewTab,
}

/// The position of `tier` in the policy order; a name outside
/// `policy.tiers` has no defined order (`Tier` is deliberately not `Ord`).
fn tier_index(tiers: &[Tier], tier: &Tier) -> Option<usize> {
    tiers.iter().position(|candidate| candidate == tier)
}

/// A configured policy tier (`no_change_cap`, `security_floor`,
/// `broad_change_floor`) resolved to its position. A policy tier that is not
/// in `tiers` cannot be ordered — it is skipped here and left to F27 config
/// validation, which owns rejecting it.
fn policy_tier<'t>(tiers: &'t [Tier], configured: Option<&'t Tier>) -> Option<(&'t Tier, usize)> {
    let tier = configured?;
    tier_index(tiers, tier).map(|index| (tier, index))
}

/// A probability is contract-valid only finite and inside `[0, 1]`
/// (`jev_noul_answer_shape`); anything else is a malformed answer.
fn valid_probability(probability: Probability) -> bool {
    probability.0.is_finite() && (0.0..=1.0).contains(&probability.0)
}

/// The noul verdict: `P(yes)` at or above the bound resolves "yes". The
/// launch nouls carry no policy threshold — `threshold` is `None` and the
/// verdict bound is 0.5 (the calibrated majority; the spec names a threshold
/// only for `provider_limited`, F21).
pub(crate) fn noul_yes(probability: Probability, threshold: Option<f64>) -> bool {
    probability.0 >= threshold.unwrap_or(0.5)
}

/// F13 step 5 — the exploration lottery: `sha256(caller ‖ idempotencyKey)`,
/// with `caller` the durable caller key, read as a uniform fraction below
/// the policy rate.
fn exploration_assigned(caller: &CallerKey, key: &IdempotencyKey, rate: f64) -> bool {
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
    fraction < rate
}

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

/// F14 — where the new pane goes: the tab Jev picked while it still holds
/// fewer than `TAB_PANE_MAX` panes (the right split, no focus — H#53), else
/// a new tab. `open_tabs` is the caller's current open governor tabs with
/// their pane counts; a picked tab that has since closed or filled plans a
/// new tab, whose initial pane is used, never split (H#102).
#[must_use]
pub fn placement_plan(
    related_tab: Option<&TabChoice>,
    open_tabs: &[(TabId, usize)],
) -> PlacementPlan {
    match related_tab {
        Some(TabChoice::Tab(tab)) => {
            let fits = open_tabs
                .iter()
                .any(|(id, panes)| id == tab && *panes < TAB_PANE_MAX);
            if fits {
                PlacementPlan::ExistingTab { tab: tab.clone() }
            } else {
                PlacementPlan::NewTab
            }
        }
        Some(TabChoice::New) | None => PlacementPlan::NewTab,
    }
}

#[cfg(test)]
mod tests {
    mod builders;
    mod f12;
    mod f13;
    mod f13_eval;
    mod f13_explore;
    mod f14;

    use alloc::vec::Vec;

    use super::{
        Candidate, ChangesFiles, JEV_REQUEST_MAX_BYTES, JudgmentOutcome, JudgmentPurpose, Question,
        TAB_PANE_MAX, TRANSCRIPT_WINDOW_MAX_BYTES,
    };
    use crate::config::{OperatingPointId, Provider, Tier};
    use crate::identity::AgentKind;

    #[test]
    fn n5_f14_routing_bound_values() {
        assert_eq!(
            JEV_REQUEST_MAX_BYTES, 98_304,
            "Jev request bound is 96 KiB (N5)"
        );
        assert_eq!(
            TRANSCRIPT_WINDOW_MAX_BYTES, 32_768,
            "transcript window is 32 KiB (N5)"
        );
        assert_eq!(TAB_PANE_MAX, 4, "Jev's tab is used under four panes (F14)");
    }

    #[test]
    fn f15_candidate_carries_harness_and_args() {
        let candidate = Candidate {
            operating_point: OperatingPointId("op-1".into()),
            provider: Provider("prov-1".into()),
            tier: Tier("t0".into()),
            harness: AgentKind("kind-1".into()),
            args: Vec::from(["--flag".into()]),
        };
        // `agent.start {kind, args}` replays the persisted candidate verbatim
        // (F15) — a catalog edit must not be able to change either half.
        assert_eq!(
            candidate.harness,
            AgentKind("kind-1".into()),
            "candidate must carry the persisted harness kind (F15)"
        );
        assert_eq!(candidate.args.len(), 1, "candidate carries the exact args");
        assert!(
            candidate.args.iter().any(|arg| arg.as_str() == "--flag"),
            "start args replayed verbatim (F15)"
        );
    }

    #[test]
    fn f12_f23_f24_question_spellings() {
        let cases = [
            (Question::DoneWhenVerifiable, "done_when_verifiable"),
            (Question::WeakestSufficientTier, "weakest_sufficient_tier"),
            (Question::ChangesFiles, "changes_files"),
            (Question::SecurityBoundary, "security_boundary"),
            (Question::NeedsExternal, "needs_external"),
            (Question::LongRunning, "long_running"),
            (Question::RelatedTab, "related_tab"),
            (Question::BlockedOnInput, "blocked_on_input"),
            (Question::NoRecentProgress, "no_recent_progress"),
            (Question::OutsideScope, "outside_scope"),
            (Question::ProviderLimited, "provider_limited"),
            (Question::HandoffMeetsItem { item: 2 }, "handoff_meets_item"),
        ];
        for (question, name) in cases {
            assert_eq!(
                question.as_str(),
                name,
                "question spelling must match the spec"
            );
        }
    }

    #[test]
    fn f12_changes_files_spellings() {
        let cases = [
            (ChangesFiles::None, "none"),
            (ChangesFiles::Few, "few"),
            (ChangesFiles::Broad, "broad"),
        ];
        for (value, name) in cases {
            assert_eq!(
                value.as_str(),
                name,
                "changes_files spelling must match F12"
            );
        }
    }

    #[test]
    fn appendix_b_judgment_purpose_spellings() {
        let cases = [
            (JudgmentPurpose::Launch, "launch"),
            (JudgmentPurpose::Review, "review"),
            (JudgmentPurpose::Acceptance, "acceptance"),
            (JudgmentPurpose::ProviderLimit, "provider_limit"),
        ];
        for (purpose, name) in cases {
            assert_eq!(
                purpose.as_str(),
                name,
                "judgment purpose spelling must match the DDL"
            );
        }
    }

    #[test]
    fn appendix_b_judgment_outcome_spellings() {
        let cases = [
            (JudgmentOutcome::Answered, "answered"),
            (JudgmentOutcome::TransportFailed, "transport_failed"),
            (JudgmentOutcome::AuthFailed, "auth_failed"),
            (JudgmentOutcome::InvalidResponse, "invalid_response"),
            (JudgmentOutcome::TooLarge, "too_large"),
            (JudgmentOutcome::Stale, "stale"),
        ];
        for (outcome, name) in cases {
            assert_eq!(
                outcome.as_str(),
                name,
                "judgment outcome spelling must match the DDL"
            );
        }
    }
}
