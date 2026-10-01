//! F24 — handoff and acceptance in the lifecycle: digest suppression
//! follows the completed assessment, `judging_digest` names the in-flight
//! acceptance ask, and the rejection window keeps running through
//! `judging`.

use alloc::vec::Vec;

use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{
    DeadlineKind, EffectCertainty, EffectKind, EffectOutcome, Event, JudgmentVerdict, Settlement,
    State, transition,
};

use super::builders::{
    EMPTY_READ, NOW, dispatched_outbox, effect_keys, frozen, frozen_writes, is_quiet, run_in,
    run_result, settlement_of, stamped, test_policy, updated_run,
};

// ---- F24 — suppression follows the completed assessment, not the freeze
// row: a frozen-but-unassessed digest rewritten into the handoff resumes
// judging (fresh `evidence_generation`, no second freeze row); only an
// assessed digest suppresses (F7's failing sequence). ----

#[test]
pub(super) fn f24_rewritten_unassessed_handoff_resumes_judging() {
    // freeze A in active; rewrite to B before A's judgment lands —
    // B's freeze bumps evidence_generation, so A's ask goes stale (F20).
    let mut run = run_in(State::Active);
    let digest_a = Digest([7; 32]);
    let digest_b = Digest([8; 32]);
    let t_a = transition(
        &run,
        &stamped(&run, Event::Handoff { digest: digest_a }),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    let mut handoffs = Vec::from([frozen_writes(&t_a)[0].clone()]);
    run = updated_run(&t_a).clone();
    assert_eq!(
        run.judging_digest,
        Some(digest_a),
        "the freeze's ask assesses A"
    );
    let t_b = transition(
        &run,
        &stamped(&run, Event::Handoff { digest: digest_b }),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    handoffs.push(frozen_writes(&t_b)[0].clone());
    run = updated_run(&t_b).clone();
    assert_eq!(
        run.judging_digest,
        Some(digest_b),
        "the rewrite's ask assesses B — A's pending answer is already stale"
    );
    // B is rejected → repair. Then the handoff file is rewritten back to
    // A: A was frozen but never assessed — judging must resume for it.
    let t_reject = transition(
        &run,
        &stamped(&run, Event::Judgment(JudgmentVerdict::Reject)),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    run = updated_run(&t_reject).clone();
    assert_eq!(run.state, State::Repair);
    let generation_before = run.evidence_generation;
    let t_rewrite = transition(
        &run,
        &stamped(&run, Event::Handoff { digest: digest_a }),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t_rewrite);
    assert_eq!(
        record.state,
        State::Judging,
        "A's assessment never landed — the rewrite resumes judging it (F24)"
    );
    assert_eq!(
        record.evidence_generation,
        generation_before + 1,
        "a fresh ask passes the F20 stamp check"
    );
    assert!(
        frozen_writes(&t_rewrite).is_empty(),
        "the freeze row stands — no duplicate"
    );
    assert_eq!(
        effect_keys(&t_rewrite),
        Vec::from(["run:r-1:accept:0:3"]),
        "the re-ask names the new evidence generation"
    );
    assert_eq!(
        record.judging_digest,
        Some(digest_a),
        "the resumed ask assesses A"
    );
    assert_eq!(
        record.repair_deadline, run.repair_deadline,
        "the repair window is untouched"
    );
}

#[test]
pub(super) fn f24_assessed_digest_stays_suppressed() {
    let mut run = run_in(State::Repair);
    run.evidence_generation = 2;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700));
    let mut assessed = frozen(&run, 0, 9);
    assessed.assessed = true;
    let handoffs = Vec::from([assessed]);
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
    assert!(
        is_quiet(&t),
        "a completed assessment is the suppression — the digest stays dead (F24)"
    );
}

#[test]
pub(super) fn f24_repeated_handoff_while_its_ask_is_in_flight_is_ignored() {
    // the same digest arrives again while its acceptance ask is still in
    // flight — the frozen row names `judging_digest`, so re-asking would
    // bump `evidence_generation` and stale the pending answer (F20). The
    // repeat is a true no-op: no bump, no ask, no write (F24).
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judging_digest = Some(Digest([9; 32]));
    let handoffs = Vec::from([frozen(&run, 0, 9)]);
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
    assert!(
        is_quiet(&t),
        "the digest under assessment stays put — the pending answer still lands"
    );
}

#[test]
pub(super) fn f24_other_unassessed_digest_resumes_judging() {
    // B's ask is in flight (`judging_digest` = B); the adapter then reports
    // A — frozen but never assessed, and not the digest under judgment. The
    // latest handoff content wins: judging resumes on A at a fresh
    // `evidence_generation` (B's pending answer goes stale, F20), and no
    // second freeze row is written.
    let mut run = run_in(State::Judging);
    run.evidence_generation = 1;
    run.judging_digest = Some(Digest([8; 32]));
    let handoffs = Vec::from([frozen(&run, 0, 7), frozen(&run, 0, 8)]);
    let t = transition(
        &run,
        &stamped(
            &run,
            Event::Handoff {
                digest: Digest([7; 32]),
            },
        ),
        NOW,
        &test_policy(),
        (None, &[], &handoffs),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Judging);
    assert_eq!(
        record.evidence_generation, 2,
        "the re-ask names a fresh generation"
    );
    assert_eq!(
        record.judging_digest,
        Some(Digest([7; 32])),
        "the resumed ask assesses A now"
    );
    assert!(
        frozen_writes(&t).is_empty(),
        "the freeze row stands — no duplicate"
    );
    assert_eq!(effect_keys(&t), Vec::from(["run:r-1:accept:0:2"]));
}

// ---- F24 — the rejection window keeps running through `judging`: an
// in-window repair follow-up qualifies there exactly as in `repair`, and
// `deadline(repair)` waits on a pending qualifying dispatch. ----

#[test]
pub(super) fn f24_judging_repair_followup_in_window_advances_generation() {
    // rejected, re-frozen into `judging`, and the repair follow-up's result
    // lands while the window is still open — it advances the work
    // generation and returns to `active`, clearing the window's fields.
    let mut run = run_in(State::Judging);
    run.evidence_generation = 2;
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(700));
    run.judging_digest = Some(Digest([8; 32]));
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(
        record.work_generation, 1,
        "the in-window dispatch opens a new work generation"
    );
    assert_eq!(record.repair_deadline, None);
    assert_eq!(record.rejected_at, None);
    assert_eq!(
        record.judging_digest, None,
        "no acceptance ask survives the generation advance"
    );
}

#[test]
pub(super) fn f24_judging_repair_deadline_waits_on_a_pending_dispatch() {
    // the deadline fires while a qualifying in-window dispatch is still
    // pending — the Run stays `judging`; the dispatch's result decides.
    let mut run = run_in(State::Judging);
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let t = transition(
        &run,
        &stamped(&run, Event::Deadline(DeadlineKind::Repair)),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert!(
        is_quiet(&t),
        "a qualifying dispatch in flight holds the settle (F24)"
    );
    // and when its result lands past the deadline it still counts — the
    // dispatch commit inside the window is what qualifies (F24).
    let t_ack = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    let record = updated_run(&t_ack);
    assert_eq!(record.state, State::Active);
    assert_eq!(record.work_generation, 1, "the in-window dispatch counts");
}

#[test]
pub(super) fn f24_judging_late_unqualifying_result_settles_rejected() {
    // the deadline passed while the in-window dispatch was pending; its
    // `failed/absent` result proves the prompt never ran, and with nothing
    // qualifying pending the Run settles `rejected` in this transition.
    let mut run = run_in(State::Judging);
    run.rejected_at = Some(Timestamp(200));
    run.repair_deadline = Some(Timestamp(400)); // past at NOW
    let journal = Vec::from([dispatched_outbox(&run, 3, Timestamp(300))]);
    let t = transition(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "outbox:3",
                EffectKind::Prompt,
                EffectOutcome::Failed {
                    certainty: EffectCertainty::Absent,
                },
                None,
            ),
        ),
        NOW,
        &test_policy(),
        (None, &journal, &[]),
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t)),
        Some(Settlement::Rejected),
        "a provably-absent repair past the deadline settles rejected (F24)"
    );
}
