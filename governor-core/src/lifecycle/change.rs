//! §9 — the transition function's output: `StateChange`, one row-group write
//! of the Appendix B transaction, and `Transition`, everything the pure
//! function returns for `store::apply` to commit as one transaction.

use alloc::vec::Vec;

use crate::acceptance::FrozenHandoff;
use crate::delivery::{ExpiryReason, MailboxEvent, OutboxMessage};
use crate::identity::{CallerBinding, EventId, RunId};
use crate::recovery::{Cooldown, RecoveryObligation};
use crate::task::Launch;

use super::{Effect, EffectWrite, OwnerChange, Run, RunUpdate};

/// §9/Appendix B — one row-group write inside a `Transition`'s transaction;
/// each variant names the write set its transaction performs.
#[expect(
    clippy::large_enum_variant,
    reason = "variants carry whole spec-shaped records (Launch, Run); boxing would distort the shared vocabulary"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum StateChange {
    /// F1 — persist the caller binding: the `callers` row when the key is new
    /// plus the `relay_bindings` row, in one transaction.
    BindCaller(CallerBinding),
    /// F11 — write the Launch row: admission, the persisted routing decision,
    /// or the outcome (`routed`/`done` phase writes upsert the same row).
    RecordLaunch(Launch),
    /// F13 — create the reserved Run with its `max_age_deadline`, in the
    /// routing transaction.
    ReserveRun(Run),
    /// Appendix B — the conditional run write behind "Effect result" and
    /// "Settle" (and every other row write).
    UpdateRun(RunUpdate),
    /// F4 — a `handover`/`adopt` owner change, conditional on the expected
    /// owner.
    ChangeOwner(OwnerChange),
    /// F8 — a journal-row write: the dispatch commit or the result commit.
    WriteEffect(EffectWrite),
    /// F17 — write an outbox row (enqueue, dispatch bookkeeping,
    /// resolution).
    RecordFollowUp(OutboxMessage),
    /// F20 — expire the Run's still-`queued` follow-ups in the settle
    /// transaction; dispatched ones are untouched (they stay visible in their
    /// last state).
    ExpireFollowUps {
        /// The Run whose queue expires.
        run: RunId,
        /// The recorded expiry reason.
        reason: ExpiryReason,
    },
    /// F21 — write the recovery obligation (record, dispatch, block, fail).
    RecordRecovery(RecoveryObligation),
    /// F21 — upsert the provider cooldown; it only ever lengthens.
    SetCooldown(Cooldown),
    /// F24 — write the frozen handoff row.
    FreezeHandoff(FrozenHandoff),
    /// F6/F18 — mark a mailbox event acknowledged; `ack` is idempotent.
    AckEvent(EventId),
}

/// §9 — everything a transition returns: the pure function's whole output,
/// committed as one `store::apply` transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// The row writes — the Appendix B transaction's write set, in apply
    /// order.
    pub state_changes: Vec<StateChange>,
    /// The mailbox events to emit (F18; dedup keys make repeats no-ops).
    pub events: Vec<MailboxEvent>,
    /// The effects to plan — journaled `planned`, dispatched under the F8
    /// protocol.
    pub effects: Vec<Effect>,
}
