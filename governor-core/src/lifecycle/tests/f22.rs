//! F22 — the total transition function: the rules table, versioned
//! stamps, restart, and the journal write behind `effect_result`.

use alloc::vec::Vec;

use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, Digest, NativeSession, Observation, PaneId, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectState, Event,
    JudgmentVerdict, PromptCertainty, Settlement, State, StateChange, TRANSITION_RULES,
    UnresolvedReason, transition,
};
use crate::routing::{JudgmentOutcome, Question};

use super::builders::{
    EMPTY_READ, NOW, decision, effect_writes, event_kinds, is_quiet, journal_effect, noul,
    obs_unique, review_record, run_in, run_result, settlement_of, stamped, test_policy, transact,
    updated_records, updated_run,
};
#[test]
fn f22_total_transition_table() {
    // every state × every event kind returns a Transition — total, never a panic.
    let states = [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ];
    for state in states {
        let run = run_in(state);
        let events = Vec::from([
            obs_unique(Some(ChildStatus::Working)),
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
            Event::Obs {
                observation: Observation::Invalid,
                handoff_reading: None,
            },
            Event::Handoff {
                digest: Digest([3; 32]),
            },
            Event::Judgment(JudgmentVerdict::Accept),
            Event::Deadline(DeadlineKind::MaxAge),
            Event::Deadline(DeadlineKind::Idle),
            Event::Deadline(DeadlineKind::Repair),
            Event::Deadline(DeadlineKind::Judgment),
            Event::Cancel { close_pane: false },
            Event::Cancel { close_pane: true },
            Event::ProviderLimited,
            run_result(
                &run,
                "x",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
            Event::Restart,
        ]);
        for event in events {
            let _transition = transition(
                &run,
                &stamped(&run, event),
                NOW,
                &test_policy(),
                (Some(&decision(1)), &[], &[]),
                "/fp",
            );
        }
    }
}

#[test]
fn f22_rule_list_is_exposed_as_data() {
    assert!(
        TRANSITION_RULES.len() >= 30,
        "the Appendix C rule list renders the whole table"
    );
    assert!(
        TRANSITION_RULES
            .iter()
            .any(|(s, e, _)| *s == "settled" && *e == "cancel(closePane)"),
        "the settled-state close rule is listed"
    );
    assert!(
        TRANSITION_RULES
            .iter()
            .any(|(s, e, _)| *s == "*" && *e == "obs(invalid)"),
        "the all-state invalid rule is listed"
    );
}

#[test]
fn f22_obs_invalid_changes_nothing_in_every_state() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ] {
        let run = run_in(state);
        let t = transact(
            &run,
            &stamped(
                &run,
                Event::Obs {
                    observation: Observation::Invalid,
                    handoff_reading: None,
                },
            ),
        );
        assert!(is_quiet(&t), "obs(invalid) changes nothing");
    }
}

#[test]
fn f22_max_age_settles_any_unsettled_run() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
    ] {
        let run = run_in(state);
        let t = transition(
            &run,
            &stamped(&run, Event::Deadline(DeadlineKind::MaxAge)),
            Timestamp(1_000),
            &test_policy(),
            EMPTY_READ,
            "/fp",
        );
        assert_eq!(
            settlement_of(updated_run(&t)),
            Some(Settlement::Unresolved {
                reason: UnresolvedReason::MaxAge
            }),
            "max_age settles every unsettled Run"
        );
    }
    // before the deadline it is a no-op (the scheduler fired early).
    let run = run_in(State::Active);
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::MaxAge)),
        Timestamp(999),
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t), "a premature deadline event applies nothing");
}

#[test]
fn f22_restart_converts_dispatching_to_unconfirmed() {
    let run = run_in(State::Active);
    let journal = Vec::from([
        journal_effect(
            &run,
            "prompt:task",
            EffectKind::Prompt,
            EffectState::Acknowledged,
        ),
        journal_effect(
            &run,
            "nudge:0",
            EffectKind::Prompt,
            EffectState::Dispatching,
        ),
        journal_effect(
            &run,
            "review:0",
            EffectKind::JevEvaluate,
            EffectState::Dispatching,
        ),
        journal_effect(&run, "close", EffectKind::Close, EffectState::Planned),
    ]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([
            ("run:r-1:nudge:0", EffectState::Unconfirmed),
            ("run:r-1:review:0", EffectState::Unconfirmed),
        ]),
        "dispatching effects become unconfirmed — never re-dispatched"
    );
    // planned effects keep dispatching later; acknowledged rows are durable.
}

#[test]
fn f22_restart_never_changes_deadlines() {
    let mut run = run_in(State::Judging);
    run.judgment_deadline = Some(Timestamp(800));
    run.repair_deadline = Some(Timestamp(700));
    run.idle_deadline = Some(Timestamp(600));
    let journal = Vec::from([journal_effect(
        &run,
        "accept:0:1",
        EffectKind::JevEvaluate,
        EffectState::Dispatching,
    )]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        updated_records(&t).is_empty(),
        "restart writes no run row outside the prompting rule"
    );
}

#[test]
fn f22_restart_re_derives_prompting_from_the_journal() {
    let run = run_in(State::Prompting);
    let journal = Vec::from([journal_effect(
        &run,
        "prompt:task",
        EffectKind::Prompt,
        EffectState::Dispatching,
    )]);
    let t = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.prompt_certainty,
        Some(PromptCertainty::Unconfirmed),
        "a dispatching task prompt that went unconfirmed promotes prompting → active (F8/F16)"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::PromptUnconfirmed])
    );
    // if the prompt was only `planned`, nothing promotes.
    let planned_journal = Vec::from([journal_effect(
        &run,
        "prompt:task",
        EffectKind::Prompt,
        EffectState::Planned,
    )]);
    let t_planned = transition(
        &run,
        &stamped(&run, Event::Restart),
        NOW,
        &test_policy(),
        (None, &planned_journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t_planned),
        "a still-planned prompt keeps prompting"
    );
}

#[test]
fn f22_unique_observation_refreshes_the_locator() {
    let run = run_in(State::Judging);
    let session = Some(NativeSession("sess-2".into()));
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Unique {
                    status: Some(ChildStatus::Done),
                    pane: PaneId("w1:p2".into()),
                    native_session: session,
                },
                handoff_reading: None,
            },
        ),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let record = updated_run(&t);
    let identity = record.identity.clone().expect("identity kept");
    assert_eq!(
        identity.pane_id,
        PaneId("w1:p2".into()),
        "a move is followed"
    );
    assert_eq!(
        identity.native_session,
        Some(NativeSession("sess-2".into())),
        "the reported session is captured"
    );
    assert_eq!(record.child_status, Some(ChildStatus::Done));
    assert_eq!(
        record.state,
        State::Judging,
        "status alone never leaves judging"
    );
}

#[test]
fn f22_failed_results_journal_their_certainty() {
    let run = run_in(State::Active);
    for (outcome, certainty) in [
        (
            EffectOutcome::PreInteractiveFailed,
            Some(EffectCertainty::Absent),
        ),
        (
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
            Some(EffectCertainty::Unknown),
        ),
        (EffectOutcome::Unconfirmed, None),
        (EffectOutcome::Acknowledged, None),
    ] {
        let t = transact(
            &run,
            &stamped(
                &run,
                run_result(&run, "nudge:0", EffectKind::Prompt, outcome, None),
            ),
        );
        let write = t
            .state_changes
            .iter()
            .find_map(|c| match c {
                StateChange::WriteEffect(w) => Some(w),
                StateChange::BindCaller(_)
                | StateChange::RecordLaunch(_)
                | StateChange::ReserveRun(_)
                | StateChange::UpdateRun(_)
                | StateChange::ChangeOwner(_)
                | StateChange::RecordFollowUp(_)
                | StateChange::ExpireFollowUps { .. }
                | StateChange::RecordRecovery(_)
                | StateChange::SetCooldown(_)
                | StateChange::FreezeHandoff(_)
                | StateChange::AckEvent(_) => None,
            })
            .expect("one journal write per result");
        assert_eq!(write.certainty, certainty);
    }
}

#[test]
fn f22_unanswered_set_applies_nothing() {
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
    record.set.outcome = JudgmentOutcome::TransportFailed;
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(record)),
            ),
        ),
    );
    assert!(
        t.events.is_empty() && t.effects.is_empty() && updated_records(&t).is_empty(),
        "only an answered set maps answers (H#83 — incomplete evidence proves nothing)"
    );
}
