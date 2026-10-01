//! F13/F14 — the launch plan write: the transaction that takes a `reserved`
//! Run, journals the one topology effect its placement plan needs and moves
//! the Run to `starting` (Appendix C `reserved` / "topology effect
//! planned"). F13 persists the routing decision before any topology effect,
//! so this is the first Herdr mutation a launch commits — its result events
//! land in `starting`, where the launch pipeline consumes them.

use alloc::vec::Vec;

use crate::identity::PaneId;
use crate::lifecycle::{
    EffectKind, EffectTarget, Run, State, Transition, edited, effect_key, nothing, planned_effect,
    write_run,
};
use crate::routing::{Decision, PlacementPlan};

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
    Transition {
        state_changes: Vec::from([write_run(run, record)]),
        events: Vec::new(),
        effects: Vec::from([planned_effect(
            run,
            kind,
            effect_key(run, suffix),
            Some(target),
        )]),
    }
}
