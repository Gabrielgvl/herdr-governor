//! `outbox` — §4.8's linked-outbox resolution (B3's private copy; P5.C2
//! owns the shared helper this stands in for): the `WriteFollowUp::Resolve`
//! a terminated outbox-linked effect owes, plus the
//! `follow_up_unconfirmed` mailbox event on the `unconfirmed` outcome.

use governor_core::delivery::{
    FollowUpWrite, MailboxEvent, MailboxEventKind, MailboxSubject, OutboxState,
};
use governor_core::identity::{EffectKey, EventId, RunId};
use governor_core::lifecycle::{Effect, EffectOutcome, StateChange};

use crate::store::Store;

/// Parse `run:<id>:outbox:<seq>` — the F9/F17 journal-key convention.
/// `rsplit_once` keeps run ids containing `:` honest.
fn outbox_key(key: &EffectKey) -> Option<(RunId, u64)> {
    let rest = key.0.strip_prefix("run:")?;
    let (run, seq) = rest.rsplit_once(":outbox:")?;
    Some((RunId(run.to_owned()), seq.parse().ok()?))
}

/// For an outbox-linked effect whose `outcome` is terminal, the
/// `WriteFollowUp::Resolve` the same transaction owes plus — on
/// `unconfirmed` — the `follow_up_unconfirmed` event. The row's own
/// `resolve_dispatch` carries the state rule (a non-`dispatching` row
/// resolves to `None`, so a restart that finds the entry already
/// resolved writes nothing and emits nothing).
///
/// ponytail: B3's private copy — C2's shared `linked_outbox_resolution`
/// replaces it (the result path's caller lands with the runner).
pub(in crate::daemon) fn linked_outbox_resolution(
    store: &Store,
    effect: &Effect,
    outcome: EffectOutcome,
) -> Option<(StateChange, Option<MailboxEvent>)> {
    let (run, seq) = outbox_key(&effect.key)?;
    let resolved = store
        .outbox(&run)
        .ok()?
        .into_iter()
        .find(|message| message.seq == seq)?
        .resolve_dispatch(outcome)?;
    let write = StateChange::WriteFollowUp(FollowUpWrite::Resolve {
        run: run.clone(),
        seq,
        state: resolved.state,
    });
    let event = if resolved.state == OutboxState::Unconfirmed {
        let subject = MailboxSubject::Run(run);
        let dedup = MailboxEventKind::FollowUpUnconfirmed.dedup_key(&subject, Some(seq))?;
        MailboxEvent::emitted(
            EventId(format!("evt:{}", dedup.0)),
            subject,
            MailboxEventKind::FollowUpUnconfirmed,
            Some(seq),
            format!("{{\"seq\":{seq}}}"),
        )
    } else {
        None
    };
    Some((write, event))
}
