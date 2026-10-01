//! F13/F14 — the launch plan write: the transaction that takes a `reserved`
//! Run, journals the one topology effect its placement plan needs and moves
//! the Run to `starting` (Appendix C `reserved` / "topology effect
//! planned"). F13 persists the routing decision before any topology effect,
//! so this is the first Herdr mutation a launch commits — its result events
//! land in `starting`, where the launch pipeline consumes them.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::Policy;
use crate::identity::{PaneId, RunId, Timestamp, mint_agent_name};
use crate::lifecycle::{
    EffectKind, EffectTarget, Run, State, Transition, edited, effect_key, nothing, op_digest,
    planned_effect, write_run,
};
use crate::routing::{Decision, PlacementPlan};
use crate::task::Launch;

/// F13/F14 — the launch plan transaction (Appendix C `reserved` | "topology
/// effect planned"): journal exactly the one topology effect `plan` names
/// and write the Run `starting`, in one transaction.
///
/// A `NewTab` plan journals `tab_create` with a `CallerContext` target —
/// the caller's pane locates the workspace the tab opens in (F14); the new
/// tab's initial pane hosts the child and is never split (H#102). An
/// `ExistingTab` plan journals `pane_split` into the tab the placement
/// picked (right split, no focus — H#53; `placement_plan` already applied
/// the four-pane cap).
///
/// A Run that is not `reserved`, or a `decision` with no candidates,
/// produces nothing: F13 abstains rather than reserve without a candidate,
/// so either input means no launch is being planned — no Herdr mutation is
/// journaled.
#[must_use]
pub fn launch_plan(
    run: &Run,
    decision: &Decision,
    plan: &PlacementPlan,
    caller_pane: &PaneId,
) -> Transition {
    if run.state != State::Reserved || decision.candidates.is_empty() {
        return nothing();
    }
    let (kind, suffix, target) = match plan {
        PlacementPlan::NewTab => (
            EffectKind::TabCreate,
            "tab",
            EffectTarget::CallerContext(caller_pane.clone()),
        ),
        PlacementPlan::ExistingTab { tab } => (
            EffectKind::PaneSplit,
            "split",
            EffectTarget::ExistingTab(tab.clone()),
        ),
    };
    let record = edited(run, |next| {
        next.state = State::Starting;
    });
    // Topology kinds render deterministically at plan time — the target is
    // the whole operation descriptor, so the digest takes no params (OQ-15).
    let digest = op_digest(kind, Some(&target), &[]);
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            kind,
            effect_key(run, suffix),
            Some(target),
            Some(digest),
        )]),
    }
}

/// F13/Appendix B "Route" — the reserved Run the Route transaction writes:
/// `reserved` with `max_age_deadline` fixed at reserve (F22), the minted
/// `gov-<runId[0..8]>` name (F2/H#52), the Launch's caller as first owner and
/// the F23 `base_commit` baseline the daemon pinned before the first
/// topology effect. Supervision obligations apply from this moment
/// (H#36/H#44).
#[must_use]
pub fn reserved_run(
    launch: &Launch,
    run_id: RunId,
    cwd: String,
    base_commit: Option<String>,
    now: Timestamp,
    policy: &Policy,
) -> Run {
    let child_name = mint_agent_name(&run_id).0;
    Run {
        id: run_id,
        launch: launch.id.clone(),
        owner: launch.caller.clone(),
        owner_generation: 0,
        version: 0,
        state: State::Reserved,
        prompt_certainty: None,
        child_name,
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd,
        base_commit,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: now.after(policy.max_age),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}
