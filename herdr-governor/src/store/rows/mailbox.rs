//! `mailbox` — the `mailbox` row ↔ `MailboxEvent`. The subject is exactly
//! one of `run_id`/`launch_id` (the enum permits one; the Appendix B CHECK
//! requires at least one — both set is foreign data). `kind` is bare TEXT in
//! the schema, so its decode is the checked `as_str()` parse like any enum
//! column. `body_json` is stored verbatim; `acked_at`/`created_at` are
//! store-stamped.

use rusqlite::Row;

use governor_core::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use governor_core::identity::{DedupKey, EventId, LaunchId, RunId, Timestamp};

use crate::store::error::StoreError;
use crate::store::rows::{Params, corrupt, enum_decode, read_col, ts_encode, ts_opt_encode};

const TABLE: &str = "mailbox";

const KINDS: &[MailboxEventKind] = &[
    MailboxEventKind::HandoffAccepted,
    MailboxEventKind::HandoffRejected,
    MailboxEventKind::Settled,
    MailboxEventKind::Stalled,
    MailboxEventKind::BlockedOnInput,
    MailboxEventKind::OutsideScope,
    MailboxEventKind::LaunchFailed,
    MailboxEventKind::LaunchAnswered,
    MailboxEventKind::PromptUnconfirmed,
    MailboxEventKind::FollowUpUnconfirmed,
    MailboxEventKind::FollowUpExpired,
    MailboxEventKind::CooldownHit,
    MailboxEventKind::RecoveryPending,
    MailboxEventKind::RecoveryBlocked,
    MailboxEventKind::RecoveryDispatched,
];

/// The `mailbox` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct MailboxRow {
    event_id: String,
    dedup_key: String,
    launch_id: Option<String>,
    run_id: Option<String>,
    kind: String,
    body_json: String,
    acked_at: Option<String>,
    created_at: String,
}

impl MailboxRow {
    /// Encode `event`; the stamps are the writer's.
    pub(in crate::store) fn from_core(
        event: &MailboxEvent,
        created_at: Timestamp,
        acked_at: Option<Timestamp>,
    ) -> Result<Self, StoreError> {
        let (launch_id, run_id) = match &event.subject {
            MailboxSubject::Run(run) => (None, Some(run.0.clone())),
            MailboxSubject::Launch(launch) => (Some(launch.0.clone()), None),
        };
        Ok(Self {
            event_id: event.id.0.clone(),
            dedup_key: event.dedup_key.0.clone(),
            launch_id,
            run_id,
            kind: event.kind.as_str().into(),
            body_json: event.body.clone(),
            acked_at: ts_opt_encode(acked_at, TABLE, "acked_at")?,
            created_at: ts_encode(created_at, TABLE, "created_at")?,
        })
    }

    /// The checked decode back to `MailboxEvent`.
    pub(in crate::store) fn to_core(&self) -> Result<MailboxEvent, StoreError> {
        let subject = match (&self.launch_id, &self.run_id) {
            (None, Some(run)) => MailboxSubject::Run(RunId(run.clone())),
            (Some(launch), None) => MailboxSubject::Launch(LaunchId(launch.clone())),
            _ => {
                return Err(corrupt(
                    TABLE,
                    "run_id",
                    "an event binds exactly one subject",
                ));
            }
        };
        Ok(MailboxEvent {
            id: EventId(self.event_id.clone()),
            dedup_key: DedupKey(self.dedup_key.clone()),
            subject,
            kind: enum_decode(&self.kind, TABLE, "kind", KINDS, MailboxEventKind::as_str)?,
            body: self.body_json.clone(),
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("event_id", self.event_id.clone().into()),
            ("dedup_key", self.dedup_key.clone().into()),
            ("launch_id", self.launch_id.clone().into()),
            ("run_id", self.run_id.clone().into()),
            ("kind", self.kind.clone().into()),
            ("body_json", self.body_json.clone().into()),
            ("acked_at", self.acked_at.clone().into()),
            ("created_at", self.created_at.clone().into()),
        ]
    }

    /// Pull the row's columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            event_id: read_col(row, TABLE, "event_id")?,
            dedup_key: read_col(row, TABLE, "dedup_key")?,
            launch_id: read_col(row, TABLE, "launch_id")?,
            run_id: read_col(row, TABLE, "run_id")?,
            kind: read_col(row, TABLE, "kind")?,
            body_json: read_col(row, TABLE, "body_json")?,
            acked_at: read_col(row, TABLE, "acked_at")?,
            created_at: read_col(row, TABLE, "created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{KINDS, MailboxRow};
    use crate::store::StoreError;
    use crate::store::rows::tests::{NOW, poison, via_sqlite};
    use governor_core::delivery::{MailboxEvent, MailboxSubject};
    use governor_core::identity::{EventId, LaunchId, RunId, Timestamp};

    #[test]
    fn mailbox_round_trips_every_kind_and_subject() {
        for kind in KINDS {
            let subject = if kind.binds_launch() {
                MailboxSubject::Launch(LaunchId("l-1".into()))
            } else {
                MailboxSubject::Run(RunId("r-1".into()))
            };
            // Recurrent kinds need a qualifier, one-shot kinds refuse one.
            let qualifier = kind.dedup_key(&subject, Some(4)).map(|_| 4);
            let event = MailboxEvent::emitted(
                EventId("ev-1".into()),
                subject,
                *kind,
                qualifier,
                r#"{"k":1}"#.into(),
            )
            .unwrap();
            let row = MailboxRow::from_core(&event, NOW, Some(Timestamp(NOW.0 + 1))).unwrap();
            let back = via_sqlite(&row.params(), MailboxRow::read).unwrap();
            assert_eq!(
                back.to_core().unwrap(),
                event,
                "mailbox round-trip of {kind:?}"
            );
        }
    }

    #[test]
    fn mailbox_unknown_kind_and_double_subject_are_corrupt() {
        let event = MailboxEvent::emitted(
            EventId("ev-1".into()),
            MailboxSubject::Run(RunId("r-1".into())),
            governor_core::delivery::MailboxEventKind::Settled,
            None,
            "{}".into(),
        )
        .unwrap();
        let mut params = MailboxRow::from_core(&event, NOW, None).unwrap().params();
        poison(&mut params, "kind", "shouted");
        let err = via_sqlite(&params, MailboxRow::read)
            .unwrap()
            .to_core()
            .unwrap_err();
        assert!(
            matches!(err, StoreError::CorruptRow { column: "kind", .. }),
            "{err}"
        );
        let mut again = MailboxRow::from_core(&event, NOW, None).unwrap().params();
        poison(&mut again, "launch_id", "l-1");
        let second = via_sqlite(&again, MailboxRow::read)
            .unwrap()
            .to_core()
            .unwrap_err();
        assert!(
            matches!(
                second,
                StoreError::CorruptRow {
                    column: "run_id",
                    ..
                }
            ),
            "{second}"
        );
    }
}
