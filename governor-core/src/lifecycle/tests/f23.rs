//! F23 — the supervision mapping: periodic reviews and the answered
//! judgment set's questions.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::delivery::MailboxEventKind;
use crate::identity::Timestamp;
use crate::lifecycle::{
    DeadlineKind, EffectKind, EffectOutcome, EffectReceipt, EffectResolution, EffectState,
    EffectTarget, EffectWrite, Event, Settlement, State, StateChange, periodic_review,
};
use crate::routing::{JudgmentOutcome, Probability, Question, noul_yes};

use super::builders::{
    effect_keys, effect_writes, event_dedups, event_kinds, identity, journal_effect, noul,
    noul_with_threshold, review_record, run_in, run_result, settlement_of, stamped, transact,
    triple, updated_records, updated_run,
};
#[test]
fn f23_blocked_on_input_produces_an_event() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
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
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::BlockedOnInput])
    );
    assert_eq!(event_dedups(&t), Vec::from(["run:r-1:blocked_on_input:0"]));
    assert!(t.effects.is_empty(), "blocked_on_input never nudges");
}

#[test]
fn f23_blocked_on_input_below_threshold_is_silent() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.2)]));
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
    assert!(t.events.is_empty());
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:review:0", EffectState::Acknowledged)]),
        "the answered set still journals"
    );
}

#[test]
fn f23_no_recent_progress_nudges_once_per_episode() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::NoRecentProgress, 0.9)]));
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
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:nudge:0"]));
    assert_eq!(t.effects[0].kind, EffectKind::Prompt);
    assert_eq!(t.effects[0].target, Some(EffectTarget::Child(identity())));
    assert_eq!(
        updated_run(&t).nudged_episode,
        Some(0),
        "the episode's one nudge is spent"
    );
    assert!(
        t.events.is_empty(),
        "the first stall nudges, it does not report"
    );

    // the same episode never nudges again — a repeated stall reports stalled.
    let mut run_stalled = run_in(State::Active);
    run_stalled.nudged_episode = Some(0);
    let stalled_record = review_record(
        &run_stalled,
        Vec::from([noul(Question::NoRecentProgress, 0.9)]),
    );
    let t_stalled = transact(
        &run_stalled,
        &stamped(
            &run_stalled,
            run_result(
                &run_stalled,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(stalled_record)),
            ),
        ),
    );
    assert!(
        t_stalled.effects.is_empty(),
        "no second nudge in one episode"
    );
    assert_eq!(
        event_kinds(&t_stalled),
        Vec::from([MailboxEventKind::Stalled])
    );
    assert_eq!(event_dedups(&t_stalled), Vec::from(["run:r-1:stalled:0"]));
}

#[test]
fn f23_provider_limited_above_threshold_settles() {
    let run = run_in(State::Active);
    let record = review_record(
        &run,
        Vec::from([
            noul(Question::BlockedOnInput, 0.9),
            noul(Question::ProviderLimited, 0.9),
        ]),
    );
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
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::ProviderLimited),
        "a cleared provider_limited settles via the F21 transaction"
    );
    assert!(
        t.state_changes
            .iter()
            .any(|c| matches!(c, StateChange::RecordRecovery(_))),
        "the recovery obligation rides the settle"
    );
    // the advisory answers of a terminal settle never emit.
    assert_eq!(
        event_kinds(&t),
        Vec::from([
            MailboxEventKind::CooldownHit,
            MailboxEventKind::RecoveryPending,
            MailboxEventKind::Settled,
        ])
    );
}

#[test]
fn f23_provider_limited_below_threshold_does_not_settle() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::ProviderLimited, 0.5)]));
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
    assert!(updated_records(&t).is_empty());
    assert!(t.events.is_empty());
}

#[test]
fn f23_outside_scope_produces_an_event() {
    let run = run_in(State::Active);
    let record = review_record(&run, Vec::from([noul(Question::OutsideScope, 0.9)]));
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
    assert_eq!(event_kinds(&t), Vec::from([MailboxEventKind::OutsideScope]));
}

#[test]
fn f23_stale_judgment_set_journals_stale_and_applies_nothing() {
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::BlockedOnInput, 0.9)]));
    let mut stale_versions = triple(&run);
    stale_versions.evidence_generation = stale_versions.evidence_generation.saturating_add(1);
    record.set.versions = Some(stale_versions);
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
    assert!(t.events.is_empty(), "stale answers apply nothing");
    assert!(t.effects.is_empty());
    assert!(
        updated_records(&t).is_empty(),
        "no run write — the version guard is the checkpoint"
    );
    // and the journaled set is marked stale.
    let receipt_is_stale = t.state_changes.iter().any(|c| match c {
        StateChange::WriteEffect(w) => matches!(
            w,
            EffectWrite::Result {
                resolution: EffectResolution::Acknowledged {
                    receipt: Some(EffectReceipt::Judgments(r)),
                },
                ..
            } if r.set.outcome == JudgmentOutcome::Stale
        ),
        StateChange::BindCaller(_)
        | StateChange::RecordLaunch(_)
        | StateChange::ReserveRun(_)
        | StateChange::UpdateRun(_)
        | StateChange::ChangeOwner(_)
        | StateChange::WriteFollowUp(_)
        | StateChange::ExpireFollowUps { .. }
        | StateChange::RecordRecovery(_)
        | StateChange::SetCooldown(_)
        | StateChange::FreezeHandoff(_)
        | StateChange::AckEvent(_) => false,
    });
    assert!(
        receipt_is_stale,
        "the set journals with outcome stale (F20)"
    );
}

#[test]
fn f23_periodic_reviews_pause_while_the_owner_is_absent() {
    let run = run_in(State::Active);
    assert!(
        periodic_review(&run, true, &[]).is_none(),
        "no review while the owner's session is absent"
    );
    let effect = periodic_review(&run, false, &[]).expect("a present owner gets reviews");
    assert_eq!(effect.kind, EffectKind::JevEvaluate);
    assert_eq!(effect.key.0, "run:r-1:review:0");
    // acceptance and deadlines never pause — they run through `transition`,
    // which takes no owner-presence input at all. A deadline fires regardless.
    let mut judging = run_in(State::Judging);
    judging.evidence_generation = 1;
    judging.judgment_deadline = Some(Timestamp(400));
    let t = transact(
        &judging,
        &stamped(&judging, Event::Deadline(DeadlineKind::Judgment)),
    );
    assert!(
        settlement_of(updated_run(&t)).is_some(),
        "deadlines are never paused"
    );
}

#[test]
fn f23_unchanged_evidence_is_never_re_asked() {
    let run = run_in(State::Active);
    // the ask for this generation is still in flight — no second one.
    for state in [EffectState::Planned, EffectState::Dispatching] {
        let journal = Vec::from([journal_effect(
            &run,
            "review:0",
            EffectKind::JevEvaluate,
            state,
        )]);
        assert!(
            periodic_review(&run, false, &journal).is_none(),
            "at most one ask in flight per key"
        );
    }
    // and a completed (answered) review suppresses the generation's ask.
    let mut answered = journal_effect(
        &run,
        "review:0",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    answered.receipt = Some(EffectReceipt::Judgments(review_record(&run, Vec::new())));
    let journal = Vec::from([answered]);
    assert!(
        periodic_review(&run, false, &journal).is_none(),
        "one completed review per evidence_generation"
    );
    // and never while the run is not being supervised.
    let run_judging = run_in(State::Judging);
    assert!(periodic_review(&run_judging, false, &[]).is_none());
}

#[test]
fn f23_noul_threshold_boundary() {
    let run = run_in(State::Active);
    // exactly at the policy threshold → cleared.
    let record = review_record(&run, Vec::from([noul(Question::ProviderLimited, 0.7)]));
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
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::ProviderLimited),
        "p == threshold clears"
    );
    // a judgment carrying its own recorded threshold applies that.
    let thresholded = review_record(
        &run,
        Vec::from([noul_with_threshold(
            Question::BlockedOnInput,
            0.6,
            Some(0.8),
        )]),
    );
    let t_thresholded = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(thresholded)),
            ),
        ),
    );
    assert!(
        t_thresholded.events.is_empty(),
        "0.6 < the recorded 0.8 threshold → not cleared"
    );
    // and a judgment without the "yes" probability never clears.
    let mut missing = noul(Question::BlockedOnInput, 0.9);
    missing.probabilities = BTreeMap::from([(String::from("no"), Probability(0.1))]);
    let missing_record = review_record(&run, Vec::from([missing]));
    let t_missing = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(missing_record)),
            ),
        ),
    );
    assert!(t_missing.events.is_empty(), "no P(yes) → never cleared");
}

#[test]
fn f23_noul_threshold_matches_routing() {
    // supervision reads every noul through `routing::noul_yes` — the verdict
    // observable through the F23 mapping must equal noul_yes on the same
    // inputs, at the boundary and on both sides of it.
    let run = run_in(State::Active);
    let cleared = |p: f64, threshold: Option<f64>| {
        let record = review_record(
            &run,
            Vec::from([noul_with_threshold(Question::BlockedOnInput, p, threshold)]),
        );
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
        event_kinds(&t) == [MailboxEventKind::BlockedOnInput]
    };
    for (p, threshold) in [
        (0.8, Some(0.8)),                // exactly at the recorded bound
        (0.8f64.next_down(), Some(0.8)), // just below it
        (0.5, None),                     // the default majority bound
        (0.5f64.next_down(), None),      // just below it
    ] {
        assert_eq!(
            cleared(p, threshold),
            noul_yes(Probability(p), threshold),
            "supervision verdict must equal routing::noul_yes for p={p} threshold={threshold:?}"
        );
    }
    // the agreement has a direction — the bound is inclusive, not flipped.
    assert!(cleared(0.8, Some(0.8)), "p == threshold clears");
    assert!(!cleared(0.8f64.next_down(), Some(0.8)), "below does not");
}

#[test]
fn f23_launch_bound_set_is_not_stale_but_still_maps() {
    // a set bound to the Launch (versions=None) is never stale for a Run —
    // its F23 answers still map (the supervisor read it for this Run).
    let run = run_in(State::Active);
    let mut record = review_record(&run, Vec::from([noul(Question::OutsideScope, 0.9)]));
    record.set.versions = None;
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
    assert_eq!(event_kinds(&t), Vec::from([MailboxEventKind::OutsideScope]));
}
