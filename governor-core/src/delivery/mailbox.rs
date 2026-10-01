//! F18 — the mailbox: durable actionable events with stable dedup keys, the
//! subject a kind binds, and the hint rule for the owner's pane.

use alloc::format;
use alloc::string::String;
use core::time::Duration;

use crate::config::Capability;
use crate::identity::{
    CallerKey, ChildStatus, DedupKey, Digest, EffectId, EffectKey, EventId, LaunchId, Observation,
    PaneId, RunId, Timestamp,
};
use crate::lifecycle::{Effect, EffectKind, EffectState, EffectTarget};

/// F18 — at most one hint prompt per owner per 5 seconds, never retried.
pub const HINT_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// F18 — the actionable mailbox event kinds; `dedup_key` makes a repeat
/// observation never a copy (Appendix B `mailbox.kind` is free-text — the
/// spellings are the spec's bullet names).
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
        dispatched_at: None,
    }
}
