//! F22 — the launch lane of the total transition function: `starting`.

use alloc::vec::Vec;

use crate::config::{OperatingPointId, Provider, Tier};
use crate::identity::Observation;
use crate::lifecycle::{
    EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectState, EffectTarget, Event,
    Settlement, State, UnresolvedReason, transition,
};
use crate::routing::PlacementPlan;

use super::builders::{
    NOW, decision, effect_keys, effect_writes, identity, journal_effect_at, run_in, run_result,
    settlement_of, stamped, test_policy, transact, updated_records, updated_run,
};
#[test]
pub(super) fn f22_starting_start_acknowledged_goes_prompting() {
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
pub(super) fn f22_starting_pre_interactive_failure_tries_next_candidate() {
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
pub(super) fn f22_starting_failure_with_no_candidates_stays() {
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
pub(super) fn f22_starting_unconfirmed_and_failed_stay_starting() {
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
pub(super) fn f22_starting_absent_is_launch_failed() {
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
pub(super) fn f22_starting_ack_uses_the_matched_candidates_index() {
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

// ---- F8 — the candidate index comes from the acknowledged key's
// `start:<index>` suffix: journal position is dispatch order, not
// candidate order, and an unparsable suffix selects no candidate. ----

#[test]
fn f22_starting_ack_parses_the_key_not_the_journal_position() {
    let run = run_in(State::Starting);
    // journal order is dispatch order — `start:1` committed first.
    let journal = Vec::from([
        journal_effect_at(
            &run,
            "start:1",
            EffectKind::AgentStart,
            EffectState::Dispatching,
            Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        ),
        journal_effect_at(
            &run,
            "start:0",
            EffectKind::AgentStart,
            EffectState::Failed,
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
        "the `start:1` key selects candidate 1 — not journal position 0"
    );
    assert_eq!(record.provider, Some(Provider("prov-1".into())));
    assert_eq!(record.tier_start, Some(Tier("t1".into())));
}

#[test]
fn f22_starting_ack_with_unparsable_key_selects_no_candidate() {
    let mut run = run_in(State::Starting);
    run.operating_point = None;
    run.provider = None;
    run.tier_start = None;
    // the journaled row's key carries no numeric suffix — position is not
    // a candidate index, so no candidate is selected.
    let journal = Vec::from([journal_effect_at(
        &run,
        "start:late",
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
                "start:late",
                EffectKind::AgentStart,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::AgentStarted {
                    identity: identity(),
                }),
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
        record.operating_point, None,
        "an unparsable `start:` suffix selects no candidate"
    );
}
