//! F31 — the typed provider-limit ask: `run:<id>:limit:<record_id>` asks
//! once per record for the Run's lifetime in `active`/`judging`/`repair`,
//! re-asks failed attempts under `:<n>`, and yields to an in-flight
//! blocked-episode ask that already carries the record as evidence.

use alloc::vec::Vec;

use crate::lifecycle::{
    EffectKind, EffectReceipt, EffectState, LimitRecordKey, State, limit_observed,
};
use crate::routing::JudgmentOutcome;

use super::builders::{journal_effect, review_record, run_in};

fn record() -> LimitRecordKey {
    LimitRecordKey("src-a:1000".into())
}

/// The record's journaled ask marked answered — a `Judgments` receipt
/// with `answered` outcome is the only terminal suppressor (F23 family
/// rule).
fn answered_ask(run: &crate::lifecycle::Run, suffix: &str) -> crate::lifecycle::Effect {
    let mut ask = journal_effect(
        run,
        suffix,
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    );
    ask.receipt = Some(EffectReceipt::Judgments(review_record(run, Vec::new())));
    ask
}

#[test]
fn f31_limit_observed_once_per_record_in_supervised_states() {
    for state in [State::Active, State::Judging, State::Repair] {
        let run = run_in(state);
        let Some(ask) = limit_observed(&run, &record(), &[]) else {
            panic!("{state:?} plans the limit ask")
        };
        assert_eq!(ask.kind, EffectKind::JevEvaluate, "the ask is a Jev eval");
        assert_eq!(ask.state, EffectState::Planned, "the ask lands planned");
        assert_eq!(
            ask.key.0, "run:r-1:limit:src-a:1000",
            "the family carries the record id"
        );
        assert_eq!(
            limit_observed(&run, &record(), &Vec::from([ask])),
            None,
            "{state:?} never re-asks while the record's ask is in flight"
        );
        let journal = Vec::from([answered_ask(&run, "limit:src-a:1000")]);
        assert_eq!(
            limit_observed(&run, &record(), &journal),
            None,
            "{state:?} never re-asks a record already answered — once per Run lifetime"
        );
    }
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Settled,
    ] {
        assert_eq!(
            limit_observed(&run_in(state), &record(), &[]),
            None,
            "{state:?} never plans the limit ask"
        );
    }
    // A distinct record is a distinct family — asking one leaves the
    // other free.
    let run = run_in(State::Active);
    let journal = Vec::from([answered_ask(&run, "limit:src-a:1000")]);
    let Some(second) = limit_observed(&run, &LimitRecordKey("src-a:2000".into()), &journal) else {
        panic!("a distinct record asks fresh")
    };
    assert_eq!(second.key.0, "run:r-1:limit:src-a:2000");
}

#[test]
fn f31_limit_observed_reasks_failed_attempt_under_n_key() {
    let run = run_in(State::Active);
    let journal = Vec::from([journal_effect(
        &run,
        "limit:src-a:1000",
        EffectKind::JevEvaluate,
        EffectState::Failed,
    )]);
    let Some(second) = limit_observed(&run, &record(), &journal) else {
        panic!("a failed attempt re-asks")
    };
    assert_eq!(
        second.key.0, "run:r-1:limit:src-a:1000:1",
        "the retry rides the :<n> suffix"
    );
    let mut attempts = journal;
    attempts.push(journal_effect(
        &run,
        "limit:src-a:1000:1",
        EffectKind::JevEvaluate,
        EffectState::Unconfirmed,
    ));
    let Some(third) = limit_observed(&run, &record(), &attempts) else {
        panic!("an unconfirmed attempt re-asks too")
    };
    assert_eq!(third.key.0, "run:r-1:limit:src-a:1000:2");
    // An acknowledged-but-unanswered ask (no answered Judgments receipt)
    // is not a completed review — it re-asks as well.
    let mut unanswered = Vec::from([journal_effect(
        &run,
        "limit:src-a:1000",
        EffectKind::JevEvaluate,
        EffectState::Acknowledged,
    )]);
    unanswered[0].receipt = Some(EffectReceipt::Judgments({
        let mut record = review_record(&run, Vec::new());
        record.set.outcome = JudgmentOutcome::Stale;
        record
    }));
    let Some(retry) = limit_observed(&run, &record(), &unanswered) else {
        panic!("a non-answered outcome re-asks")
    };
    assert_eq!(retry.key.0, "run:r-1:limit:src-a:1000:1");
}

#[test]
fn f31_limit_observed_keys_once_per_record_across_generations() {
    // The family carries no generation component (r3): a record asked
    // under one work/evidence generation is asked for the Run's lifetime.
    let mut run = run_in(State::Active);
    run.work_generation = 1;
    run.evidence_generation = 0;
    let Some(ask) = limit_observed(&run, &record(), &[]) else {
        panic!("the first-generation ask plans")
    };
    assert_eq!(
        ask.key.0, "run:r-1:limit:src-a:1000",
        "the key names the record alone — no generation"
    );
    run.work_generation = 4;
    run.evidence_generation = 7;
    assert_eq!(
        limit_observed(&run, &record(), &Vec::from([ask])),
        None,
        "generations moved, the record's ask still suppresses"
    );
    let journal = Vec::from([answered_ask(&run, "limit:src-a:1000")]);
    assert_eq!(
        limit_observed(&run, &record(), &journal),
        None,
        "an answered record never re-asks, generations or not"
    );
}

#[test]
fn f31_blocked_episode_ask_suppresses_limit_ask_same_pass() {
    let run = run_in(State::Active);
    for state in [EffectState::Planned, EffectState::Dispatching] {
        let journal = Vec::from([journal_effect(
            &run,
            "blocked:0",
            EffectKind::JevEvaluate,
            state,
        )]);
        assert_eq!(
            limit_observed(&run, &record(), &journal),
            None,
            "a {state:?} blocked ask carries the record — no same-pass double ask"
        );
    }
    // The suppression keys the CURRENT episode, retries included.
    let journal = Vec::from([journal_effect(
        &run,
        "blocked:0:1",
        EffectKind::JevEvaluate,
        EffectState::Planned,
    )]);
    assert_eq!(
        limit_observed(&run, &record(), &journal),
        None,
        "a blocked retry in flight suppresses the same way"
    );
    // A terminal blocked ask frees the limit ask next pass.
    let settled_blocked = Vec::from([journal_effect(
        &run,
        "blocked:0",
        EffectKind::JevEvaluate,
        EffectState::Failed,
    )]);
    assert!(
        limit_observed(&run, &record(), &settled_blocked).is_some(),
        "a settled blocked ask no longer suppresses"
    );
    // A later episode's ask is a different family — the record ask must
    // not suppress on a stale episode's row either.
    let mut later = run_in(State::Active);
    later.blocked_episode = 1;
    let closed_episode = Vec::from([journal_effect(
        &later,
        "blocked:0",
        EffectKind::JevEvaluate,
        EffectState::Planned,
    )]);
    assert!(
        limit_observed(&later, &record(), &closed_episode).is_some(),
        "a closed episode's blocked ask does not suppress episode 1's pass"
    );
    let current_episode = Vec::from([journal_effect(
        &later,
        "blocked:1",
        EffectKind::JevEvaluate,
        EffectState::Dispatching,
    )]);
    assert_eq!(
        limit_observed(&later, &record(), &current_episode),
        None,
        "the current episode's blocked ask does suppress"
    );
}
