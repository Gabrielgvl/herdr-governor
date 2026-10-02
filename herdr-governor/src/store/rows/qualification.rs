//! `qualification` — the `qualifications` row ↔ `Qualification`.
//! `passed` is the `0`/`1` INTEGER the schema CHECK constraint allows; `evidence_json` is
//! stored verbatim; `qualified_at` is store-stamped.

use rusqlite::Row;

use governor_core::config::{Capability, OperatingPointId, Qualification};
use governor_core::identity::Timestamp;

use crate::store::error::StoreError;
use crate::store::rows::{Params, corrupt, hex_decode, hex_encode, read_col, ts_encode};

const TABLE: &str = "qualifications";

/// The `qualifications` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct QualificationRow {
    operating_point_id: String,
    args_digest: String,
    capability: String,
    passed: i64,
    evidence_json: String,
    qualified_at: String,
}

impl QualificationRow {
    /// Encode `qualification`; `qualified_at` is the writer's stamp.
    pub(in crate::store) fn from_core(
        qualification: &Qualification,
        qualified_at: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            operating_point_id: qualification.operating_point.0.clone(),
            args_digest: hex_encode(qualification.args_digest),
            capability: qualification.capability.0.clone(),
            passed: i64::from(qualification.passed),
            evidence_json: qualification.evidence.clone(),
            qualified_at: ts_encode(qualified_at, TABLE, "qualified_at")?,
        })
    }

    /// The checked decode back to `Qualification`; `passed` outside `{0,1}`
    /// is corrupt.
    pub(in crate::store) fn to_core(&self) -> Result<Qualification, StoreError> {
        let passed = match self.passed {
            0 => false,
            1 => true,
            other => return Err(corrupt(TABLE, "passed", format!("{other} is not 0 or 1"))),
        };
        Ok(Qualification {
            operating_point: OperatingPointId(self.operating_point_id.clone()),
            args_digest: hex_decode(&self.args_digest, TABLE, "args_digest")?,
            capability: Capability(self.capability.clone()),
            passed,
            evidence: self.evidence_json.clone(),
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("operating_point_id", self.operating_point_id.clone().into()),
            ("args_digest", self.args_digest.clone().into()),
            ("capability", self.capability.clone().into()),
            ("passed", self.passed.into()),
            ("evidence_json", self.evidence_json.clone().into()),
            ("qualified_at", self.qualified_at.clone().into()),
        ]
    }

    /// Pull the row's columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            operating_point_id: read_col(row, TABLE, "operating_point_id")?,
            args_digest: read_col(row, TABLE, "args_digest")?,
            capability: read_col(row, TABLE, "capability")?,
            passed: read_col(row, TABLE, "passed")?,
            evidence_json: read_col(row, TABLE, "evidence_json")?,
            qualified_at: read_col(row, TABLE, "qualified_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::QualificationRow;
    use crate::store::StoreError;
    use crate::store::rows::tests::{NOW, digest, via_sqlite};
    use governor_core::config::{Capability, OperatingPointId, Qualification};

    fn qualification(passed: bool) -> Qualification {
        Qualification {
            operating_point: OperatingPointId("op-1".into()),
            args_digest: digest(0x66),
            capability: Capability(Capability::PROMPT_ACK.into()),
            passed,
            evidence: r#"{"ok":true}"#.into(),
        }
    }

    #[test]
    fn qualification_round_trips_pass_and_fail() {
        for passed in [true, false] {
            let row = QualificationRow::from_core(&qualification(passed), NOW).unwrap();
            let back = via_sqlite(&row.params(), QualificationRow::read).unwrap();
            assert_eq!(
                back.to_core().unwrap(),
                qualification(passed),
                "qualifications round-trip"
            );
        }
        let mut params = QualificationRow::from_core(&qualification(true), NOW)
            .unwrap()
            .params();
        params
            .iter_mut()
            .find(|(name, _)| *name == "passed")
            .unwrap()
            .1 = rusqlite::types::Value::Integer(2);
        let err = via_sqlite(&params, QualificationRow::read)
            .unwrap()
            .to_core()
            .unwrap_err();
        assert!(matches!(err, StoreError::CorruptRow { .. }), "{err}");
    }
}
