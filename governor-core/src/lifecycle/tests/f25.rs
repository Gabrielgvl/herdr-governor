//! F25 — idle episodes and pane loss.

use alloc::vec::Vec;

use crate::identity::{ChildStatus, Timestamp};
use crate::lifecycle::{DeadlineKind, Event, Settlement, State};

use super::builders::{
    NOW, effect_keys, is_quiet, obs_unique, run_in, settlement_of, stamped, transact, updated_run,
};
#[test]
fn f25_idle_episode_opens_nudge_and_deadline() {
    let run = run_in(State::Active);
    for status in [ChildStatus::Idle, ChildStatus::Done] {
        let t = transact(&run, &stamped(&run, obs_unique(Some(status))));
        let record = updated_run(&t);
        assert_eq!(
            record.idle_since,
            Some(NOW),
            "the episode opens at the observation"
        );
        assert_eq!(
            record.idle_deadline,
            Some(Timestamp(900_500)),
            "idle_deadline is the episode start + 15 minutes"
        );
        assert_eq!(
            effect_keys(&t),
            Vec::from(["run:r-1:nudge:0"]),
            "the child gets the episode's one nudge"
        );
        assert_eq!(record.nudged_episode, Some(0));
    }
}

#[test]
fn f25_repeated_idle_does_not_renudge_or_extend() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(910));
    run.nudged_episode = Some(0);
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Idle))));
    let record = updated_run(&t);
    assert_eq!(
        record.idle_since,
        Some(Timestamp(10)),
        "the episode does not restart"
    );
    assert_eq!(record.idle_deadline, Some(Timestamp(910)));
    assert!(t.effects.is_empty(), "one nudge per episode (F23)");
}

#[test]
fn f25_stall_then_idle_shares_one_episode() {
    let mut run = run_in(State::Active);
    run.nudged_episode = Some(0); // a stall already spent the episode's nudge
    let t = transact(&run, &stamped(&run, obs_unique(Some(ChildStatus::Idle))));
    let record = updated_run(&t);
    assert_eq!(record.idle_since, Some(NOW));
    assert!(
        t.effects.is_empty(),
        "stall and idle share the episode's one nudge"
    );
}

#[test]
fn f25_idle_deadline_settles_no_handoff() {
    let mut run = run_in(State::Active);
    run.idle_since = Some(Timestamp(10));
    run.idle_deadline = Some(Timestamp(400));
    let t = transact(&run, &stamped(&run, Event::Deadline(DeadlineKind::Idle)));
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::NoHandoff),
        "the child that never wrote a handoff settles no_handoff"
    );
    // before the deadline → nothing.
    let mut run_early = run_in(State::Active);
    run_early.idle_deadline = Some(Timestamp(600));
    let t_early = transact(
        &run_early,
        &stamped(&run_early, Event::Deadline(DeadlineKind::Idle)),
    );
    assert!(is_quiet(&t_early));
    // idle deadlines never fire outside active.
    let mut run_judging = run_in(State::Judging);
    run_judging.evidence_generation = 1;
    run_judging.idle_deadline = Some(Timestamp(400));
    let t_judging = transact(
        &run_judging,
        &stamped(&run_judging, Event::Deadline(DeadlineKind::Idle)),
    );
    assert!(
        is_quiet(&t_judging),
        "judging answers to judgment_deadline only"
    );
}
