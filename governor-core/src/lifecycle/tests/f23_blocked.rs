//! F23×F17 — the answered stall review against a `Blocked` child: a
//! blocked child is never prompted, so the review plans no nudge and the
//! episode's one nudge stays unspent.

use alloc::vec::Vec;

use crate::identity::ChildStatus;
use crate::lifecycle::{EffectKind, EffectOutcome, EffectReceipt, EffectState, State};
use crate::routing::Question;

use super::builders::{
    effect_keys, effect_writes, noul, review_record, run_in, run_result, stamped, transact,
    updated_records,
};

#[test]
pub(super) fn f23_no_recent_progress_on_a_blocked_child_never_nudges() {
    // F23×F17 — a blocked child is never prompted (the F9 eligibility
    // rule): the answered stall plans no nudge and leaves the episode's one
    // nudge unspent — a stall once the child reports again still nudges.
    let mut run = run_in(State::Active);
    run.child_status = Some(ChildStatus::Blocked);
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
    assert!(
        t.effects.is_empty(),
        "a blocked child is never prompted (F17)"
    );
    assert!(
        t.events.is_empty(),
        "no stalled report — the nudge was never spent"
    );
    assert!(
        updated_records(&t).is_empty(),
        "the episode's one nudge stays unspent"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:review:0", EffectState::Acknowledged)]),
        "the answered set still journals"
    );
    // the unspent episode still nudges once the child reports again.
    let mut working = run.clone();
    working.child_status = Some(ChildStatus::Working);
    let working_record =
        review_record(&working, Vec::from([noul(Question::NoRecentProgress, 0.9)]));
    let t_working = transact(
        &working,
        &stamped(
            &working,
            run_result(
                &working,
                "review:0",
                EffectKind::JevEvaluate,
                EffectOutcome::Acknowledged,
                Some(EffectReceipt::Judgments(working_record)),
            ),
        ),
    );
    assert_eq!(
        effect_keys(&t_working),
        Vec::from(["run:r-1:nudge:0"]),
        "the nudge the blocked answer never spent still fires"
    );
}
