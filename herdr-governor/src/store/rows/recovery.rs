//! `recovery` — the `recoveries` row ↔ `RecoveryObligation`.
//! `created_at`/`updated_at` are store-stamped; the Appendix B CHECK
//! (`dispatched` ⇔ `successor_launch_id`) is restated on decode.

use rusqlite::Row;

use governor_core::identity::{LaunchId, RunId, Timestamp};
use governor_core::recovery::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};

use crate::store::error::StoreError;
use crate::store::rows::{Params, corrupt, enum_decode, read_col, ts_decode, ts_encode};

const TABLE: &str = "recoveries";

const ORIGINS: &[RecoveryOrigin] = &[RecoveryOrigin::ProviderLimit, RecoveryOrigin::Caller];

pub(in crate::store) const STATUSES: &[RecoveryStatus] = &[
    RecoveryStatus::Pending,
    RecoveryStatus::Blocked,
    RecoveryStatus::Dispatched,
    RecoveryStatus::Failed,
];

/// The `recoveries` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct RecoveryRow {
    predecessor_run_id: String,
    origin: String,
    state: String,
    reason: Option<String>,
    successor_launch_id: Option<String>,
    expires_at: String,
    created_at: String,
    updated_at: String,
}

impl RecoveryRow {
    /// Encode `obligation`; `now` stamps `created_at`/`updated_at`.
    pub(in crate::store) fn from_core(
        obligation: &RecoveryObligation,
        now: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            predecessor_run_id: obligation.predecessor.0.clone(),
            origin: obligation.origin.as_str().into(),
            state: obligation.status.as_str().into(),
            reason: obligation.reason.clone(),
            successor_launch_id: obligation.successor_launch.as_ref().map(|l| l.0.clone()),
            expires_at: ts_encode(obligation.expires_at, TABLE, "expires_at")?,
            created_at: ts_encode(now, TABLE, "created_at")?,
            updated_at: ts_encode(now, TABLE, "updated_at")?,
        })
    }

    /// The checked decode back to `RecoveryObligation`.
    pub(in crate::store) fn to_core(&self) -> Result<RecoveryObligation, StoreError> {
        let status = enum_decode(
            &self.state,
            TABLE,
            "state",
            STATUSES,
            RecoveryStatus::as_str,
        )?;
        if (status == RecoveryStatus::Dispatched) != self.successor_launch_id.is_some() {
            return Err(corrupt(
                TABLE,
                "successor_launch_id",
                "set exactly when the state is 'dispatched'",
            ));
        }
        Ok(RecoveryObligation {
            predecessor: RunId(self.predecessor_run_id.clone()),
            origin: enum_decode(
                &self.origin,
                TABLE,
                "origin",
                ORIGINS,
                RecoveryOrigin::as_str,
            )?,
            status,
            reason: self.reason.clone(),
            successor_launch: self.successor_launch_id.clone().map(LaunchId),
            expires_at: ts_decode(&self.expires_at, TABLE, "expires_at")?,
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("predecessor_run_id", self.predecessor_run_id.clone().into()),
            ("origin", self.origin.clone().into()),
            ("state", self.state.clone().into()),
            ("reason", self.reason.clone().into()),
            (
                "successor_launch_id",
                self.successor_launch_id.clone().into(),
            ),
            ("expires_at", self.expires_at.clone().into()),
            ("created_at", self.created_at.clone().into()),
            ("updated_at", self.updated_at.clone().into()),
        ]
    }

    /// Pull the row's columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            predecessor_run_id: read_col(row, TABLE, "predecessor_run_id")?,
            origin: read_col(row, TABLE, "origin")?,
            state: read_col(row, TABLE, "state")?,
            reason: read_col(row, TABLE, "reason")?,
            successor_launch_id: read_col(row, TABLE, "successor_launch_id")?,
            expires_at: read_col(row, TABLE, "expires_at")?,
            created_at: read_col(row, TABLE, "created_at")?,
            updated_at: read_col(row, TABLE, "updated_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{RecoveryRow, STATUSES};
    use crate::store::StoreError;
    use crate::store::rows::tests::{NOW, poison, via_sqlite};
    use governor_core::identity::{LaunchId, RunId, Timestamp};
    use governor_core::recovery::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};

    fn obligation(status: RecoveryStatus) -> RecoveryObligation {
        RecoveryObligation {
            predecessor: RunId("r-1".into()),
            origin: RecoveryOrigin::ProviderLimit,
            status,
            reason: (status == RecoveryStatus::Failed).then(|| "expired".to_owned()),
            successor_launch: (status == RecoveryStatus::Dispatched)
                .then(|| LaunchId("l-2".into())),
            expires_at: Timestamp(NOW.0 + 1000),
        }
    }

    #[test]
    fn recovery_round_trips_every_status() {
        for status in STATUSES {
            let obligation = obligation(*status);
            let row = RecoveryRow::from_core(&obligation, NOW).unwrap();
            let back = via_sqlite(&row.params(), RecoveryRow::read).unwrap();
            assert_eq!(back.to_core().unwrap(), obligation, "recoveries round-trip");
        }
    }

    #[test]
    fn recovery_restates_the_dispatched_check() {
        let mut params = RecoveryRow::from_core(&obligation(RecoveryStatus::Pending), NOW)
            .unwrap()
            .params();
        poison(&mut params, "successor_launch_id", "l-2");
        let err = via_sqlite(&params, RecoveryRow::read)
            .unwrap()
            .to_core()
            .unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::CorruptRow {
                    column: "successor_launch_id",
                    ..
                }
            ),
            "{err}"
        );
    }
}
