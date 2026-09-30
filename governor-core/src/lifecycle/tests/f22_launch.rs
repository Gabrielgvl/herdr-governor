//! F22 — the launch lane of the total transition function:
//! `reserved`, `starting` and `prompting`.

use alloc::vec::Vec;

use crate::config::{OperatingPointId, Provider, Tier};
use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, Digest, Observation, PaneId, TabId};
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectState,
    EffectTarget, Event, JudgmentVerdict, PromptCertainty, Settlement, State, UnresolvedReason,
    transition,
};
use crate::routing::PlacementPlan;

use super::builders::{
    NOW, decision, effect_keys, effect_writes, event_kinds, identity, is_quiet, journal_effect_at,
    obs_unique, run_in, run_result, settlement_of, stamped, test_policy, transact, updated_records,
    updated_run,
};
#[test]
fn f22_reserved_absent_is_launch_not_started() {
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
fn f22_starting_start_acknowledged_goes_prompting() {
    let run = run_in(State::Starting);
    let started_receipt = EffectReceipt::AgentStarted {
        identity: identity(),
    };
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(started_receipt),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Prompting);
    assert_eq!(record.identity, Some(identity()));
    assert_eq!(
        record.operating_point,
        Some(OperatingPointId("op-0".into())),
        "the started candidate's point is recorded"
    );
    assert_eq!(record.provider, Some(Provider("prov-0".into())));
    assert_eq!(record.tier_start, Some(Tier("t0".into())));
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:start:0", EffectState::Acknowledged)])
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:prompt:task"]));
    assert_eq!(t.effects[0].kind, EffectKind::Prompt);
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::Child(identity())),
        "the task prompt targets the captured identity"
    );
}

#[test]
fn f22_starting_topology_acknowledgement_plans_first_start() {
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

#[test]
fn f22_starting_pre_interactive_failure_tries_next_candidate() {
    let run = run_in(State::Starting);
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::PreInteractiveFailed,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(2)), &journal, &[]),
        "/fp",
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:start:1"]));
    assert_eq!(
        t.effects[0].target,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        "the next candidate tries the same pane (F15)"
    );
    // the failed start journals absent.
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:start:0", EffectState::Failed)])
    );
    assert!(
        updated_records(&t).is_empty(),
        "the Run stays starting for the retry"
    );
}

#[test]
fn f22_starting_failure_with_no_candidates_stays() {
    let run = run_in(State::Starting);
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:0",
        EffectKind::AgentStart,
        EffectState::Dispatching,
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
    )]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:0",
                EffectKind::AgentStart,
                EffectOutcome::PreInteractiveFailed,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(1)), &journal, &[]),
        "/fp",
    );
    assert!(
        t.effects.is_empty(),
        "no next candidate → nothing is planned; the Run waits on obs(absent) or max_age"
    );
    assert!(updated_records(&t).is_empty());
}

#[test]
fn f22_starting_unconfirmed_and_failed_stay_starting() {
    let run = run_in(State::Starting);
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent,
        },
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let journal = Vec::from([journal_effect_at(
            &run,
            "start:0",
            EffectKind::AgentStart,
            EffectState::Dispatching,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        )]);
        let t = transition(
            &run,
            &stamped(
                &run,
                run_result(&run, "start:0", EffectKind::AgentStart, outcome, None),
            ),
            NOW,
            &test_policy(),
            (Some(&decision(2)), &journal, &[]),
            "/fp",
        );
        assert!(t.effects.is_empty(), "no fallback without proof");
        assert!(updated_records(&t).is_empty(), "the Run stays starting");
        assert_eq!(effect_writes(&t).len(), 1, "the result still journals");
    }
}

#[test]
fn f22_starting_absent_is_launch_failed() {
    let run = run_in(State::Starting);
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
            reason: UnresolvedReason::LaunchFailed
        })
    );
}

#[test]
fn f22_prompting_acknowledged_goes_active() {
    let run = run_in(State::Prompting);
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(record.prompt_certainty, Some(PromptCertainty::Acknowledged));
}

#[test]
fn f22_prompting_unconfirmed_goes_active_unconfirmed() {
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::PreInteractiveFailed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let run = run_in(State::Prompting);
        let t = transact(
            &run,
            &stamped(
                &run,
                run_result(&run, "prompt:task", EffectKind::Prompt, outcome, None),
            ),
        );
        let record = updated_run(&t);
        assert_eq!(record.state, State::Active);
        assert_eq!(
            record.prompt_certainty,
            Some(PromptCertainty::Unconfirmed),
            "a possibly-consumed prompt records unconfirmed and never resubmits"
        );
        assert_eq!(
            event_kinds(&t),
            Vec::from([MailboxEventKind::PromptUnconfirmed])
        );
    }
}

#[test]
fn f22_prompting_absent_is_pane_lost() {
    let run = run_in(State::Prompting);
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
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::PaneLost));
}

#[test]
fn f22_prompting_ignores_non_task_prompt_results() {
    let run = run_in(State::Prompting);
    // a prompt effect that is not the task prompt (kind matches, key does
    // not) must not promote the Run — the `||` guard keeps it out.
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(
        updated_records(&t).is_empty(),
        "only the task prompt's resolution promotes"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)]),
        "the journal write still commits"
    );
    // and a non-prompt effect under the task key stays out too.
    let t_kind = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Close,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(updated_records(&t_kind).is_empty());
}

#[test]
fn f22_starting_ack_uses_the_matched_candidates_index() {
    let run = run_in(State::Starting);
    let journal = Vec::from([
        journal_effect_at(
            &run,
            "start:0",
            EffectKind::AgentStart,
            EffectState::Failed,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
        journal_effect_at(
            &run,
            "start:1",
            EffectKind::AgentStart,
            EffectState::Dispatching,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
    ]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "start:1",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::AgentStarted {
                    identity: identity(),
                }),
            ),
        ),
        NOW,
        &test_policy(),
        (Some(&decision(3)), &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(
        record.operating_point,
        Some(OperatingPointId("op-1".into())),
        "the run records the candidate that actually started"
    );
    // without a persisted decision the identity still lands, but the
    // candidate fields cannot be recovered — they stay unset.
    let mut run_nodecision = run_in(State::Starting);
    run_nodecision.operating_point = None;
    run_nodecision.provider = None;
    run_nodecision.tier_start = None;
    let t_nodecision = transition(
        &run_nodecision,
        &stamped(
            &run_nodecision,
            run_result(
                &run_nodecision,
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
        (None, &journal, &[]),
        "/fp",
    );
    let record_nodecision = updated_run(&t_nodecision);
    assert_eq!(record_nodecision.identity, Some(identity()));
    assert_eq!(
        record_nodecision.operating_point, None,
        "no decision → the started candidate cannot be recovered"
    );
}
