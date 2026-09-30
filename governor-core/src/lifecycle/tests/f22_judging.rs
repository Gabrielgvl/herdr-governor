//! F22 — the supervision lane of the total transition function: `judging`.

use alloc::vec::Vec;

use crate::delivery::MailboxEventKind;
use crate::identity::{Digest, Observation, Timestamp};
use crate::lifecycle::{
    DeadlineKind, Event, JudgmentVerdict, Settlement, State, UnresolvedReason, transition,
};

use super::builders::{
    EMPTY_READ, NOW, event_dedups, event_kinds, frozen, frozen_writes, is_quiet, run_in,
    settlement_of, stamped, test_policy, transact, updated_run,
};
#[test]
pub(super) fn f22_judging_accept_settles_accepted() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
    );
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::Accepted));
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffAccepted, MailboxEventKind::Settled])
    );
}

#[test]
pub(super) fn f22_judging_reject_enters_repair_and_arms_deadline_once() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Reject)),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Repair);
    assert_eq!(
        record.repair_deadline,
        Some(Timestamp(900_500)),
        "the repair window arms on the first rejection"
    );
    assert_eq!(
        event_kinds(&t),
        Vec::from([MailboxEventKind::HandoffRejected])
    );
    assert_eq!(
        event_dedups(&t),
        Vec::from(["run:r-1:handoff_rejected:0:1"]),
        "the verdict event is scoped to the rejected binding"
    );

    // a second rejection in the same work generation never extends it (F24).
    let mut again = run_in(State::Judging);
    again.evidence_generation = 2;
    again.repair_deadline = Some(Timestamp(600));
    let t_again = transition(
        &again,
        &stamped(&again, Event::Judgment(JudgmentVerdict::Reject)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_again).repair_deadline,
        Some(Timestamp(600)),
        "an armed repair_deadline is preserved, never reset"
    );
}

#[test]
pub(super) fn f22_judging_unavailable_stays_until_deadline() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(800));
    let t = transact(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Unavailable)),
    );
    assert!(is_quiet(&t), "an armed deadline already bounds the wait");
    // with no deadline armed the transition arms it.
    let mut unarmed = run_in(State::Judging);
    unarmed.evidence_generation = 1;
    let t_unarmed = transition(
        &unarmed,
        &stamped(&unarmed, Event::Judgment(JudgmentVerdict::Unavailable)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        updated_run(&t_unarmed).judgment_deadline,
        Some(Timestamp(1_800_500))
    );
}

#[test]
pub(super) fn f22_judging_deadlines() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judgment_deadline = Some(Timestamp(400));
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable
        }),
        "the bound expires → unresolved(judgment_unavailable)"
    );
    // a not-yet-passed judgment deadline is a no-op.
    run.judgment_deadline = Some(Timestamp(600));
    let t_early = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Judgment)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert!(is_quiet(&t_early));
    // the repair deadline keeps running through judging (F24).
    run.judgment_deadline = Some(Timestamp(600));
    run.repair_deadline = Some(Timestamp(400));
    let t_repair = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t_repair)),
        Some(Settlement::Rejected),
        "a re-frozen handoff does not extend the repair deadline"
    );
}

#[test]
pub(super) fn f22_judging_absent_stays_judging() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
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
    assert!(is_quiet(&t), "judging waits on the judgment, not the pane");
}

#[test]
pub(super) fn f22_judging_new_digest_refreezes_same_digest_is_ignored() {
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
    // a digest already frozen for this generation is never re-judged (F24).
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([9; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    assert!(is_quiet(&t), "the known digest is not re-frozen");
    // a rewritten handoff is new evidence → freeze again, stay judging.
    let t_rewrite = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([10; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t_rewrite);
    assert_eq!(record.state, State::Judging);
    assert_eq!(record.evidence_generation, 2);
    assert_eq!(frozen_writes(&t_rewrite).len(), 1);
}
