//! F17 — the outbox: caller follow-ups, serialized per target and sent at the
//! first safe moment. F18 — the mailbox: durable actionable events with
//! stable dedup keys, and the hint rule. F9 — the per-target ordering the
//! `effect_id` link journals.
//!
//! The types above the functions are the fixed vocabulary; the functions are
//! the pure rules: admission, dispatch eligibility, state transitions, event
//! emission and hint gating. Time, digests, identity and snapshot reads are
//! inputs; nothing here does I/O.

mod followup;
mod mailbox;
mod serial;

pub use followup::{
    ExpiryReason, FOLLOWUP_FILE_MAX_BYTES, FOLLOWUP_FILE_RETENTION, FOLLOWUP_INLINE_MAX_BYTES,
    FollowUpWrite, MessageBody, OutboxMessage, OutboxState, enqueue_follow_up,
    follow_up_body_needs_file, follow_up_body_too_large, follow_up_effect, follow_up_file_retained,
};
pub use mailbox::{
    HINT_MIN_INTERVAL, MailboxEvent, MailboxEventKind, MailboxSubject, hint_effect, hint_eligible,
};
pub use serial::next_dispatchable_follow_up;

#[cfg(test)]
mod tests {
    mod builders;
    mod f17;
    mod f18;
    mod f9;

    use super::{
        FOLLOWUP_FILE_MAX_BYTES, FOLLOWUP_FILE_RETENTION, FOLLOWUP_INLINE_MAX_BYTES,
        HINT_MIN_INTERVAL,
    };

    #[test]
    fn n5_f18_delivery_bound_values() {
        assert_eq!(
            FOLLOWUP_INLINE_MAX_BYTES, 16_384,
            "inline follow-up bound is 16 KiB (N5)"
        );
        assert_eq!(
            FOLLOWUP_FILE_MAX_BYTES, 1_048_576,
            "published follow-up bound is 1 MiB (N5)"
        );
        assert_eq!(
            FOLLOWUP_FILE_RETENTION.as_secs(),
            604_800,
            "body file retention is 7 days (F18)"
        );
        assert_eq!(
            HINT_MIN_INTERVAL.as_secs(),
            5,
            "hint interval is 5 seconds (F18)"
        );
    }
}
