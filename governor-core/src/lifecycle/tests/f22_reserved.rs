//! F22 — the launch lane of the total transition function: `reserved`.

use alloc::vec::Vec;

use crate::identity::{ChildStatus, Digest, Observation, PaneId, TabId};
use crate::lifecycle::{
    DeadlineKind, EffectKind, EffectOutcome, EffectReceipt, EffectState, EffectTarget, Event,
    JudgmentVerdict, Settlement, State, UnresolvedReason, transition,
};
use crate::routing::PlacementPlan;

use super::builders::{
    NOW, decision, effect_keys, is_quiet, journal_effect_at, obs_unique, run_in, run_result,
    settlement_of, stamped, test_policy, transact, updated_run,
};
#[test]
pub(super) fn f22_reserved_absent_is_launch_not_started() {
    let run = run_in(State::Reserved);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchNotStarted
        }),
        "the pane that would host the child vanished before it started"
    );
}

#[test]
fn f22_reserved_ignores_the_rest() {
    let run = run_in(State::Reserved);
    for event in [
        obs_unique(Some(ChildStatus::Working)),
        Event::Handoff {
            digest: Digest([1; 32]),
        },
        Event::Judgment(JudgmentVerdict::Accept),
        Event::Deadline(DeadlineKind::Idle),
    ] {
        let t = transact(&run, &stamped(&run, event));
        assert!(is_quiet(&t), "reserved has no other answers");
    }
}

#[test]
pub(super) fn f22_starting_topology_acknowledgement_plans_first_start() {
    let run = run_in(State::Reserved);
    // a new tab's initial pane hosts the child — no split is planned (H#102).
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "tab:create",
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
        (Some(&decision(2)), &[], &[]),
        "/fp",
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:start:0"]));
    assert_eq!(t.effects[0].kind, EffectKind::AgentStart);
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab))
    );

    // a split into a picked tab starts into the resulting pane plan.
    let journal = Vec::from([journal_effect_at(
        &run,
        "split",
        EffectKind::PaneSplit,
        EffectState::Dispatching,
        Some(EffectTarget::ExistingTab(TabId("t3".into()))),
    )]);
    let t_split = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "split",
                EffectKind::PaneSplit,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::PaneCreated {
                    pane: PaneId("t3:p4".into()),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    assert_eq!(
        t_split.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::ExistingTab {
            tab: TabId("t3".into())
        })),
        "the start plans into the tab the split ran in"
    );
}
