//! `cooldown` — the `cooldowns` row ↔ `Cooldown`. `updated_at` is
//! store-stamped; the never-shortened `until` rule belongs to the writer.

use rusqlite::Row;

use governor_core::config::Provider;
use governor_core::identity::{RunId, Timestamp};
use governor_core::recovery::Cooldown;

use crate::store::error::StoreError;
use crate::store::rows::{Params, read_col, ts_decode, ts_encode};

const TABLE: &str = "cooldowns";

/// The `cooldowns` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct CooldownRow {
    provider: String,
    until: String,
    reason: String,
    source_run_id: Option<String>,
    updated_at: String,
}

impl CooldownRow {
    /// Encode `cooldown`; `now` stamps `updated_at`.
    pub(in crate::store) fn from_core(
        cooldown: &Cooldown,
        now: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            provider: cooldown.provider.0.clone(),
            until: ts_encode(cooldown.until, TABLE, "until")?,
            reason: cooldown.reason.clone(),
            source_run_id: cooldown.source_run.as_ref().map(|r| r.0.clone()),
            updated_at: ts_encode(now, TABLE, "updated_at")?,
        })
    }

    /// The checked decode back to `Cooldown`.
    pub(in crate::store) fn to_core(&self) -> Result<Cooldown, StoreError> {
        Ok(Cooldown {
            provider: Provider(self.provider.clone()),
            until: ts_decode(&self.until, TABLE, "until")?,
            reason: self.reason.clone(),
            source_run: self.source_run_id.clone().map(RunId),
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("provider", self.provider.clone().into()),
            ("until", self.until.clone().into()),
            ("reason", self.reason.clone().into()),
            ("source_run_id", self.source_run_id.clone().into()),
            ("updated_at", self.updated_at.clone().into()),
        ]
    }

    /// Pull the row's columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            provider: read_col(row, TABLE, "provider")?,
            until: read_col(row, TABLE, "until")?,
            reason: read_col(row, TABLE, "reason")?,
            source_run_id: read_col(row, TABLE, "source_run_id")?,
            updated_at: read_col(row, TABLE, "updated_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CooldownRow;
    use crate::store::rows::tests::{NOW, via_sqlite};
    use governor_core::config::Provider;
    use governor_core::identity::{RunId, Timestamp};
    use governor_core::recovery::Cooldown;

    #[test]
    fn cooldown_round_trips_with_and_without_source() {
        for source_run in [None, Some(RunId("r-1".into()))] {
            let cooldown = Cooldown {
                provider: Provider("prov".into()),
                until: Timestamp(NOW.0 + 60_000),
                reason: "provider_limited".into(),
                source_run,
            };
            let row = CooldownRow::from_core(&cooldown, NOW).unwrap();
            let back = via_sqlite(&row.params(), CooldownRow::read).unwrap();
            assert_eq!(back.to_core().unwrap(), cooldown, "cooldowns round-trip");
        }
    }
}
