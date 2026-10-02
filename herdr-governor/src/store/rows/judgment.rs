//! `judgment` — the `judgment_sets`/`judgments` rows ↔ `JudgmentSet` /
//! `Judgment`, plus the same pair as the JSON payload an
//! `EffectReceipt::Judgments` embeds in `effects.result_json`. The set's
//! `(run_version, work_generation, evidence_generation)` version triple is
//! one group: all present or all absent. `question` stores
//! `handoff_meets_item_<item>` for the per-item kind (F24); every other
//! question stores `Question::as_str()`.

use std::collections::BTreeMap;

use rusqlite::Row;
use serde_json::{Value, json};

use governor_core::config::ConfigVersion;
use governor_core::identity::{JudgmentSetId, LaunchId, RunId, Timestamp};
use governor_core::lifecycle::VersionTriple;
use governor_core::routing::{
    Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability, Question,
    QuestionVersion,
};

use crate::store::error::StoreError;
use crate::store::rows::{
    Params, corrupt, enum_decode, hex_decode, hex_encode, hex_opt_decode, i64_to_u64, json_parse,
    json_write, member, member_opt_str, member_str, read_col, ts_encode, ts_opt_encode, u64_to_col,
};

const TABLE: &str = "judgment_sets";
const ROWS_TABLE: &str = "judgments";

const PURPOSES: &[JudgmentPurpose] = &[
    JudgmentPurpose::Launch,
    JudgmentPurpose::Review,
    JudgmentPurpose::Acceptance,
    JudgmentPurpose::ProviderLimit,
];

const OUTCOMES: &[JudgmentOutcome] = &[
    JudgmentOutcome::Answered,
    JudgmentOutcome::TransportFailed,
    JudgmentOutcome::AuthFailed,
    JudgmentOutcome::InvalidResponse,
    JudgmentOutcome::TooLarge,
    JudgmentOutcome::Stale,
];

const FIXED_QUESTIONS: &[Question] = &[
    Question::DoneWhenVerifiable,
    Question::WeakestSufficientTier,
    Question::ChangesFiles,
    Question::SecurityBoundary,
    Question::NeedsExternal,
    Question::LongRunning,
    Question::RelatedTab,
    Question::BlockedOnInput,
    Question::NoRecentProgress,
    Question::OutsideScope,
    Question::ProviderLimited,
];

/// The `judgment_sets` row. `requested_at`/`answered_at` are store-stamped
/// — `JudgmentSet` carries neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct JudgmentSetRow {
    set_id: String,
    purpose: String,
    launch_id: Option<String>,
    run_id: Option<String>,
    run_version: Option<i64>,
    work_generation: Option<i64>,
    evidence_generation: Option<i64>,
    task_digest: String,
    handoff_digest: Option<String>,
    evidence_digest: Option<String>,
    model: String,
    question_version: String,
    policy_version: String,
    outcome: String,
    requested_at: String,
    answered_at: Option<String>,
}

impl JudgmentSetRow {
    /// Encode `set`; the writer supplies both stamps (`answered_at` is
    /// `Some` when the response completed the request).
    pub(in crate::store) fn from_core(
        set: &JudgmentSet,
        requested_at: Timestamp,
        answered_at: Option<Timestamp>,
    ) -> Result<Self, StoreError> {
        let (run_version, work_generation, evidence_generation) = match set.versions {
            Some(v) => (
                Some(u64_to_col(v.version, TABLE, "run_version")?),
                Some(u64_to_col(v.work_generation, TABLE, "work_generation")?),
                Some(u64_to_col(
                    v.evidence_generation,
                    TABLE,
                    "evidence_generation",
                )?),
            ),
            None => (None, None, None),
        };
        Ok(Self {
            set_id: set.id.0.clone(),
            purpose: set.purpose.as_str().into(),
            launch_id: set.launch.as_ref().map(|l| l.0.clone()),
            run_id: set.run.as_ref().map(|r| r.0.clone()),
            run_version,
            work_generation,
            evidence_generation,
            task_digest: hex_encode(set.task_digest),
            handoff_digest: set.handoff_digest.map(hex_encode),
            evidence_digest: set.evidence_digest.map(hex_encode),
            model: set.model.clone(),
            question_version: set.question_version.0.clone(),
            policy_version: set.policy_version.0.clone(),
            outcome: set.outcome.as_str().into(),
            requested_at: ts_encode(requested_at, TABLE, "requested_at")?,
            answered_at: ts_opt_encode(answered_at, TABLE, "answered_at")?,
        })
    }

    /// The checked decode back to `JudgmentSet`.
    pub(in crate::store) fn to_core(&self) -> Result<JudgmentSet, StoreError> {
        let versions = match (
            self.run_version,
            self.work_generation,
            self.evidence_generation,
        ) {
            (Some(version), Some(work), Some(evidence)) => Some(VersionTriple {
                version: i64_to_u64(version, TABLE, "run_version")?,
                work_generation: i64_to_u64(work, TABLE, "work_generation")?,
                evidence_generation: i64_to_u64(evidence, TABLE, "evidence_generation")?,
            }),
            (None, None, None) => None,
            _ => {
                return Err(corrupt(
                    TABLE,
                    "run_version",
                    "the version triple is all-or-none",
                ));
            }
        };
        if self.launch_id.is_none() && self.run_id.is_none() {
            return Err(corrupt(TABLE, "run_id", "a set binds a launch or a run"));
        }
        Ok(JudgmentSet {
            id: JudgmentSetId(self.set_id.clone()),
            purpose: enum_decode(
                &self.purpose,
                TABLE,
                "purpose",
                PURPOSES,
                JudgmentPurpose::as_str,
            )?,
            launch: self.launch_id.clone().map(LaunchId),
            run: self.run_id.clone().map(RunId),
            versions,
            task_digest: hex_decode(&self.task_digest, TABLE, "task_digest")?,
            handoff_digest: hex_opt_decode(
                self.handoff_digest.as_deref(),
                TABLE,
                "handoff_digest",
            )?,
            evidence_digest: hex_opt_decode(
                self.evidence_digest.as_deref(),
                TABLE,
                "evidence_digest",
            )?,
            model: self.model.clone(),
            question_version: QuestionVersion(self.question_version.clone()),
            policy_version: ConfigVersion(self.policy_version.clone()),
            outcome: enum_decode(
                &self.outcome,
                TABLE,
                "outcome",
                OUTCOMES,
                JudgmentOutcome::as_str,
            )?,
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("set_id", self.set_id.clone().into()),
            ("purpose", self.purpose.clone().into()),
            ("launch_id", self.launch_id.clone().into()),
            ("run_id", self.run_id.clone().into()),
            ("run_version", self.run_version.into()),
            ("work_generation", self.work_generation.into()),
            ("evidence_generation", self.evidence_generation.into()),
            ("task_digest", self.task_digest.clone().into()),
            ("handoff_digest", self.handoff_digest.clone().into()),
            ("evidence_digest", self.evidence_digest.clone().into()),
            ("model", self.model.clone().into()),
            ("question_version", self.question_version.clone().into()),
            ("policy_version", self.policy_version.clone().into()),
            ("outcome", self.outcome.clone().into()),
            ("requested_at", self.requested_at.clone().into()),
            ("answered_at", self.answered_at.clone().into()),
        ]
    }

    /// Pull the row's own columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            set_id: read_col(row, TABLE, "set_id")?,
            purpose: read_col(row, TABLE, "purpose")?,
            launch_id: read_col(row, TABLE, "launch_id")?,
            run_id: read_col(row, TABLE, "run_id")?,
            run_version: read_col(row, TABLE, "run_version")?,
            work_generation: read_col(row, TABLE, "work_generation")?,
            evidence_generation: read_col(row, TABLE, "evidence_generation")?,
            task_digest: read_col(row, TABLE, "task_digest")?,
            handoff_digest: read_col(row, TABLE, "handoff_digest")?,
            evidence_digest: read_col(row, TABLE, "evidence_digest")?,
            model: read_col(row, TABLE, "model")?,
            question_version: read_col(row, TABLE, "question_version")?,
            policy_version: read_col(row, TABLE, "policy_version")?,
            outcome: read_col(row, TABLE, "outcome")?,
            requested_at: read_col(row, TABLE, "requested_at")?,
            answered_at: read_col(row, TABLE, "answered_at")?,
        })
    }
}

/// The `judgments` row.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::store) struct JudgmentRow {
    set_id: String,
    question: String,
    probabilities_json: String,
    answer: String,
    threshold: Option<f64>,
}

impl JudgmentRow {
    /// Encode `judgment` under `set` — the row's set link.
    pub(in crate::store) fn from_core(
        set: &JudgmentSetId,
        judgment: &Judgment,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            set_id: set.0.clone(),
            question: question_name(judgment.question),
            probabilities_json: json_write(
                &probabilities_to_json(&judgment.probabilities),
                ROWS_TABLE,
                "probabilities_json",
            )?,
            answer: judgment.answer.clone(),
            threshold: judgment.threshold,
        })
    }

    /// The checked decode back to `Judgment`.
    pub(in crate::store) fn to_core(&self) -> Result<Judgment, StoreError> {
        let parsed = json_parse(&self.probabilities_json, ROWS_TABLE, "probabilities_json")?;
        Ok(Judgment {
            question: question_from_name(&self.question, ROWS_TABLE, "question")?,
            probabilities: probabilities_from_json(&parsed)?,
            answer: self.answer.clone(),
            threshold: self.threshold,
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("set_id", self.set_id.clone().into()),
            ("question", self.question.clone().into()),
            ("probabilities_json", self.probabilities_json.clone().into()),
            ("answer", self.answer.clone().into()),
            ("threshold", self.threshold.into()),
        ]
    }

    /// Pull the row's own columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            set_id: read_col(row, ROWS_TABLE, "set_id")?,
            question: read_col(row, ROWS_TABLE, "question")?,
            probabilities_json: read_col(row, ROWS_TABLE, "probabilities_json")?,
            answer: read_col(row, ROWS_TABLE, "answer")?,
            threshold: read_col(row, ROWS_TABLE, "threshold")?,
        })
    }
}

/// The stored question name — `handoff_meets_item_<item>` for the per-item
/// kind, `Question::as_str()` otherwise (mirrors `AssessmentKey::question_name`).
fn question_name(question: Question) -> String {
    match question {
        Question::HandoffMeetsItem { item } => {
            format!("handoff_meets_item_{item}")
        }
        Question::DoneWhenVerifiable
        | Question::WeakestSufficientTier
        | Question::ChangesFiles
        | Question::SecurityBoundary
        | Question::NeedsExternal
        | Question::LongRunning
        | Question::RelatedTab
        | Question::BlockedOnInput
        | Question::NoRecentProgress
        | Question::OutsideScope
        | Question::ProviderLimited => question.as_str().into(),
    }
}

fn question_from_name(
    text: &str,
    table: &'static str,
    column: &'static str,
) -> Result<Question, StoreError> {
    if let Some(suffix) = text.strip_prefix("handoff_meets_item_") {
        let item = suffix
            .parse::<u8>()
            .map_err(|_parse| corrupt(table, column, format!("bad item index {suffix:?}")))?;
        return Ok(Question::HandoffMeetsItem { item });
    }
    enum_decode(text, table, column, FIXED_QUESTIONS, Question::as_str)
}

fn probabilities_to_json(probabilities: &BTreeMap<String, Probability>) -> Value {
    Value::Object(
        probabilities
            .iter()
            .map(|(label, p)| (label.clone(), Value::from(p.0)))
            .collect(),
    )
}

fn probabilities_from_json(value: &Value) -> Result<BTreeMap<String, Probability>, StoreError> {
    value
        .as_object()
        .ok_or_else(|| corrupt(ROWS_TABLE, "probabilities_json", "expected an object"))?
        .iter()
        .map(|(label, p)| {
            let number = p.as_f64().ok_or_else(|| {
                corrupt(ROWS_TABLE, "probabilities_json", "probability not a number")
            })?;
            if !(number.is_finite() && (0.0..=1.0).contains(&number)) {
                return Err(corrupt(
                    ROWS_TABLE,
                    "probabilities_json",
                    format!("probability {number} outside [0, 1]"),
                ));
            }
            Ok((label.clone(), Probability(number)))
        })
        .collect()
}

fn set_to_json(set: &JudgmentSet) -> Value {
    let versions = set.versions;
    json!({
        "set_id": set.id.0,
        "purpose": set.purpose.as_str(),
        "launch_id": set.launch.as_ref().map(|l| l.0.as_str()),
        "run_id": set.run.as_ref().map(|r| r.0.as_str()),
        "run_version": versions.map(|v| v.version),
        "work_generation": versions.map(|v| v.work_generation),
        "evidence_generation": versions.map(|v| v.evidence_generation),
        "task_digest": hex_encode(set.task_digest),
        "handoff_digest": set.handoff_digest.map(hex_encode),
        "evidence_digest": set.evidence_digest.map(hex_encode),
        "model": set.model,
        "question_version": set.question_version.0,
        "policy_version": set.policy_version.0,
        "outcome": set.outcome.as_str(),
    })
}

fn set_from_json(value: &Value) -> Result<JudgmentSet, StoreError> {
    const COL: &str = "result_json";
    let versions = match (
        member(value, "run_version", TABLE, COL)?,
        member(value, "work_generation", TABLE, COL)?,
        member(value, "evidence_generation", TABLE, COL)?,
    ) {
        (Value::Null, Value::Null, Value::Null) => None,
        (v, w, e) => Some(VersionTriple {
            version: json_u64(v)?,
            work_generation: json_u64(w)?,
            evidence_generation: json_u64(e)?,
        }),
    };
    Ok(JudgmentSet {
        id: JudgmentSetId(member_str(value, "set_id", TABLE, COL)?),
        purpose: enum_decode(
            &member_str(value, "purpose", TABLE, COL)?,
            TABLE,
            COL,
            PURPOSES,
            JudgmentPurpose::as_str,
        )?,
        launch: member_opt_str(value, "launch_id", TABLE, COL)?.map(LaunchId),
        run: member_opt_str(value, "run_id", TABLE, COL)?.map(RunId),
        versions,
        task_digest: hex_decode(&member_str(value, "task_digest", TABLE, COL)?, TABLE, COL)?,
        handoff_digest: member_opt_str(value, "handoff_digest", TABLE, COL)?
            .map(|d| hex_decode(&d, TABLE, COL))
            .transpose()?,
        evidence_digest: member_opt_str(value, "evidence_digest", TABLE, COL)?
            .map(|d| hex_decode(&d, TABLE, COL))
            .transpose()?,
        model: member_str(value, "model", TABLE, COL)?,
        question_version: QuestionVersion(member_str(value, "question_version", TABLE, COL)?),
        policy_version: ConfigVersion(member_str(value, "policy_version", TABLE, COL)?),
        outcome: enum_decode(
            &member_str(value, "outcome", TABLE, COL)?,
            TABLE,
            COL,
            OUTCOMES,
            JudgmentOutcome::as_str,
        )?,
    })
}

fn json_u64(value: &Value) -> Result<u64, StoreError> {
    value
        .as_u64()
        .ok_or_else(|| corrupt(TABLE, "result_json", "version field not a u64"))
}

fn judgment_to_json(judgment: &Judgment) -> Value {
    json!({
        "question": question_name(judgment.question),
        "probabilities": probabilities_to_json(&judgment.probabilities),
        "answer": judgment.answer,
        "threshold": judgment.threshold,
    })
}

fn judgment_from_json(value: &Value) -> Result<Judgment, StoreError> {
    const COL: &str = "result_json";
    Ok(Judgment {
        question: question_from_name(&member_str(value, "question", TABLE, COL)?, TABLE, COL)?,
        probabilities: probabilities_from_json(member(value, "probabilities", TABLE, COL)?)?,
        answer: member_str(value, "answer", TABLE, COL)?,
        threshold: {
            let threshold = member(value, "threshold", TABLE, COL)?;
            if threshold.is_null() {
                None
            } else {
                Some(
                    threshold
                        .as_f64()
                        .ok_or_else(|| corrupt(TABLE, COL, "threshold not a number"))?,
                )
            }
        },
    })
}

/// The `judgments` receipt payload: `{"set": {…}, "judgments": […]}`.
pub(in crate::store) fn record_to_json(record: &JudgmentRecord) -> Value {
    json!({
        "set": set_to_json(&record.set),
        "judgments": record.judgments.iter().map(judgment_to_json).collect::<Vec<_>>(),
    })
}

/// Decode the `judgments` receipt payload.
///
/// # Errors
/// [`StoreError::CorruptRow`] on any malformed member.
pub(in crate::store) fn record_from_json(value: &Value) -> Result<JudgmentRecord, StoreError> {
    const COL: &str = "result_json";
    Ok(JudgmentRecord {
        set: set_from_json(member(value, "set", TABLE, COL)?)?,
        judgments: member(value, "judgments", TABLE, COL)?
            .as_array()
            .ok_or_else(|| corrupt(TABLE, COL, "judgments is not an array"))?
            .iter()
            .map(judgment_from_json)
            .collect::<Result<_, _>>()?,
    })
}
