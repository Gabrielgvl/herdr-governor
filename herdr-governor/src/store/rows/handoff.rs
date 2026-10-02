//! `handoff` — the `handoffs` row ↔ `FrozenHandoff`. `assessed` is not a
//! stored column: the read derives it (an `answered` acceptance
//! `judgment_sets` row for the same run, work generation and digest) under
//! the `assessed` alias, and `params()` never emits it.

use rusqlite::Row;

use governor_core::acceptance::FrozenHandoff;
use governor_core::identity::RunId;

use crate::store::error::StoreError;
use crate::store::rows::{
    Params, hex_decode, hex_encode, i64_to_u64, read_col, ts_decode, ts_encode, u64_to_col,
};

const TABLE: &str = "handoffs";

/// The `handoffs` row plus the derived `assessed` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct HandoffRow {
    run_id: String,
    work_generation: i64,
    digest: String,
    frozen_path: String,
    frozen_at: String,
    assessed: bool,
}

impl HandoffRow {
    /// Encode `handoff` (every field is a core field here).
    pub(in crate::store) fn from_core(handoff: &FrozenHandoff) -> Result<Self, StoreError> {
        Ok(Self {
            run_id: handoff.run.0.clone(),
            work_generation: u64_to_col(handoff.work_generation, TABLE, "work_generation")?,
            digest: hex_encode(handoff.digest),
            frozen_path: handoff.frozen_path.clone(),
            frozen_at: ts_encode(handoff.frozen_at, TABLE, "frozen_at")?,
            assessed: handoff.assessed,
        })
    }

    /// The checked decode back to `FrozenHandoff`.
    pub(in crate::store) fn to_core(&self) -> Result<FrozenHandoff, StoreError> {
        Ok(FrozenHandoff {
            run: RunId(self.run_id.clone()),
            work_generation: i64_to_u64(self.work_generation, TABLE, "work_generation")?,
            digest: hex_decode(&self.digest, TABLE, "digest")?,
            frozen_path: self.frozen_path.clone(),
            frozen_at: ts_decode(&self.frozen_at, TABLE, "frozen_at")?,
            assessed: self.assessed,
        })
    }

    /// The stored columns as bindable `(column, value)` pairs — `assessed`
    /// is derived, never bound.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("run_id", self.run_id.clone().into()),
            ("work_generation", self.work_generation.into()),
            ("digest", self.digest.clone().into()),
            ("frozen_path", self.frozen_path.clone().into()),
            ("frozen_at", self.frozen_at.clone().into()),
        ]
    }

    /// Pull the row's columns plus the query's `assessed` alias out of a
    /// query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            run_id: read_col(row, TABLE, "run_id")?,
            work_generation: read_col(row, TABLE, "work_generation")?,
            digest: read_col(row, TABLE, "digest")?,
            frozen_path: read_col(row, TABLE, "frozen_path")?,
            frozen_at: read_col(row, TABLE, "frozen_at")?,
            assessed: read_col(row, TABLE, "assessed")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HandoffRow;
    use crate::store::rows::tests::{NOW, digest, via_sqlite};
    use governor_core::acceptance::FrozenHandoff;
    use governor_core::identity::RunId;

    #[test]
    fn handoff_round_trips_with_the_derived_assessed_flag() {
        for assessed in [false, true] {
            let handoff = FrozenHandoff {
                run: RunId("r-1".into()),
                work_generation: 2,
                digest: digest(0x55),
                frozen_path: "/frozen/h".into(),
                frozen_at: NOW,
                assessed,
            };
            let row = HandoffRow::from_core(&handoff).unwrap();
            let mut params = row.params();
            assert!(
                params.iter().all(|(name, _)| *name != "assessed"),
                "assessed is derived, never bound"
            );
            params.push(("assessed", assessed.into()));
            let back = via_sqlite(&params, HandoffRow::read).unwrap();
            assert_eq!(back.to_core().unwrap(), handoff, "handoffs round-trip");
        }
    }
}
