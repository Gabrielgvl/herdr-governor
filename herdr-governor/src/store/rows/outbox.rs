//! `outbox` — the `outbox` row ↔ `OutboxMessage`. `sender_caller_id` is the
//! surrogate the `callers` join resolves; `to_core` takes the `CallerKey`
//! back. `body_inline`/`body_path` are the XOR pair behind `MessageBody`;
//! `enqueued_at`/`finished_at` are store-stamped.

use rusqlite::Row;

use governor_core::delivery::{ExpiryReason, MessageBody, OutboxMessage, OutboxState};
use governor_core::identity::{CallerKey, EffectId, MessageKey, RunId, Timestamp};

use crate::store::error::StoreError;
use crate::store::rows::{
    Params, corrupt, enum_decode, enum_opt_decode, hex_decode, hex_encode, i64_to_u64, read_col,
    ts_encode, ts_opt_encode, u64_to_col,
};

const TABLE: &str = "outbox";

pub(in crate::store) const STATES: &[OutboxState] = &[
    OutboxState::Queued,
    OutboxState::Dispatching,
    OutboxState::Submitted,
    OutboxState::Unconfirmed,
    OutboxState::Expired,
];

const EXPIRY_REASONS: &[ExpiryReason] = &[ExpiryReason::RunSettled];

/// The `outbox` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct OutboxRow {
    run_id: String,
    seq: i64,
    message_key: String,
    sender_caller_id: i64,
    body_digest: String,
    body_inline: Option<String>,
    body_path: Option<String>,
    state: String,
    effect_id: Option<String>,
    expiry_reason: Option<String>,
    enqueued_at: String,
    finished_at: Option<String>,
}

impl OutboxRow {
    /// Encode `message`; `sender` is the surrogate id the writer resolved,
    /// the stamps are the writer's.
    pub(in crate::store) fn from_core(
        message: &OutboxMessage,
        sender: i64,
        enqueued_at: Timestamp,
        finished_at: Option<Timestamp>,
    ) -> Result<Self, StoreError> {
        let (body_inline, body_path) = match &message.body {
            MessageBody::Inline(text) => (Some(text.clone()), None),
            MessageBody::File { path } => (None, Some(path.clone())),
        };
        Ok(Self {
            run_id: message.run.0.clone(),
            seq: u64_to_col(message.seq, TABLE, "seq")?,
            message_key: message.message_key.0.clone(),
            sender_caller_id: sender,
            body_digest: hex_encode(message.body_digest),
            body_inline,
            body_path,
            state: message.state.as_str().into(),
            effect_id: message.effect.as_ref().map(|e| e.0.clone()),
            expiry_reason: message.expiry_reason.map(|r| r.as_str().into()),
            enqueued_at: ts_encode(enqueued_at, TABLE, "enqueued_at")?,
            finished_at: ts_opt_encode(finished_at, TABLE, "finished_at")?,
        })
    }

    /// The checked decode back to `OutboxMessage`; `sender` is the key the
    /// read's `callers` join selected. The Appendix B CHECK constraints are restated:
    /// exactly one body form, and `expired` ⇒ no effect and a reason.
    pub(in crate::store) fn to_core(&self, sender: CallerKey) -> Result<OutboxMessage, StoreError> {
        let body = match (&self.body_inline, &self.body_path) {
            (Some(text), None) => MessageBody::Inline(text.clone()),
            (None, Some(path)) => MessageBody::File { path: path.clone() },
            _ => {
                return Err(corrupt(
                    TABLE,
                    "body_inline",
                    "exactly one body form is required",
                ));
            }
        };
        let state = enum_decode(&self.state, TABLE, "state", STATES, OutboxState::as_str)?;
        let expiry_reason = enum_opt_decode(
            self.expiry_reason.as_deref(),
            TABLE,
            "expiry_reason",
            EXPIRY_REASONS,
            ExpiryReason::as_str,
        )?;
        if state == OutboxState::Expired && (self.effect_id.is_some() || expiry_reason.is_none()) {
            return Err(corrupt(
                TABLE,
                "expiry_reason",
                "'expired' needs a reason and no effect",
            ));
        }
        Ok(OutboxMessage {
            run: RunId(self.run_id.clone()),
            seq: i64_to_u64(self.seq, TABLE, "seq")?,
            message_key: MessageKey(self.message_key.clone()),
            sender,
            body_digest: hex_decode(&self.body_digest, TABLE, "body_digest")?,
            body,
            state,
            effect: self.effect_id.clone().map(EffectId),
            expiry_reason,
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("run_id", self.run_id.clone().into()),
            ("seq", self.seq.into()),
            ("message_key", self.message_key.clone().into()),
            ("sender_caller_id", self.sender_caller_id.into()),
            ("body_digest", self.body_digest.clone().into()),
            ("body_inline", self.body_inline.clone().into()),
            ("body_path", self.body_path.clone().into()),
            ("state", self.state.clone().into()),
            ("effect_id", self.effect_id.clone().into()),
            ("expiry_reason", self.expiry_reason.clone().into()),
            ("enqueued_at", self.enqueued_at.clone().into()),
            ("finished_at", self.finished_at.clone().into()),
        ]
    }

    /// Pull the row's own columns out of a query row (the joined sender
    /// columns go through `rows::caller::key_from_row`).
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            run_id: read_col(row, TABLE, "run_id")?,
            seq: read_col(row, TABLE, "seq")?,
            message_key: read_col(row, TABLE, "message_key")?,
            sender_caller_id: read_col(row, TABLE, "sender_caller_id")?,
            body_digest: read_col(row, TABLE, "body_digest")?,
            body_inline: read_col(row, TABLE, "body_inline")?,
            body_path: read_col(row, TABLE, "body_path")?,
            state: read_col(row, TABLE, "state")?,
            effect_id: read_col(row, TABLE, "effect_id")?,
            expiry_reason: read_col(row, TABLE, "expiry_reason")?,
            enqueued_at: read_col(row, TABLE, "enqueued_at")?,
            finished_at: read_col(row, TABLE, "finished_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::OutboxRow;
    use crate::store::StoreError;
    use crate::store::rows::tests::{NOW, caller, digest, poison, via_sqlite};
    use governor_core::delivery::{ExpiryReason, MessageBody, OutboxMessage, OutboxState};
    use governor_core::identity::{EffectId, MessageKey, RunId};

    fn message(body: MessageBody, state: OutboxState) -> OutboxMessage {
        let expired = state == OutboxState::Expired;
        OutboxMessage {
            run: RunId("r-1".into()),
            seq: 3,
            message_key: MessageKey("m-1".into()),
            sender: caller(),
            body_digest: digest(0x44),
            body,
            state,
            effect: (!expired && state != OutboxState::Queued).then(|| EffectId("e-1".into())),
            expiry_reason: expired.then_some(ExpiryReason::RunSettled),
        }
    }

    #[test]
    fn outbox_round_trips_both_bodies_and_states() {
        for body in [
            MessageBody::Inline("hello".into()),
            MessageBody::File {
                path: "/var/f".into(),
            },
        ] {
            for state in super::STATES {
                let message = message(body.clone(), *state);
                let row = OutboxRow::from_core(&message, 7, NOW, Some(NOW)).unwrap();
                let back = via_sqlite(&row.params(), OutboxRow::read).unwrap();
                assert_eq!(
                    back.to_core(caller()).unwrap(),
                    message,
                    "outbox round-trip"
                );
            }
        }
    }

    #[test]
    fn outbox_restates_the_expired_and_body_checks() {
        let message = message(MessageBody::Inline("hello".into()), OutboxState::Expired);
        let mut params = OutboxRow::from_core(&message, 7, NOW, None)
            .unwrap()
            .params();
        poison(&mut params, "expiry_reason", "later");
        let err = via_sqlite(&params, OutboxRow::read)
            .unwrap()
            .to_core(caller())
            .unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::CorruptRow {
                    column: "expiry_reason",
                    ..
                }
            ),
            "{err}"
        );
        let mut again = OutboxRow::from_core(&message, 7, NOW, None)
            .unwrap()
            .params();
        poison(&mut again, "body_path", "/also");
        let second = via_sqlite(&again, OutboxRow::read)
            .unwrap()
            .to_core(caller())
            .unwrap_err();
        assert!(
            matches!(
                second,
                StoreError::CorruptRow {
                    column: "body_inline",
                    ..
                }
            ),
            "{second}"
        );
    }
}
