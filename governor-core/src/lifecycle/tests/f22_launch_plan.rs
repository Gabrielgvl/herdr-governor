//! F13/F14 — the launch plan write: `reserved` + "topology effect planned"
//! → `starting`, and the full launch pipeline it opens.

use alloc::vec::Vec;

use crate::identity::{PaneId, TabId};
use crate::lifecycle::{
    EffectKind, EffectOutcome, EffectReceipt, EffectState, EffectTarget, State, launch_plan,
    transition,
};
use crate::routing::PlacementPlan;

use super::builders::{
    NOW, decision, effect_keys, effect_writes, identity, is_quiet, run_in, run_result, stamped,
    test_policy, updated_run,
};

fn caller_pane() -> PaneId {
    PaneId("w6:p1".into())
}

/// The named proof for `("reserved", "topology effect planned")`: the plan
/// write journals the placement's one topology effect and moves the Run to
/// `starting` in the same transition — every other Run field is left as the
/// F13 reservation wrote it.
#[test]
pub(super) fn f22_topology_effect_planned_moves_reserved_to_starting() {
    let run = run_in(State::Reserved);
    for plan in [
        PlacementPlan::NewTab,
        PlacementPlan::ExistingTab {
            tab: TabId("t3".into()),
        },
    ] {
        let t = launch_plan(&run, &decision(2), &plan, &caller_pane());
        let record = updated_run(&t);
        assert_eq!(record.state, State::Starting);
        assert_eq!(record.version, run.version.saturating_add(1));
        let mut expected = run.clone();
        expected.state = State::Starting;
        expected.version = run.version.saturating_add(1);
        assert_eq!(
            record, &expected,
            "the plan write moves the state and bumps the version only"
        );
        assert_eq!(t.effects.len(), 1, "one topology effect is journaled");
        assert_eq!(t.effects[0].state, EffectState::Planned);
        assert!(t.events.is_empty());
    }
}

#[test]
fn f22_launch_plan_new_tab_journals_tab_create() {
    let run = run_in(State::Reserved);
    let t = launch_plan(&run, &decision(2), &PlacementPlan::NewTab, &caller_pane());
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:tab"]));
    let effect = &t.effects[0];
    assert_eq!(effect.kind, EffectKind::TabCreate);
    assert_eq!(
        effect.target,
        Some(EffectTarget::CallerContext(caller_pane())),
        "the caller's pane locates the workspace the tab opens in (F14)"
    );
}

#[test]
fn f22_launch_plan_existing_tab_journals_pane_split() {
    let run = run_in(State::Reserved);
    let t = launch_plan(
        &run,
        &decision(2),
        &PlacementPlan::ExistingTab {
            tab: TabId("t3".into()),
        },
        &caller_pane(),
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:split"]));
    let effect = &t.effects[0];
    assert_eq!(effect.kind, EffectKind::PaneSplit);
    assert_eq!(
        effect.target,
        Some(EffectTarget::ExistingTab(TabId("t3".into()))),
        "the split names the tab the placement picked (F14)"
    );
}

#[test]
fn f22_launch_plan_plans_nothing_outside_reserved() {
    for state in [
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ] {
        let run = run_in(state);
        let t = launch_plan(&run, &decision(2), &PlacementPlan::NewTab, &caller_pane());
        assert!(is_quiet(&t), "the plan write only ever fires on reserved");
    }
}

#[test]
fn f22_launch_plan_without_candidates_plans_nothing() {
    let run = run_in(State::Reserved);
    let t = launch_plan(&run, &decision(0), &PlacementPlan::NewTab, &caller_pane());
    assert!(
        is_quiet(&t),
        "F13 abstains rather than reserve without a candidate — no topology effect is journaled for a launch that cannot start"
    );
}

/// The launch pipeline end to end: `reserved` → plan write → `starting` →
/// topology ack plans `start:0` → `agent_start` ack goes `prompting` with
/// the task prompt planned (Appendix C rows for `reserved`/`starting`).
#[test]
fn f22_launch_pipeline_reserved_to_prompting() {
    let run = run_in(State::Reserved);
    let decision = decision(2);

    // the plan write: journal tab_create, move to starting
    let plan_write = launch_plan(&run, &decision, &PlacementPlan::NewTab, &caller_pane());
    let starting = updated_run(&plan_write).clone();
    assert_eq!(starting.state, State::Starting);
    let mut journal = plan_write.effects.clone();

    // the topology ack lands in starting and plans the first start
    let topology = transition(
        &starting,
        &stamped(
            &starting,
            run_result(
                &starting,
                "tab",
                EffectKind::TabCreate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::TabCreated {
                    tab: TabId("t9".into()),
                    pane: PaneId("t9:p1".into()),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision), &journal, &[]),
        "/fp",
    );
    assert_eq!(
        effect_writes(&topology),
        Vec::from([("run:r-1:tab", EffectState::Acknowledged)]),
    );
    assert_eq!(effect_keys(&topology), Vec::from(["run:r-1:start:0"]));
    assert_eq!(
        topology.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        "a new tab's initial pane hosts the child — never split (H#102)"
    );

    // the start ack captures the identity and goes prompting
    journal.extend(topology.effects.iter().cloned());
    let started = transition(
        &starting,
        &stamped(
            &starting,
            run_result(
                &starting,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::AgentStarted {
                    identity: identity(),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision), &journal, &[]),
        "/fp",
    );
    let record = updated_run(&started);
    assert_eq!(record.state, State::Prompting);
    assert_eq!(record.identity, Some(identity()));
    assert_eq!(effect_keys(&started), Vec::from(["run:r-1:prompt:task"]));
}
