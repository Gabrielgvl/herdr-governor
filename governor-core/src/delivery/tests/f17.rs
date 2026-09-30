//! F17 — the outbox: spellings, admission gates, state transitions, the
//! published-file retention rule and the dispatch effect's keying.

use crate::delivery::{
    ExpiryReason, MessageBody, OutboxMessage, OutboxState, enqueue_follow_up,
    follow_up_body_needs_file, follow_up_body_too_large, follow_up_effect, follow_up_file_retained,
};
use crate::identity::{EffectId, EffectKey, MessageKey, RunId, Timestamp};
use crate::lifecycle::{
    EffectCertainty, EffectKind, EffectOutcome, EffectState, EffectTarget, State,
};
use crate::task::Refusal;

use super::builders::{
    caller, child_identity, digest, dispatched, message, run, settled_run, stranger,
};

#[test]
fn f17_outbox_state_spellings() {
    let cases = [
        (OutboxState::Queued, "queued"),
        (OutboxState::Dispatching, "dispatching"),
        (OutboxState::Submitted, "submitted"),
        (OutboxState::Unconfirmed, "unconfirmed"),
        (OutboxState::Expired, "expired"),
    ];
    for (state, name) in cases {
        assert_eq!(
            state.as_str(),
            name,
            "outbox state spelling must match the DDL"
        );
    }
}

#[test]
fn f17_expiry_reason_spellings() {
    assert_eq!(
        ExpiryReason::RunSettled.as_str(),
        "settled",
        "expiry reason spelling must match the DDL"
    );
}

// ---- F17 admission ----

#[test]
fn f17_same_key_and_digest_returns_existing_seq() {
    let existing = message(4, "k-dup");
    let result = enqueue_follow_up(
        &run(),
        &[existing],
        &caller(),
        &MessageKey("k-dup".into()),
        MessageBody::Inline("body".into()),
        digest(7),
    );
    assert_eq!(
        result,
        Ok((4, None)),
        "same messageKey + same digest returns the existing seq, enqueuing nothing (F17)"
    );
}

#[test]
fn f17_same_key_different_digest_is_message_key_conflict() {
    let existing = message(4, "k-dup");
    let result = enqueue_follow_up(
        &run(),
        &[existing],
        &caller(),
        &MessageKey("k-dup".into()),
        MessageBody::Inline("other".into()),
        digest(9),
    );
    assert_eq!(
        result,
        Err(Refusal::MessageKeyConflict),
        "same messageKey + different digest is MESSAGE_KEY_CONFLICT (F17)"
    );
}

#[test]
fn f17_message_after_settlement_is_run_settled() {
    let result = enqueue_follow_up(
        &settled_run(),
        &[],
        &caller(),
        &MessageKey("k1".into()),
        MessageBody::Inline("b".into()),
        digest(1),
    );
    assert_eq!(
        result,
        Err(Refusal::RunSettled),
        "after settlement the refusal is RUN_SETTLED and nothing enqueues (F17)"
    );
}

#[test]
fn f17_settlement_gate_precedes_key_replay() {
    // Even an idempotent replay (same key, same digest) is refused once
    // the Run is settled — "after settlement" wins over the key rule.
    let existing = message(4, "k-dup");
    let result = enqueue_follow_up(
        &settled_run(),
        &[existing],
        &caller(),
        &MessageKey("k-dup".into()),
        MessageBody::Inline("body".into()),
        digest(7),
    );
    assert_eq!(
        result,
        Err(Refusal::RunSettled),
        "a settled Run refuses even an idempotent retry (F17)"
    );
}

#[test]
fn f4_message_from_non_owner_is_refused() {
    let result = enqueue_follow_up(
        &run(),
        &[],
        &stranger(),
        &MessageKey("k1".into()),
        MessageBody::Inline("b".into()),
        digest(1),
    );
    assert_eq!(
        result,
        Err(Refusal::NotOwner),
        "message requires the current owner (F4)"
    );
}

#[test]
fn f17_enqueue_assigns_the_next_seq_within_the_run() {
    let foreign = OutboxMessage {
        run: RunId("r2".into()),
        ..message(9, "k-foreign")
    };
    let queue = [message(1, "k1"), message(3, "k3"), foreign];
    let result = enqueue_follow_up(
        &run(),
        &queue,
        &caller(),
        &MessageKey("k9".into()),
        MessageBody::File {
            path: "/f/k9".into(),
        },
        digest(2),
    );
    let Ok((seq, Some(row))) = result else {
        panic!("a fresh key on an unsettled Run enqueues (F17)")
    };
    assert_eq!(seq, 4, "seq continues this Run's outbox, not another's");
    assert_eq!(row.seq, 4, "the row carries its seq");
    assert_eq!(row.run, RunId("r1".into()), "the row binds this Run");
    assert_eq!(
        row.message_key,
        MessageKey("k9".into()),
        "the row carries the caller key"
    );
    assert_eq!(row.sender, caller(), "the row records the verified caller");
    assert_eq!(row.body_digest, digest(2), "the row carries the digest");
    assert_eq!(
        row.body,
        MessageBody::File {
            path: "/f/k9".into()
        },
        "the row carries the decided body placement"
    );
    assert_eq!(row.state, OutboxState::Queued, "a new entry lands queued");
    assert_eq!(row.effect, None, "a queued row has no effect link");
    assert_eq!(row.expiry_reason, None, "a queued row has no expiry");
}

#[test]
fn f17_first_message_gets_seq_one() {
    let result = enqueue_follow_up(
        &run(),
        &[],
        &caller(),
        &MessageKey("k1".into()),
        MessageBody::Inline("b".into()),
        digest(1),
    );
    assert_eq!(
        result.map(|pair| pair.0),
        Ok(1),
        "the first follow-up is seq 1"
    );
}

#[test]
fn f17_message_enqueues_while_the_run_is_unsettled() {
    // A `reserved` Run accepts follow-ups: they wait in the outbox until
    // the child exists and the first safe moment arrives (F17).
    let mut reserved = run();
    reserved.state = State::Reserved;
    reserved.prompt_certainty = None;
    reserved.identity = None;
    let result = enqueue_follow_up(
        &reserved,
        &[],
        &caller(),
        &MessageKey("k1".into()),
        MessageBody::Inline("b".into()),
        digest(1),
    );
    assert_eq!(
        result.map(|pair| pair.0),
        Ok(1),
        "an unsettled Run accepts follow-ups"
    );
}

#[test]
fn n5_f17_body_over_16kib_publishes_as_file() {
    assert!(
        !follow_up_body_needs_file(16_384),
        "exactly 16 KiB stays inline (N5)"
    );
    assert!(
        follow_up_body_needs_file(16_385),
        "16 KiB + 1 must publish as a file first (N5/F17)"
    );
    assert!(
        follow_up_body_needs_file(1_048_576),
        "1 MiB still publishes as a file (N5)"
    );
    assert!(
        !follow_up_body_needs_file(1_048_577),
        "over 1 MiB is refused, never published (N5)"
    );
    assert!(!follow_up_body_needs_file(0), "an empty body stays inline");
}

#[test]
fn n5_f17_body_over_1mib_is_refused() {
    assert!(
        !follow_up_body_too_large(1_048_576),
        "1 MiB is within the file bound (N5)"
    );
    assert!(
        follow_up_body_too_large(1_048_577),
        "1 MiB + 1 is refused (N5)"
    );
    assert!(
        follow_up_body_too_large(usize::MAX),
        "an unbounded size is refused (N5)"
    );
}

// ---- F17 outbox state transitions ----

#[test]
fn f17_dispatch_marks_a_queued_entry_dispatching() {
    let dispatched = message(2, "k").into_dispatching(EffectId("eff-9".into()));
    assert_eq!(
        dispatched.map(|m| (m.state, m.effect)),
        Some((OutboxState::Dispatching, Some(EffectId("eff-9".into())))),
        "queued → dispatching records the prompt effect link (F17/F9)"
    );
}

#[test]
fn f17_dispatch_only_starts_from_queued() {
    for state in [
        OutboxState::Dispatching,
        OutboxState::Submitted,
        OutboxState::Unconfirmed,
        OutboxState::Expired,
    ] {
        let entry = dispatched(1, "k", state);
        assert_eq!(
            entry.into_dispatching(EffectId("eff-2".into())),
            None,
            "only queued can start a dispatch (F17)"
        );
    }
}

#[test]
fn f17_acknowledged_dispatch_lands_submitted() {
    let resolved =
        dispatched(1, "k", OutboxState::Dispatching).resolve_dispatch(EffectOutcome::Acknowledged);
    assert_eq!(
        resolved.map(|m| (m.state, m.effect)),
        Some((OutboxState::Submitted, Some(EffectId("eff".into())))),
        "dispatching + acknowledged → submitted, keeping the effect link (F17)"
    );
}

#[test]
fn f17_interrupted_or_failed_dispatch_lands_unconfirmed() {
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::PreInteractiveFailed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Absent,
        },
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let resolved = dispatched(1, "k", OutboxState::Dispatching).resolve_dispatch(outcome);
        assert_eq!(
            resolved.map(|m| m.state),
            Some(OutboxState::Unconfirmed),
            "a non-acknowledged dispatch is the F9 ordering barrier (F17)"
        );
    }
}

#[test]
fn f17_dispatch_resolution_requires_dispatching() {
    for entry in [
        message(1, "k"),
        dispatched(2, "k", OutboxState::Submitted),
        dispatched(3, "k", OutboxState::Unconfirmed),
    ] {
        assert_eq!(
            entry.resolve_dispatch(EffectOutcome::Acknowledged),
            None,
            "no resolution off the dispatching edge (F17)"
        );
    }
}

#[test]
fn f17_transcript_evidence_resolves_unconfirmed_to_submitted() {
    let resolved = dispatched(1, "k", OutboxState::Unconfirmed).resolve_unconfirmed();
    assert_eq!(
        resolved.map(|m| m.state),
        Some(OutboxState::Submitted),
        "finding the envelope's delivery id resolves the barrier (F9/F17)"
    );
    for entry in [
        message(1, "k"),
        dispatched(2, "k", OutboxState::Dispatching),
        dispatched(3, "k", OutboxState::Submitted),
    ] {
        assert_eq!(
            entry.resolve_unconfirmed(),
            None,
            "only an unconfirmed entry resolves on transcript evidence"
        );
    }
}

#[test]
fn f17_only_a_never_dispatched_message_expires() {
    let expired = message(1, "k").into_expired(ExpiryReason::RunSettled);
    assert_eq!(
        expired.map(|m| (m.state, m.expiry_reason, m.effect)),
        Some((OutboxState::Expired, Some(ExpiryReason::RunSettled), None)),
        "queued → expired keeps effect_id NULL and carries a reason (F17/Appendix B)"
    );
    for state in [
        OutboxState::Dispatching,
        OutboxState::Submitted,
        OutboxState::Unconfirmed,
    ] {
        let entry = dispatched(1, "k", state);
        assert_eq!(
            entry.into_expired(ExpiryReason::RunSettled),
            None,
            "a dispatched message stays visible in its last state (F17)"
        );
    }
    let expired_again = dispatched(1, "k", OutboxState::Expired);
    assert_eq!(
        expired_again.into_expired(ExpiryReason::RunSettled),
        None,
        "expired is terminal"
    );
}

#[test]
fn f17_expiry_clears_any_stray_effect_link() {
    // Appendix B CHECK: `expired` implies `effect_id IS NULL`. A queued
    // row carrying an inconsistent link must not leak it into expiry.
    let stray = OutboxMessage {
        effect: Some(EffectId("stray".into())),
        ..message(1, "k")
    };
    assert_eq!(
        stray
            .into_expired(ExpiryReason::RunSettled)
            .map(|m| m.effect),
        Some(None),
        "expiry erases any effect link (Appendix B CHECK)"
    );
}

// ---- F17 file retention ----

#[test]
fn f17_body_files_live_until_seven_days_after_absence() {
    let absent = Timestamp(10_000);
    let within = Timestamp(absent.0.saturating_add(604_799_999));
    let boundary = Timestamp(absent.0.saturating_add(604_800_000));
    for state in [
        OutboxState::Queued,
        OutboxState::Dispatching,
        OutboxState::Submitted,
        OutboxState::Unconfirmed,
    ] {
        assert!(
            follow_up_file_retained(state, None, Timestamp(0)),
            "live or possibly consumed files are kept while the identity is not absent (F17)"
        );
        assert!(
            follow_up_file_retained(state, Some(absent), within),
            "kept inside the 7-day window after observed absent (F17/H#64)"
        );
        assert!(
            !follow_up_file_retained(state, Some(absent), boundary),
            "collectable at the 7-day boundary (F17/H#64)"
        );
    }
}

#[test]
fn f17_expired_entries_protect_no_file() {
    assert!(
        !follow_up_file_retained(OutboxState::Expired, None, Timestamp(0)),
        "an expired message's file was never sent — nothing references it (F17)"
    );
    assert!(
        !follow_up_file_retained(OutboxState::Expired, Some(Timestamp(0)), Timestamp(0)),
        "expired never retains"
    );
}

// ---- F17 dispatch effect ----

#[test]
fn f17_dispatch_effect_is_keyed_by_run_and_seq() {
    let entry = message(4, "k");
    let effect = follow_up_effect(
        &entry,
        child_identity(),
        EffectId("eff-4".into()),
        digest(8),
    );
    assert_eq!(
        effect.key,
        EffectKey("run:r1:outbox:4".into()),
        "the effect key is run:<id>:outbox:<seq> (N1/Appendix B)"
    );
    assert_eq!(
        effect.id,
        EffectId("eff-4".into()),
        "the effect id is the given one"
    );
    assert_eq!(
        effect.kind,
        EffectKind::Prompt,
        "a follow-up dispatch is a prompt"
    );
    assert_eq!(
        effect.subject_run,
        Some(RunId("r1".into())),
        "the effect binds the Run"
    );
    assert_eq!(
        effect.subject_launch, None,
        "an outbox prompt has no launch subject"
    );
    assert_eq!(
        effect.target,
        Some(EffectTarget::Child(child_identity())),
        "the target is the captured child identity (F10)"
    );
    assert_eq!(
        effect.payload_digest,
        Some(digest(8)),
        "the digest of the rendered operation is recorded"
    );
    assert_eq!(
        effect.state,
        EffectState::Planned,
        "effects land planned (F8)"
    );
    assert_eq!(effect.certainty, None, "a planned effect has no certainty");
    assert_eq!(effect.receipt, None, "a planned effect has no receipt");
}
