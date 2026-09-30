//! F17 — the outbox: caller follow-ups, serialized per target and sent at the
//! first safe moment. F18 — the mailbox: durable actionable events with
//! stable dedup keys, and the hint rule. F9 — the per-target ordering the
//! `effect_id` link journals.

use alloc::string::String;
use core::time::Duration;

use crate::identity::{
    CallerKey, DedupKey, Digest, EffectId, EventId, LaunchId, MessageKey, RunId,
};

/// N5 — a follow-up body over 16 KiB is first published as a file.
pub const FOLLOWUP_INLINE_MAX_BYTES: usize = 16 * 1024;

/// N5 — a follow-up body over 1 MiB is refused.
pub const FOLLOWUP_FILE_MAX_BYTES: usize = 1024 * 1024;

/// F18 — a body file stays until 7 days after the child's identity is
/// observed absent (H#64).
pub const FOLLOWUP_FILE_RETENTION: Duration = Duration::from_hours(168);

/// F18 — at most one hint prompt per owner per 5 seconds, never retried.
pub const HINT_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// F17/Appendix B `outbox.state` — a follow-up's lifecycle. `expired` is
/// reachable only from `queued` — a dispatched message stays visible in its
/// last state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutboxState {
    /// `queued` — recorded, waiting for the first safe moment (F17).
    Queued,
    /// `dispatching` — its prompt effect is in flight.
    Dispatching,
    /// `submitted` — the prompt was acknowledged.
    Submitted,
    /// `unconfirmed` — dispatch interrupted; possibly consumed — it is an
    /// ordering barrier until transcript evidence or settlement resolves it
    /// (F9).
    Unconfirmed,
    /// `expired` — never dispatched; `expiry_reason` is required (Appendix B
    /// CHECK).
    Expired,
}

impl OutboxState {
    /// Appendix B — the stored spelling of the state.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Submitted => "submitted",
            Self::Unconfirmed => "unconfirmed",
            Self::Expired => "expired",
        }
    }
}

/// F17 — why a never-dispatched follow-up expired. The only cause the spec
/// names is the Run settling (F20 expires the queue in the same transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExpiryReason {
    /// The Run settled before the message was dispatched (F20).
    RunSettled,
}

impl ExpiryReason {
    /// Appendix B — the stored spelling of the reason (`outbox.expiry_reason`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RunSettled => "settled",
        }
    }
}

/// F17 — where a follow-up body lives: the row holds exactly one of the two
/// (Appendix B `body_inline`/`body_path` XOR CHECK).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageBody {
    /// `body_inline` — the body carried in the row.
    Inline(String),
    /// `body_path` — a published immutable 0600 file (write, fsync, rename,
    /// verify size and digest; a failed publication enqueues nothing).
    File {
        /// The published file's path.
        path: String,
    },
}

/// F17/Appendix B `outbox` — one caller follow-up to one Run, keyed by
/// `(run, seq)` with `message_key` unique within the Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxMessage {
    /// The Run it addresses.
    pub run: RunId,
    /// `seq` — its sequence number; the same `message_key` + body digest
    /// returns the existing `seq` (F17).
    pub seq: u64,
    /// `message_key` — caller-supplied; a different digest under the same key
    /// is `MESSAGE_KEY_CONFLICT`.
    pub message_key: MessageKey,
    /// `sender_caller_id` — the verified caller who sent it.
    pub sender: CallerKey,
    /// `body_digest`.
    pub body_digest: Digest,
    /// `body_inline` xor `body_path`.
    pub body: MessageBody,
    /// `state`.
    pub state: OutboxState,
    /// `effect_id` — the prompt effect dispatching it (F9); `None` while
    /// queued and always when `expired` (Appendix B CHECK).
    pub effect: Option<EffectId>,
    /// `expiry_reason` — required when `expired`.
    pub expiry_reason: Option<ExpiryReason>,
}

/// F18 — the actionable mailbox event kinds; `dedup_key` makes a repeat
/// observation never a copy (Appendix B `mailbox.kind` is free-text — the
/// spellings are the spec's bullet names).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MailboxEventKind {
    /// A handoff was accepted (F24).
    HandoffAccepted,
    /// A handoff was rejected (F24).
    HandoffRejected,
    /// The Run settled — any settlement (F20).
    Settled,
    /// `stalled` — the child stalled again after its episode's nudge (F23).
    Stalled,
    /// The child is blocked on input (F23 `blocked_on_input`).
    BlockedOnInput,
    /// The child's work strayed `outside_scope` (F23).
    OutsideScope,
    /// The Launch failed (F5 `failed`).
    LaunchFailed,
    /// The Task prompt's acknowledgement was unconfirmed (F16).
    PromptUnconfirmed,
    /// A follow-up's dispatch was unconfirmed (F17).
    FollowUpUnconfirmed,
    /// A queued follow-up expired undelivered (F17).
    FollowUpExpired,
    /// A provider entered cooldown (F21).
    CooldownHit,
    /// A recovery obligation was recorded `pending` (F21).
    RecoveryPending,
    /// A recovery obligation went `blocked` — abstained, no candidates (F21).
    RecoveryBlocked,
    /// A recovery obligation was `dispatched` (F21).
    RecoveryDispatched,
}

impl MailboxEventKind {
    /// F18 — the stored spelling of the kind.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HandoffAccepted => "handoff_accepted",
            Self::HandoffRejected => "handoff_rejected",
            Self::Settled => "settled",
            Self::Stalled => "stalled",
            Self::BlockedOnInput => "blocked_on_input",
            Self::OutsideScope => "outside_scope",
            Self::LaunchFailed => "launch_failed",
            Self::PromptUnconfirmed => "prompt_unconfirmed",
            Self::FollowUpUnconfirmed => "follow_up_unconfirmed",
            Self::FollowUpExpired => "follow_up_expired",
            Self::CooldownHit => "cooldown_hit",
            Self::RecoveryPending => "recovery_pending",
            Self::RecoveryBlocked => "recovery_blocked",
            Self::RecoveryDispatched => "recovery_dispatched",
        }
    }
}

/// F18/Appendix B — the subject a mailbox event binds: the Run's current
/// owner is derived when read, or the Launch's caller for launch-only events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxSubject {
    /// `run_id` — the destination follows the Run's owner (adoption
    /// redirects unread events — F18/H#91).
    Run(RunId),
    /// `launch_id` — a launch-only event; destination is the Launch's caller.
    Launch(LaunchId),
}

/// F18/Appendix B `mailbox` — one actionable event: stable `dedup_key`,
/// subject, kind and body. `acked_at`/`created_at` are store-stamped and not
/// carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvent {
    /// `event_id`.
    pub id: EventId,
    /// `dedup_key` — unique; e.g. `run:<id>:settled`,
    /// `run:<id>:stalled:<episode>`.
    pub dedup_key: DedupKey,
    /// Which subject the event binds (`run_id` or `launch_id`; Appendix B
    /// CHECK requires at least one — the enum permits exactly one).
    pub subject: MailboxSubject,
    /// `kind`.
    pub kind: MailboxEventKind,
    /// `body_json` — the event body.
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::{
        ExpiryReason, FOLLOWUP_FILE_MAX_BYTES, FOLLOWUP_FILE_RETENTION, FOLLOWUP_INLINE_MAX_BYTES,
        HINT_MIN_INTERVAL, MailboxEventKind, OutboxState,
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

    #[test]
    fn f18_mailbox_event_kind_spellings() {
        let cases = [
            (MailboxEventKind::HandoffAccepted, "handoff_accepted"),
            (MailboxEventKind::HandoffRejected, "handoff_rejected"),
            (MailboxEventKind::Settled, "settled"),
            (MailboxEventKind::Stalled, "stalled"),
            (MailboxEventKind::BlockedOnInput, "blocked_on_input"),
            (MailboxEventKind::OutsideScope, "outside_scope"),
            (MailboxEventKind::LaunchFailed, "launch_failed"),
            (MailboxEventKind::PromptUnconfirmed, "prompt_unconfirmed"),
            (
                MailboxEventKind::FollowUpUnconfirmed,
                "follow_up_unconfirmed",
            ),
            (MailboxEventKind::FollowUpExpired, "follow_up_expired"),
            (MailboxEventKind::CooldownHit, "cooldown_hit"),
            (MailboxEventKind::RecoveryPending, "recovery_pending"),
            (MailboxEventKind::RecoveryBlocked, "recovery_blocked"),
            (MailboxEventKind::RecoveryDispatched, "recovery_dispatched"),
        ];
        for (kind, name) in cases {
            assert_eq!(
                kind.as_str(),
                name,
                "mailbox event kind spelling must match F18"
            );
        }
    }
}
