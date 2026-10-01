//! F17 — the outbox: a caller follow-up's admission, its lifecycle
//! vocabulary and transitions, the dispatch effect, and the published-file
//! retention rule. N5 — the inline/file body bounds.

use alloc::format;
use alloc::string::String;
use core::time::Duration;

use crate::identity::{
    CallerKey, ChildIdentity, Digest, EffectId, EffectKey, MessageKey, RunId, Timestamp,
};
use crate::lifecycle::{Effect, EffectKind, EffectOutcome, EffectState, EffectTarget, Run};
use crate::task::Refusal;

/// N5 — a follow-up body over 16 KiB is first published as a file.
pub const FOLLOWUP_INLINE_MAX_BYTES: usize = 16 * 1024;

/// N5 — a follow-up body over 1 MiB is refused.
pub const FOLLOWUP_FILE_MAX_BYTES: usize = 1024 * 1024;

/// F18 — a body file stays until 7 days after the child's identity is
/// observed absent (H#64).
pub const FOLLOWUP_FILE_RETENTION: Duration = Duration::from_hours(168);

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

    /// F9/F17 — whether the entry still occupies the target's delivery
    /// pipeline: waiting for a safe moment, in flight, or an unresolved
    /// ordering barrier. `submitted` and `expired` are terminal for the
    /// pipeline (the rows stay visible).
    #[must_use]
    pub(super) fn holds_pipeline(self) -> bool {
        match self {
            Self::Queued | Self::Dispatching | Self::Unconfirmed => true,
            Self::Submitted | Self::Expired => false,
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

impl OutboxMessage {
    /// F17 — `queued` → `dispatching`: the entry's prompt effect is in
    /// flight, journaled as `effect` (F9's ordering link). Any other state
    /// is a no-op — only a queued entry may start a dispatch.
    #[must_use]
    pub fn into_dispatching(self, effect: EffectId) -> Option<Self> {
        match self.state {
            OutboxState::Queued => Some(Self {
                state: OutboxState::Dispatching,
                effect: Some(effect),
                ..self
            }),
            OutboxState::Dispatching
            | OutboxState::Submitted
            | OutboxState::Unconfirmed
            | OutboxState::Expired => None,
        }
    }

    /// F17 — `dispatching` resolves on the effect's outcome: an
    /// acknowledgement lands `submitted`; everything else — interrupted
    /// dispatch, a failure, a pre-interactive refusal — lands
    /// `unconfirmed`, the F9 ordering barrier. The vocabulary has no
    /// re-queue edge (a second dispatch attempt would need an effect-key
    /// spelling it does not define), so even a `failed`/`absent` outcome
    /// stays `unconfirmed` until transcript evidence or settlement resolves
    /// it — never `expired`, since the message was dispatched.
    #[must_use]
    pub fn resolve_dispatch(self, outcome: EffectOutcome) -> Option<Self> {
        match self.state {
            OutboxState::Dispatching => {}
            OutboxState::Queued
            | OutboxState::Submitted
            | OutboxState::Unconfirmed
            | OutboxState::Expired => return None,
        }
        let state = match outcome {
            EffectOutcome::Acknowledged => OutboxState::Submitted,
            EffectOutcome::PreInteractiveFailed
            | EffectOutcome::Failed { certainty: _ }
            | EffectOutcome::Unconfirmed => OutboxState::Unconfirmed,
        };
        Some(Self { state, ..self })
    }

    /// F9/F17 — transcript evidence that found the envelope's delivery id
    /// resolves `unconfirmed` to `submitted`, lifting the ordering barrier.
    /// Any other state is a no-op.
    #[must_use]
    pub fn resolve_unconfirmed(self) -> Option<Self> {
        match self.state {
            OutboxState::Unconfirmed => Some(Self {
                state: OutboxState::Submitted,
                ..self
            }),
            OutboxState::Queued
            | OutboxState::Dispatching
            | OutboxState::Submitted
            | OutboxState::Expired => None,
        }
    }

    /// F17/F20 — `queued` → `expired` with a reason: only a never-dispatched
    /// message may expire, so the row keeps `effect_id` NULL for the
    /// Appendix B CHECK. `dispatching`/`submitted`/`unconfirmed` stay
    /// visible in their last state.
    #[must_use]
    pub fn into_expired(self, reason: ExpiryReason) -> Option<Self> {
        match self.state {
            OutboxState::Queued => Some(Self {
                state: OutboxState::Expired,
                effect: None,
                expiry_reason: Some(reason),
                ..self
            }),
            OutboxState::Dispatching
            | OutboxState::Submitted
            | OutboxState::Unconfirmed
            | OutboxState::Expired => None,
        }
    }
}

/// N5/F17 — a body over `FOLLOWUP_FILE_MAX_BYTES` (1 MiB) is refused at the
/// tool boundary (`text` is schema-bounded; the strict-schema refusal needs
/// no domain code).
#[must_use]
pub const fn follow_up_body_too_large(body_len: usize) -> bool {
    body_len > FOLLOWUP_FILE_MAX_BYTES
}

/// N5/F17 — a body over `FOLLOWUP_INLINE_MAX_BYTES` (16 KiB) and within the
/// file bound must be published as an immutable file first: write, fsync,
/// rename, verify size and digest (H#62–64). A failed publication enqueues
/// nothing — the caller never reaches [`enqueue_follow_up`].
#[must_use]
pub const fn follow_up_body_needs_file(body_len: usize) -> bool {
    body_len > FOLLOWUP_INLINE_MAX_BYTES && !follow_up_body_too_large(body_len)
}

/// F4/F17 — `message {runId, messageKey, text}` admission, on values only.
///
/// Returns `(seq, row)`: the seq to report and, for a new message, the
/// `queued` row to persist via `StateChange::RecordFollowUp`. A repeated
/// `messageKey` with the same body digest returns `(existing seq, None)` —
/// the idempotent hit writes nothing. Checks run in gate order: `NOT_OWNER`
/// (F4 — only the current owner may message), then `RUN_SETTLED` (F17 —
/// nothing enqueues after settlement), then the key rule — a different
/// digest under a taken key is `MESSAGE_KEY_CONFLICT`.
///
/// `body` is the caller's already-decided placement per
/// [`follow_up_body_needs_file`]/[`follow_up_body_too_large`]: `Inline` or
/// a verified published `File` (the publication happened before this call —
/// it is I/O the pure core cannot do).
pub fn enqueue_follow_up(
    run: &Run,
    outbox: &[OutboxMessage],
    sender: &CallerKey,
    message_key: &MessageKey,
    body: MessageBody,
    body_digest: Digest,
) -> Result<(u64, Option<OutboxMessage>), Refusal> {
    if *sender != run.owner {
        return Err(Refusal::NotOwner);
    }
    if run.settlement.is_some() {
        return Err(Refusal::RunSettled);
    }
    if let Some(existing) = outbox
        .iter()
        .find(|m| m.run == run.id && m.message_key == *message_key)
    {
        return if existing.body_digest == body_digest {
            Ok((existing.seq, None))
        } else {
            Err(Refusal::MessageKeyConflict)
        };
    }
    let seq = outbox
        .iter()
        .filter(|m| m.run == run.id)
        .map(|m| m.seq)
        .max()
        .map_or(1, |max| max.saturating_add(1));
    Ok((
        seq,
        Some(OutboxMessage {
            run: run.id.clone(),
            seq,
            message_key: message_key.clone(),
            sender: sender.clone(),
            body_digest,
            body,
            state: OutboxState::Queued,
            effect: None,
            expiry_reason: None,
        }),
    ))
}

/// F9/F17 — the `prompt` effect that dispatches one outbox entry:
/// `run:<id>:outbox:<seq>` keys the journal's once-only rule (N1) and the
/// captured identity is re-verified fresh before the wire write (F10).
/// `payload_digest` is the digest of the enveloped body the caller renders
/// (`payload_digest` in Appendix B).
#[must_use]
pub fn follow_up_effect(
    message: &OutboxMessage,
    identity: ChildIdentity,
    id: EffectId,
    payload_digest: Digest,
) -> Effect {
    Effect {
        id,
        key: EffectKey(format!("run:{}:outbox:{}", message.run.0, message.seq)),
        kind: EffectKind::Prompt,
        subject_launch: None,
        subject_run: Some(message.run.clone()),
        target: Some(EffectTarget::Child(identity)),
        payload_digest: Some(payload_digest),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// F17 — a published body file is retained while its message is live
/// (`queued`/`dispatching`) or possibly consumed (`submitted`/`unconfirmed`)
/// — until `FOLLOWUP_FILE_RETENTION` (7 days) after the child's identity is
/// observed `absent` (H#64). While the identity is not known absent the
/// file is kept. An `expired` message's file was never sent and protects
/// nothing.
#[must_use]
pub fn follow_up_file_retained(
    state: OutboxState,
    absent_since: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    match state {
        OutboxState::Expired => false,
        OutboxState::Queued
        | OutboxState::Dispatching
        | OutboxState::Submitted
        | OutboxState::Unconfirmed => match absent_since {
            None => true,
            Some(absent) => {
                let retention_ms =
                    i64::try_from(FOLLOWUP_FILE_RETENTION.as_millis()).unwrap_or(i64::MAX);
                now.0 < absent.0.saturating_add(retention_ms)
            }
        },
    }
}
