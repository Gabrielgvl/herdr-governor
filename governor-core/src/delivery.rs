//! F17 — the outbox: caller follow-ups, serialized per target and sent at the
//! first safe moment. F18 — the mailbox: durable actionable events with
//! stable dedup keys, and the hint rule. F9 — the per-target ordering the
//! `effect_id` link journals.
//!
//! The types above the functions are the fixed vocabulary; the functions are
//! the pure rules: admission, dispatch eligibility, state transitions, event
//! emission and hint gating. Time, digests, identity and snapshot reads are
//! inputs; nothing here does I/O.

use alloc::format;
use alloc::string::String;
use core::time::Duration;

use crate::config::Capability;
use crate::identity::{
    CallerKey, ChildIdentity, ChildStatus, DedupKey, Digest, EffectId, EffectKey, EventId,
    LaunchId, MessageKey, Observation, PaneId, RunId, Timestamp,
};
use crate::lifecycle::{
    Effect, EffectKind, EffectOutcome, EffectState, EffectTarget, PromptCertainty, Run, State,
};
use crate::task::Refusal;

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

    /// F9/F17 — whether the entry still occupies the target's delivery
    /// pipeline: waiting for a safe moment, in flight, or an unresolved
    /// ordering barrier. `submitted` and `expired` are terminal for the
    /// pipeline (the rows stay visible).
    #[must_use]
    fn holds_pipeline(self) -> bool {
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

    /// F18 — whether the kind binds a Launch rather than a Run (the
    /// destination is then the Launch's caller instead of a Run's current
    /// owner). Only `launch_failed` is launch-only.
    #[must_use]
    pub const fn binds_launch(&self) -> bool {
        match self {
            Self::LaunchFailed => true,
            Self::HandoffAccepted
            | Self::HandoffRejected
            | Self::Settled
            | Self::Stalled
            | Self::BlockedOnInput
            | Self::OutsideScope
            | Self::PromptUnconfirmed
            | Self::FollowUpUnconfirmed
            | Self::FollowUpExpired
            | Self::CooldownHit
            | Self::RecoveryPending
            | Self::RecoveryBlocked
            | Self::RecoveryDispatched => false,
        }
    }

    /// F18 — the stable `dedup_key` for one event:
    /// `launch:<id>:<kind>` for the launch-only kind, `run:<id>:<kind>` for
    /// a kind that fires at most once per Run, and
    /// `run:<id>:<kind>:<qualifier>` for a recurrent kind — the qualifier is
    /// the stall's `nudge_episode` for `stalled`, the `evidence_generation`
    /// the judgment was bound to for `blocked_on_input`, `outside_scope`
    /// and `handoff_rejected`, and the outbox `seq` for the follow-up
    /// kinds. A repeat of the same observation yields the same key, so the
    /// store's UNIQUE constraint makes re-emission a no-op.
    ///
    /// Returns `None` when the kind does not bind that subject, or when the
    /// qualifier rule is violated (missing for a recurrent kind, present
    /// for a one-shot kind) — the caller must fix the emission rather than
    /// invent a key.
    #[must_use]
    pub fn dedup_key(&self, subject: &MailboxSubject, qualifier: Option<u64>) -> Option<DedupKey> {
        let base = match (self.binds_launch(), subject) {
            (true, MailboxSubject::Launch(launch)) => {
                format!("launch:{}:{}", launch.0, self.as_str())
            }
            (false, MailboxSubject::Run(run)) => {
                format!("run:{}:{}", run.0, self.as_str())
            }
            (true, MailboxSubject::Run(_)) | (false, MailboxSubject::Launch(_)) => return None,
        };
        let qualified = match self {
            Self::Stalled
            | Self::BlockedOnInput
            | Self::OutsideScope
            | Self::HandoffRejected
            | Self::FollowUpUnconfirmed
            | Self::FollowUpExpired => true,
            Self::HandoffAccepted
            | Self::Settled
            | Self::LaunchFailed
            | Self::PromptUnconfirmed
            | Self::CooldownHit
            | Self::RecoveryPending
            | Self::RecoveryBlocked
            | Self::RecoveryDispatched => false,
        };
        match (qualified, qualifier) {
            (true, Some(qualifier_value)) => Some(DedupKey(format!("{base}:{qualifier_value}"))),
            (false, None) => Some(DedupKey(base)),
            (true, None) | (false, Some(_)) => None,
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

impl MailboxEvent {
    /// F18 — emit one actionable event: validates that the kind binds the
    /// given subject (`launch_failed` takes a `Launch`, every other kind a
    /// `Run`) and derives the stable `dedup_key`; `qualifier` is required
    /// exactly for the recurrent kinds (see
    /// [`MailboxEventKind::dedup_key`]). Returns `None` on a violated
    /// pairing or qualifier rule rather than emitting an unkeyable event.
    #[must_use]
    pub fn emitted(
        id: EventId,
        subject: MailboxSubject,
        kind: MailboxEventKind,
        qualifier: Option<u64>,
        body: String,
    ) -> Option<Self> {
        let dedup_key = kind.dedup_key(&subject, qualifier)?;
        Some(Self {
            id,
            dedup_key,
            subject,
            kind,
            body,
        })
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

/// F9 — a `planned` or `dispatching` prompt effect occupies the target's
/// single prompt slot; `unconfirmed` is the separate ordering barrier (it
/// is terminal, not in flight).
fn prompt_in_flight(state: EffectState) -> bool {
    match state {
        EffectState::Planned | EffectState::Dispatching => true,
        EffectState::Acknowledged | EffectState::Failed | EffectState::Unconfirmed => false,
    }
}

/// F9/F17 — which queued follow-up, if any, may dispatch to the Run's
/// captured identity at this moment.
///
/// The gates, in order:
/// - settlement or a pre-`active` state — the Task prompt precedes every
///   follow-up in F9's order, so nothing overtakes it;
/// - an `unconfirmed` prompt to the identity bars the queue: the Task
///   prompt's `prompt_certainty`, a journaled `unconfirmed` prompt effect,
///   or an `unconfirmed` entry at the head (F9 — until transcript evidence
///   or settlement resolves it);
/// - a `planned` or `dispatching` prompt effect to the identity holds the
///   single slot — one prompt at a time;
/// - the head of the queue is the earliest entry still holding the
///   pipeline (`dispatching` holds the slot, `unconfirmed` bars it);
/// - F17's safe moment: never while `blocked` (H#17); a qualified
///   `mid_turn_input` sends immediately — even while `working` or before
///   the first observation — otherwise only when the child was last
///   reported `idle` or `done`.
///
/// `prompt_effects` is the run's effect-journal slice; only `prompt` rows
/// whose target is this Run's `Child` identity count — a hint effect
/// addresses the *owner's* pane, a different captured identity, and never
/// serializes behind the child's queue.
#[must_use]
pub fn next_dispatchable_follow_up<'a>(
    run: &Run,
    outbox: &'a [OutboxMessage],
    prompt_effects: &[Effect],
    qualified: &[Capability],
) -> Option<&'a OutboxMessage> {
    if run.settlement.is_some() {
        return None;
    }
    match run.state {
        State::Active | State::Judging | State::Repair => {}
        State::Reserved | State::Starting | State::Prompting | State::Settled => return None,
    }
    let prompts_to_child = |effect: &Effect| {
        effect.kind == EffectKind::Prompt
            && effect.subject_run.as_ref() == Some(&run.id)
            && match &effect.target {
                Some(EffectTarget::Child(_)) => true,
                Some(
                    EffectTarget::ExistingTab(_)
                    | EffectTarget::CallerContext(_)
                    | EffectTarget::AgentPane(_),
                )
                | None => false,
            }
    };
    let barrier = run.prompt_certainty == Some(PromptCertainty::Unconfirmed)
        || prompt_effects
            .iter()
            .any(|effect| prompts_to_child(effect) && effect.state == EffectState::Unconfirmed);
    if barrier {
        return None;
    }
    let in_flight = prompt_effects
        .iter()
        .any(|effect| prompts_to_child(effect) && prompt_in_flight(effect.state));
    if in_flight {
        return None;
    }
    let head = outbox
        .iter()
        .filter(|m| m.run == run.id && m.state.holds_pipeline())
        .min_by_key(|m| m.seq)?;
    match head.state {
        OutboxState::Queued => {}
        OutboxState::Dispatching
        | OutboxState::Unconfirmed
        | OutboxState::Submitted
        | OutboxState::Expired => return None,
    }
    if run.child_status == Some(ChildStatus::Blocked) {
        return None;
    }
    if qualified
        .iter()
        .any(|cap| cap.as_str() == Capability::MID_TURN_INPUT)
    {
        return Some(head);
    }
    match run.child_status {
        Some(ChildStatus::Idle | ChildStatus::Done) => Some(head),
        Some(ChildStatus::Working | ChildStatus::Blocked) | None => None,
    }
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

/// F18 — whether one hint prompt may go to the owner's pane now: the pane
/// must read `unique` on a fresh snapshot, be `idle` or `done`, and still
/// hold the owner's native session, and the owner's harness must carry a
/// qualified `hint_consumption` capability. A `busy` pane — `working`,
/// `blocked`, or one Herdr has not reported — gets nothing, and at most
/// one hint goes to an owner per `HINT_MIN_INTERVAL` (`last_hint_at` is
/// that owner's last hint). The returned pane is the dispatch target;
/// a hint is never retried — `event:<id>:hint` plans at most once.
#[must_use]
pub fn hint_eligible(
    owner: &CallerKey,
    observation: &Observation,
    qualified: &[Capability],
    last_hint_at: Option<Timestamp>,
    now: Timestamp,
) -> Option<PaneId> {
    let (status, pane, native_session) = match observation {
        Observation::Unique {
            status,
            pane,
            native_session,
        } => (*status, pane, native_session),
        Observation::Absent | Observation::Invalid => return None,
    };
    if native_session.as_ref() != Some(&owner.native_session) {
        return None;
    }
    match status {
        Some(ChildStatus::Idle | ChildStatus::Done) => {}
        Some(ChildStatus::Working | ChildStatus::Blocked) | None => return None,
    }
    if !qualified
        .iter()
        .any(|cap| cap.as_str() == Capability::HINT_CONSUMPTION)
    {
        return None;
    }
    if let Some(last) = last_hint_at {
        let interval_ms = i64::try_from(HINT_MIN_INTERVAL.as_millis()).unwrap_or(i64::MAX);
        if now.0.saturating_sub(last.0) < interval_ms {
            return None;
        }
    }
    Some(pane.clone())
}

/// F18 — the hint's `prompt` effect for a committed `event`:
/// `event:<id>:hint` keys the journal's once-only rule (never retried —
/// a failed or interrupted hint is terminal in the journal). The target
/// is `CallerContext`: the owner's pane locator, re-resolved fresh at
/// dispatch (F10) — the vocabulary's only caller-pane variant. The effect
/// binds the event's subject so dispatch re-derives the destination owner.
#[must_use]
pub fn hint_effect(
    event: &MailboxEvent,
    pane: PaneId,
    id: EffectId,
    payload_digest: Digest,
) -> Effect {
    let (subject_launch, subject_run) = match &event.subject {
        MailboxSubject::Run(run) => (None, Some(run.clone())),
        MailboxSubject::Launch(launch) => (Some(launch.clone()), None),
    };
    Effect {
        id,
        key: EffectKey(format!("event:{}:hint", event.id.0)),
        kind: EffectKind::Prompt,
        subject_launch,
        subject_run,
        target: Some(EffectTarget::CallerContext(pane)),
        payload_digest: Some(payload_digest),
        state: EffectState::Planned,
        certainty: None,
        receipt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExpiryReason, FOLLOWUP_FILE_MAX_BYTES, FOLLOWUP_FILE_RETENTION, FOLLOWUP_INLINE_MAX_BYTES,
        HINT_MIN_INTERVAL, MailboxEvent, MailboxEventKind, MailboxSubject, MessageBody,
        OutboxMessage, OutboxState, enqueue_follow_up, follow_up_body_needs_file,
        follow_up_body_too_large, follow_up_effect, follow_up_file_retained, hint_effect,
        hint_eligible, next_dispatchable_follow_up,
    };
    use crate::config::Capability;
    use crate::identity::{
        AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, DedupKey, Digest, EffectId,
        EffectKey, EventId, HerdrIncarnation, LaunchId, MessageKey, NativeSession, Observation,
        PaneId, RunId, TerminalId, Timestamp,
    };
    use crate::lifecycle::{
        Effect, EffectCertainty, EffectKind, EffectOutcome, EffectState, EffectTarget,
        PromptCertainty, Run, Settlement, State,
    };
    use crate::task::Refusal;

    fn digest(byte: u8) -> Digest {
        Digest([byte; 32])
    }

    fn caller() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("kind-owner".into()),
            native_session: NativeSession("sess-owner".into()),
        }
    }

    fn stranger() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("kind-stranger".into()),
            native_session: NativeSession("sess-stranger".into()),
        }
    }

    fn child_identity() -> ChildIdentity {
        ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: AgentKind("kind-child".into()),
            agent_name: AgentName("gov-deadbeef".into()),
            native_session: Some(NativeSession("sess-child".into())),
            pane_id: PaneId("w6:p2".into()),
        }
    }

    /// An `active` run with an acknowledged Task prompt and an idle child —
    /// the neutral base each test perturbs one field of.
    fn run() -> Run {
        Run {
            id: RunId("r1".into()),
            launch: LaunchId("l1".into()),
            owner: caller(),
            owner_generation: 0,
            version: 3,
            state: State::Active,
            prompt_certainty: Some(PromptCertainty::Acknowledged),
            child_name: "gov-deadbeef".into(),
            identity: Some(child_identity()),
            operating_point: None,
            provider: None,
            tier_start: None,
            cwd: "/proj".into(),
            base_commit: None,
            work_generation: 0,
            evidence_generation: 0,
            child_status: Some(ChildStatus::Idle),
            idle_since: None,
            idle_deadline: None,
            repair_deadline: None,
            judgment_deadline: None,
            max_age_deadline: Timestamp(86_400_000),
            nudge_episode: 0,
            nudged_episode: None,
            settlement: None,
            settled_at: None,
        }
    }

    /// A queued outbox entry on `run()` — mutate `state`/`effect` for the
    /// dispatched states (a dispatched row carries its `effect_id`).
    fn message(seq: u64, key: &str) -> OutboxMessage {
        OutboxMessage {
            run: RunId("r1".into()),
            seq,
            message_key: MessageKey(key.into()),
            sender: caller(),
            body_digest: digest(7),
            body: MessageBody::Inline("body".into()),
            state: OutboxState::Queued,
            effect: None,
            expiry_reason: None,
        }
    }

    fn dispatched(seq: u64, key: &str, state: OutboxState) -> OutboxMessage {
        OutboxMessage {
            state,
            effect: Some(EffectId("eff".into())),
            ..message(seq, key)
        }
    }

    fn settled_run() -> Run {
        Run {
            state: State::Settled,
            settlement: Some(Settlement::NoHandoff),
            settled_at: Some(Timestamp(9_000)),
            ..run()
        }
    }

    /// A `prompt` effect to the child's captured identity, in `state`.
    fn child_prompt_effect(state: EffectState) -> Effect {
        Effect {
            id: EffectId("e1".into()),
            key: EffectKey("run:r1:prompt:task".into()),
            kind: EffectKind::Prompt,
            subject_launch: None,
            subject_run: Some(RunId("r1".into())),
            target: Some(EffectTarget::Child(child_identity())),
            payload_digest: None,
            state,
            certainty: None,
            receipt: None,
        }
    }

    fn qualified(caps: &[&'static str]) -> alloc::vec::Vec<Capability> {
        caps.iter().map(|name| Capability((*name).into())).collect()
    }

    fn owner_pane(status: Option<ChildStatus>, session: Option<NativeSession>) -> Observation {
        Observation::Unique {
            status,
            pane: PaneId("w6:p1".into()),
            native_session: session,
        }
    }

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
        let resolved = dispatched(1, "k", OutboxState::Dispatching)
            .resolve_dispatch(EffectOutcome::Acknowledged);
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

    // ---- F9/F17 dispatch eligibility ----

    #[test]
    fn f17_queued_follow_up_is_eligible_when_idle_or_done() {
        let queue = [message(1, "k1")];
        for status in [ChildStatus::Idle, ChildStatus::Done] {
            let mut observed = run();
            observed.child_status = Some(status);
            assert_eq!(
                next_dispatchable_follow_up(&observed, &queue, &[], &[]).map(|m| m.seq),
                Some(1),
                "idle/done is the safe moment without mid_turn_input (F17)"
            );
        }
    }

    #[test]
    fn f17_mid_turn_input_sends_immediately() {
        let queue = [message(1, "k1")];
        let mid_turn = qualified(&[Capability::MID_TURN_INPUT]);
        for status in [Some(ChildStatus::Working), None] {
            let mut observed = run();
            observed.child_status = status;
            assert_eq!(
                next_dispatchable_follow_up(&observed, &queue, &[], &mid_turn).map(|m| m.seq),
                Some(1),
                "a qualified mid_turn_input sends immediately (F17)"
            );
        }
    }

    #[test]
    fn f17_never_sends_while_blocked() {
        let queue = [message(1, "k1")];
        let mid_turn = qualified(&[Capability::MID_TURN_INPUT]);
        let mut blocked = run();
        blocked.child_status = Some(ChildStatus::Blocked);
        assert_eq!(
            next_dispatchable_follow_up(&blocked, &queue, &[], &[]),
            None,
            "never while blocked (F17/H#17)"
        );
        assert_eq!(
            next_dispatchable_follow_up(&blocked, &queue, &[], &mid_turn),
            None,
            "mid_turn_input never beats blocked (F17/H#17)"
        );
    }

    #[test]
    fn f17_waits_while_working_without_mid_turn_input() {
        let queue = [message(1, "k1")];
        for (status, caps) in [
            (Some(ChildStatus::Working), qualified(&[])),
            (None, qualified(&[])),
            (Some(ChildStatus::Working), qualified(&["followup_read"])),
        ] {
            let mut observed = run();
            observed.child_status = status;
            assert_eq!(
                next_dispatchable_follow_up(&observed, &queue, &[], &caps),
                None,
                "without qualified mid_turn_input the child must be idle/done (F17)"
            );
        }
    }

    #[test]
    fn f17_nothing_dispatches_after_settlement() {
        let queue = [message(1, "k1")];
        assert_eq!(
            next_dispatchable_follow_up(&settled_run(), &queue, &[], &[]),
            None,
            "settlement ends delivery (F17/F20)"
        );
    }

    #[test]
    fn f9_follow_ups_never_overtake_the_task_prompt() {
        let queue = [message(1, "k1")];
        for state in [State::Reserved, State::Starting, State::Prompting] {
            let mut early = run();
            early.state = state;
            assert_eq!(
                next_dispatchable_follow_up(&early, &queue, &[], &[]),
                None,
                "the Task prompt precedes every follow-up (F9)"
            );
        }
        for state in [State::Judging, State::Repair] {
            let mut supervised = run();
            supervised.state = state;
            assert_eq!(
                next_dispatchable_follow_up(&supervised, &queue, &[], &[]).map(|m| m.seq),
                Some(1),
                "judging and repair stay deliverable (F17/F24)"
            );
        }
    }

    #[test]
    fn f9_prompts_to_one_identity_dispatch_one_at_a_time_in_order() {
        // The earliest pipeline-holding entry is the only candidate; a
        // submitted head does not block the queue behind it.
        let queue = [
            dispatched(1, "k1", OutboxState::Submitted),
            message(2, "k2"),
            message(3, "k3"),
        ];
        assert_eq!(
            next_dispatchable_follow_up(&run(), &queue, &[], &[]).map(|m| m.seq),
            Some(2),
            "the head of the queue dispatches first (F9/F17)"
        );
        // A prompt effect in flight occupies the single slot.
        for state in [EffectState::Planned, EffectState::Dispatching] {
            assert_eq!(
                next_dispatchable_follow_up(&run(), &queue, &[child_prompt_effect(state)], &[]),
                None,
                "one prompt at a time per captured identity (F9)"
            );
        }
        // A dispatching queue entry holds the slot for the rest of the queue.
        let held = [
            dispatched(1, "k1", OutboxState::Dispatching),
            message(2, "k2"),
        ];
        assert_eq!(
            next_dispatchable_follow_up(&run(), &held, &[], &[]),
            None,
            "a dispatching entry is the slot-holder, not a later queued one (F9)"
        );
    }

    #[test]
    fn f9_an_unconfirmed_prompt_is_an_ordering_barrier() {
        let queue = [message(1, "k1")];
        // A journaled prompt effect left unconfirmed bars the queue.
        assert_eq!(
            next_dispatchable_follow_up(
                &run(),
                &queue,
                &[child_prompt_effect(EffectState::Unconfirmed)],
                &[],
            ),
            None,
            "an unconfirmed prompt effect bars delivery (F9)"
        );
        // The Task prompt's own unconfirmed certainty bars it too.
        let mut unconfirmed_prompt = run();
        unconfirmed_prompt.prompt_certainty = Some(PromptCertainty::Unconfirmed);
        assert_eq!(
            next_dispatchable_follow_up(&unconfirmed_prompt, &queue, &[], &[]),
            None,
            "an unconfirmed Task prompt bars follow-ups (F9/F16)"
        );
        // And an unconfirmed entry at the head holds the queue.
        let held = [
            dispatched(1, "k1", OutboxState::Unconfirmed),
            message(2, "k2"),
        ];
        assert_eq!(
            next_dispatchable_follow_up(&run(), &held, &[], &[]),
            None,
            "an unconfirmed outbox entry is the barrier (F9/F17)"
        );
    }

    #[test]
    fn f9_a_resolved_prompt_frees_the_pipeline() {
        let queue = [message(1, "k1")];
        for state in [EffectState::Acknowledged, EffectState::Failed] {
            assert_eq!(
                next_dispatchable_follow_up(&run(), &queue, &[child_prompt_effect(state)], &[])
                    .map(|m| m.seq),
                Some(1),
                "a resolved prompt neither bars nor occupies the slot (F9)"
            );
        }
        // An unconfirmed prompt to a *different* captured identity — a hint
        // addresses the owner's pane — is not this queue's barrier.
        let unconfirmed_hint = Effect {
            key: EffectKey("event:ev1:hint".into()),
            target: Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
            ..child_prompt_effect(EffectState::Unconfirmed)
        };
        assert_eq!(
            next_dispatchable_follow_up(&run(), &queue, &[unconfirmed_hint], &[]).map(|m| m.seq),
            Some(1),
            "an unconfirmed hint prompt is not the child's barrier (F9)"
        );
    }

    #[test]
    fn f9_only_prompts_to_this_identity_serialize() {
        let queue = [message(1, "k1")];
        // A hint prompt targets the owner's pane — a different captured
        // identity — so it never bars the child's queue.
        let hint = Effect {
            key: EffectKey("event:ev1:hint".into()),
            subject_run: Some(RunId("r1".into())),
            target: Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
            ..child_prompt_effect(EffectState::Dispatching)
        };
        // A prompt for a different run and a non-prompt effect are likewise
        // outside this identity's pipeline.
        let foreign = Effect {
            key: EffectKey("run:r2:outbox:1".into()),
            subject_run: Some(RunId("r2".into())),
            ..child_prompt_effect(EffectState::Dispatching)
        };
        let jev = Effect {
            kind: EffectKind::JevEvaluate,
            target: None,
            ..child_prompt_effect(EffectState::Dispatching)
        };
        assert_eq!(
            next_dispatchable_follow_up(&run(), &queue, &[hint, foreign, jev], &[]).map(|m| m.seq),
            Some(1),
            "only prompts to this Run's captured identity hold its slot (F9)"
        );
    }

    #[test]
    fn f9_empty_or_foreign_queues_dispatch_nothing() {
        assert_eq!(
            next_dispatchable_follow_up(&run(), &[], &[], &[]),
            None,
            "an empty outbox dispatches nothing"
        );
        let foreign = OutboxMessage {
            run: RunId("r2".into()),
            ..message(1, "k")
        };
        assert_eq!(
            next_dispatchable_follow_up(&run(), &[foreign], &[], &[]),
            None,
            "another Run's outbox is not this Run's queue"
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

    // ---- F18 mailbox ----

    #[test]
    fn f18_run_scoped_dedup_key_spellings() {
        let subject = MailboxSubject::Run(RunId("r1".into()));
        let cases = [
            (MailboxEventKind::HandoffAccepted, "run:r1:handoff_accepted"),
            (MailboxEventKind::Settled, "run:r1:settled"),
            (
                MailboxEventKind::PromptUnconfirmed,
                "run:r1:prompt_unconfirmed",
            ),
            (MailboxEventKind::CooldownHit, "run:r1:cooldown_hit"),
            (MailboxEventKind::RecoveryPending, "run:r1:recovery_pending"),
            (MailboxEventKind::RecoveryBlocked, "run:r1:recovery_blocked"),
            (
                MailboxEventKind::RecoveryDispatched,
                "run:r1:recovery_dispatched",
            ),
        ];
        for (kind, key) in cases {
            assert_eq!(
                kind.dedup_key(&subject, None),
                Some(DedupKey(key.into())),
                "one-shot run kinds dedup per run (F18)"
            );
        }
    }

    #[test]
    fn f18_recurrent_kinds_dedup_per_qualifier() {
        let subject = MailboxSubject::Run(RunId("r1".into()));
        let cases = [
            (MailboxEventKind::Stalled, 2, "run:r1:stalled:2"),
            (
                MailboxEventKind::BlockedOnInput,
                7,
                "run:r1:blocked_on_input:7",
            ),
            (MailboxEventKind::OutsideScope, 7, "run:r1:outside_scope:7"),
            (
                MailboxEventKind::HandoffRejected,
                5,
                "run:r1:handoff_rejected:5",
            ),
            (
                MailboxEventKind::FollowUpUnconfirmed,
                4,
                "run:r1:follow_up_unconfirmed:4",
            ),
            (
                MailboxEventKind::FollowUpExpired,
                4,
                "run:r1:follow_up_expired:4",
            ),
        ];
        for (kind, qualifier, key) in cases {
            assert_eq!(
                kind.dedup_key(&subject, Some(qualifier)),
                Some(DedupKey(key.into())),
                "recurrent kinds dedup per episode, generation or seq (F18)"
            );
            assert_eq!(
                kind.dedup_key(&subject, None),
                None,
                "a recurrent kind without its qualifier is unkeyable"
            );
        }
        assert_eq!(
            MailboxEventKind::Settled.dedup_key(&subject, Some(1)),
            None,
            "a one-shot kind takes no qualifier"
        );
    }

    #[test]
    fn f18_launch_failed_binds_the_launch() {
        assert_eq!(
            MailboxEventKind::LaunchFailed
                .dedup_key(&MailboxSubject::Launch(LaunchId("l1".into())), None),
            Some(DedupKey("launch:l1:launch_failed".into())),
            "launch_failed keys on the Launch (F18)"
        );
        assert_eq!(
            MailboxEventKind::LaunchFailed
                .dedup_key(&MailboxSubject::Run(RunId("r1".into())), None),
            None,
            "launch_failed never binds a Run"
        );
        assert_eq!(
            MailboxEventKind::Settled
                .dedup_key(&MailboxSubject::Launch(LaunchId("l1".into())), None),
            None,
            "run-scoped kinds never bind a Launch"
        );
        assert!(
            MailboxEventKind::LaunchFailed.binds_launch(),
            "launch_failed is the launch-only kind (F18)"
        );
        assert!(
            !MailboxEventKind::FollowUpExpired.binds_launch(),
            "run events are not launch-bound"
        );
    }

    #[test]
    fn f18_emitted_event_carries_its_stable_dedup_key() {
        let emitted = MailboxEvent::emitted(
            EventId("ev7".into()),
            MailboxSubject::Run(RunId("r1".into())),
            MailboxEventKind::Stalled,
            Some(3),
            "body".into(),
        );
        let Some(event) = emitted else {
            panic!("a well-formed emission succeeds")
        };
        assert_eq!(event.id, EventId("ev7".into()), "the event id is kept");
        assert_eq!(
            event.dedup_key,
            DedupKey("run:r1:stalled:3".into()),
            "the event carries the stable dedup key (F18)"
        );
        assert_eq!(event.kind, MailboxEventKind::Stalled, "the kind is kept");
        assert_eq!(event.body, "body", "the body is kept");
        assert_eq!(
            MailboxEvent::emitted(
                EventId("ev8".into()),
                MailboxSubject::Run(RunId("r1".into())),
                MailboxEventKind::LaunchFailed,
                None,
                "b".into(),
            ),
            None,
            "a kind/subject mis-pairing emits nothing"
        );
        assert_eq!(
            MailboxEvent::emitted(
                EventId("ev9".into()),
                MailboxSubject::Run(RunId("r1".into())),
                MailboxEventKind::Stalled,
                None,
                "b".into(),
            ),
            None,
            "a missing qualifier emits nothing"
        );
    }

    // ---- F18 hints ----

    #[test]
    fn f18_hint_goes_to_a_fresh_unique_idle_or_done_owner_pane() {
        let caps = qualified(&[Capability::HINT_CONSUMPTION]);
        let owner_session = Some(NativeSession("sess-owner".into()));
        for status in [Some(ChildStatus::Idle), Some(ChildStatus::Done)] {
            assert_eq!(
                hint_eligible(
                    &caller(),
                    &owner_pane(status, owner_session.clone()),
                    &caps,
                    None,
                    Timestamp(0),
                ),
                Some(PaneId("w6:p1".into())),
                "a unique idle/done owner pane is hinted (F18)"
            );
        }
    }

    #[test]
    fn f18_hint_never_goes_to_a_busy_or_unproven_pane() {
        let caps = qualified(&[Capability::HINT_CONSUMPTION]);
        let owner_session = Some(NativeSession("sess-owner".into()));
        for status in [Some(ChildStatus::Working), Some(ChildStatus::Blocked), None] {
            assert_eq!(
                hint_eligible(
                    &caller(),
                    &owner_pane(status, owner_session.clone()),
                    &caps,
                    None,
                    Timestamp(0),
                ),
                None,
                "never to a busy pane (F18/H#87)"
            );
        }
        for observation in [Observation::Absent, Observation::Invalid] {
            assert_eq!(
                hint_eligible(&caller(), &observation, &caps, None, Timestamp(0)),
                None,
                "only a fresh unique pane is hinted (F18/F3)"
            );
        }
    }

    #[test]
    fn f18_hint_requires_the_owner_native_session() {
        let caps = qualified(&[Capability::HINT_CONSUMPTION]);
        for session in [Some(NativeSession("sess-replaced".into())), None] {
            assert_eq!(
                hint_eligible(
                    &caller(),
                    &owner_pane(Some(ChildStatus::Idle), session),
                    &caps,
                    None,
                    Timestamp(0),
                ),
                None,
                "the pane must still hold the owner's native session (F18)"
            );
        }
    }

    #[test]
    fn f18_hint_requires_qualified_hint_consumption() {
        for caps in [
            qualified(&[]),
            qualified(&[Capability::MID_TURN_INPUT]),
            qualified(&[Capability::FOLLOWUP_READ]),
        ] {
            assert_eq!(
                hint_eligible(
                    &caller(),
                    &owner_pane(
                        Some(ChildStatus::Idle),
                        Some(NativeSession("sess-owner".into()))
                    ),
                    &caps,
                    None,
                    Timestamp(0),
                ),
                None,
                "the owner's harness needs a qualified hint_consumption (F18/F26)"
            );
        }
    }

    #[test]
    fn f18_hint_rate_limit_is_one_per_five_seconds_per_owner() {
        let caps = qualified(&[Capability::HINT_CONSUMPTION]);
        let pane = owner_pane(
            Some(ChildStatus::Idle),
            Some(NativeSession("sess-owner".into())),
        );
        let last = Timestamp(10_000);
        assert_eq!(
            hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(14_999)),
            None,
            "inside 5 s the owner gets no second hint (F18/H#86)"
        );
        assert_eq!(
            hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(15_000)),
            Some(PaneId("w6:p1".into())),
            "at exactly 5 s the next hint is allowed (F18)"
        );
        assert_eq!(
            hint_eligible(&caller(), &pane, &caps, Some(last), Timestamp(9_000)),
            None,
            "a skewed clock never hurries a hint"
        );
    }

    #[test]
    fn f18_hint_effect_is_keyed_per_event_and_targets_the_owner_pane() {
        let run_event = MailboxEvent {
            id: EventId("ev7".into()),
            dedup_key: DedupKey("run:r1:settled".into()),
            subject: MailboxSubject::Run(RunId("r1".into())),
            kind: MailboxEventKind::Settled,
            body: "b".into(),
        };
        let effect = hint_effect(
            &run_event,
            PaneId("w6:p1".into()),
            EffectId("eff-1".into()),
            digest(3),
        );
        assert_eq!(
            effect.key,
            EffectKey("event:ev7:hint".into()),
            "the hint key is event:<id>:hint — planned at most once (N1/F18)"
        );
        assert_eq!(effect.kind, EffectKind::Prompt, "a hint is a prompt effect");
        assert_eq!(
            effect.target,
            Some(EffectTarget::CallerContext(PaneId("w6:p1".into()))),
            "the target is the owner's pane locator, re-resolved at dispatch (F10)"
        );
        assert_eq!(
            effect.subject_run,
            Some(RunId("r1".into())),
            "a run-bound event keys a run-bound hint"
        );
        assert_eq!(
            effect.subject_launch, None,
            "no launch subject for a run event"
        );
        assert_eq!(
            effect.state,
            EffectState::Planned,
            "effects land planned (F8)"
        );
        assert_eq!(
            effect.payload_digest,
            Some(digest(3)),
            "the rendered hint's digest is recorded"
        );
        let launch_event = MailboxEvent {
            id: EventId("ev8".into()),
            dedup_key: DedupKey("launch:l1:launch_failed".into()),
            subject: MailboxSubject::Launch(LaunchId("l1".into())),
            kind: MailboxEventKind::LaunchFailed,
            body: "b".into(),
        };
        let hint = hint_effect(
            &launch_event,
            PaneId("w6:p1".into()),
            EffectId("eff-2".into()),
            digest(4),
        );
        assert_eq!(
            hint.subject_launch,
            Some(LaunchId("l1".into())),
            "a launch-bound event keys a launch-bound hint (F18)"
        );
        assert_eq!(hint.subject_run, None, "no run subject for a launch event");
    }
}
