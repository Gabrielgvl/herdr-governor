//! `launch` — the `launches` row ↔ `Launch`, plus the hand-mapped JSON for
//! `task_json` (`Task`), `decision_json` (`Decision` — the `outcomes` view
//! reads `$.exploration.assigned`/`.executed`) and `result_json`
//! (`LaunchOutcome`). The `outcome`/`outcome_reason` columns mirror the
//! result JSON so they stay queryable; a disagreement between them and the
//! JSON is a corrupt row.

use rusqlite::Row;
use serde_json::{Value, json};

use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::identity::{
    AgentKind, CallerKey, IdempotencyKey, LaunchId, PaneId, ProjectRoot, RunId, TabId, Timestamp,
};
use governor_core::lifecycle::{CreatedTopology, EffectCertainty};
use governor_core::routing::{Candidate, Decision, Exploration};
use governor_core::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase, Retention, Task};

use crate::store::error::StoreError;
use crate::store::rows::{
    Params, corrupt, enum_decode, enum_opt_decode, hex_decode, hex_encode, json_opt_str,
    json_parse, json_str, json_str_arr, json_write, member, member_arr, member_bool,
    member_opt_str, member_str, read_col, ts_encode,
};

const TABLE: &str = "launches";

const PHASES: &[LaunchPhase] = &[
    LaunchPhase::Evaluating,
    LaunchPhase::Routed,
    LaunchPhase::Launching,
    LaunchPhase::Done,
];

const ABSTAIN_REASONS: &[AbstainReason] = &[
    AbstainReason::EvaluationFailed,
    AbstainReason::InterruptedBeforeDecision,
    AbstainReason::NoHigherTier,
    AbstainReason::NoCandidates,
];

const CERTAINTIES: &[EffectCertainty] = &[EffectCertainty::Absent, EffectCertainty::Unknown];

const RETENTIONS: &[Retention] = &[Retention::Retire, Retention::Keep];

/// The `launches` row. `caller_id` is the surrogate the writer resolved
/// through `rows::caller::caller_id`; `to_core` takes the joined key back.
/// `created_at`/`updated_at` are store-stamped — `Launch` carries neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct LaunchRow {
    launch_id: String,
    caller_id: i64,
    project_root: String,
    idempotency_key: String,
    digest_version: i64,
    task_digest: String,
    task_json: String,
    phase: String,
    outcome: Option<String>,
    outcome_reason: Option<String>,
    decision_json: Option<String>,
    config_version: Option<String>,
    result_json: Option<String>,
    created_at: String,
    updated_at: String,
}

impl LaunchRow {
    /// Encode `launch`; `caller` is the surrogate id the writer resolved,
    /// `now` stamps `created_at`/`updated_at`.
    ///
    /// # Errors
    /// Propagates JSON/time/hex encode failures as [`StoreError::CorruptRow`].
    pub(in crate::store) fn from_core(
        launch: &Launch,
        caller: i64,
        now: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            launch_id: launch.id.0.clone(),
            caller_id: caller,
            project_root: launch.project_root.0.clone(),
            idempotency_key: launch.idempotency_key.0.clone(),
            digest_version: i64::from(launch.digest_version),
            task_digest: hex_encode(launch.task_digest),
            task_json: json_write(&task_to_json(&launch.task), TABLE, "task_json")?,
            phase: launch.phase.as_str().into(),
            outcome: launch.outcome.as_ref().map(|o| o.as_str().into()),
            outcome_reason: launch
                .outcome
                .as_ref()
                .and_then(LaunchOutcome::reason_str)
                .map(str::to_owned),
            decision_json: launch
                .decision
                .as_ref()
                .map(|d| json_write(&decision_to_json(d), TABLE, "decision_json"))
                .transpose()?,
            config_version: launch.config_version.as_ref().map(|v| v.0.clone()),
            result_json: launch
                .outcome
                .as_ref()
                .map(|o| json_write(&outcome_to_json(o), TABLE, "result_json"))
                .transpose()?,
            created_at: ts_encode(now, TABLE, "created_at")?,
            updated_at: ts_encode(now, TABLE, "updated_at")?,
        })
    }

    /// The checked decode back to `Launch`; `caller` is the key the read's
    /// `callers` join selected.
    ///
    /// # Errors
    /// [`StoreError::CorruptRow`] on unknown enum text, malformed JSON, a
    /// disagreement between the `outcome`/`outcome_reason` columns and the
    /// `result_json` payload, or a `done` row without an outcome (the
    /// Appendix B CHECK restated for defense-in-depth).
    pub(in crate::store) fn to_core(&self, caller: CallerKey) -> Result<Launch, StoreError> {
        let task_json = json_parse(&self.task_json, TABLE, "task_json")?;
        let phase = enum_decode(&self.phase, TABLE, "phase", PHASES, LaunchPhase::as_str)?;
        let outcome = self.decode_outcome()?;
        if (phase == LaunchPhase::Done) != outcome.is_some() {
            return Err(corrupt(
                TABLE,
                "outcome",
                "phase is not 'done' exactly when outcome is set",
            ));
        }
        Ok(Launch {
            id: LaunchId(self.launch_id.clone()),
            caller,
            project_root: ProjectRoot(self.project_root.clone()),
            idempotency_key: IdempotencyKey(self.idempotency_key.clone()),
            digest_version: u32::try_from(self.digest_version).map_err(|_negative| {
                corrupt(
                    TABLE,
                    "digest_version",
                    format!("{} is negative", self.digest_version),
                )
            })?,
            task_digest: hex_decode(&self.task_digest, TABLE, "task_digest")?,
            task: task_from_json(&task_json)?,
            phase,
            decision: self
                .decision_json
                .as_deref()
                .map(|text| {
                    let value = json_parse(text, TABLE, "decision_json")?;
                    decision_from_json(&value)
                })
                .transpose()?,
            config_version: self.config_version.clone().map(ConfigVersion),
            outcome,
        })
    }

    /// Decode `outcome`/`outcome_reason`/`result_json` as one payload:
    /// `result_json` is required iff `outcome` is set, and must agree with
    /// both columns — denormalized mirrors that disagree are corrupt.
    fn decode_outcome(&self) -> Result<Option<LaunchOutcome>, StoreError> {
        let (outcome_col, result) = match (&self.outcome, &self.result_json) {
            (Some(outcome), Some(result)) => (outcome, result),
            (None, None) => return Ok(None),
            (None, Some(_)) => {
                return Err(corrupt(TABLE, "result_json", "result without outcome"));
            }
            (Some(_), None) => {
                return Err(corrupt(TABLE, "result_json", "outcome without result"));
            }
        };
        let parsed = outcome_from_json(&json_parse(result, TABLE, "result_json")?)?;
        if parsed.as_str() != outcome_col {
            return Err(corrupt(
                TABLE,
                "result_json",
                format!("outcome {outcome_col:?} disagrees with result payload"),
            ));
        }
        if parsed.reason_str().map(str::to_owned) != self.outcome_reason {
            return Err(corrupt(
                TABLE,
                "outcome_reason",
                "column disagrees with result payload",
            ));
        }
        Ok(Some(parsed))
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("launch_id", self.launch_id.clone().into()),
            ("caller_id", self.caller_id.into()),
            ("project_root", self.project_root.clone().into()),
            ("idempotency_key", self.idempotency_key.clone().into()),
            ("digest_version", self.digest_version.into()),
            ("task_digest", self.task_digest.clone().into()),
            ("task_json", self.task_json.clone().into()),
            ("phase", self.phase.clone().into()),
            ("outcome", self.outcome.clone().into()),
            ("outcome_reason", self.outcome_reason.clone().into()),
            ("decision_json", self.decision_json.clone().into()),
            ("config_version", self.config_version.clone().into()),
            ("result_json", self.result_json.clone().into()),
            ("created_at", self.created_at.clone().into()),
            ("updated_at", self.updated_at.clone().into()),
        ]
    }

    /// Pull the row's own columns out of a query row (the joined caller
    /// columns go through `rows::caller::key_from_row`).
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            launch_id: read_col(row, TABLE, "launch_id")?,
            caller_id: read_col(row, TABLE, "caller_id")?,
            project_root: read_col(row, TABLE, "project_root")?,
            idempotency_key: read_col(row, TABLE, "idempotency_key")?,
            digest_version: read_col(row, TABLE, "digest_version")?,
            task_digest: read_col(row, TABLE, "task_digest")?,
            task_json: read_col(row, TABLE, "task_json")?,
            phase: read_col(row, TABLE, "phase")?,
            outcome: read_col(row, TABLE, "outcome")?,
            outcome_reason: read_col(row, TABLE, "outcome_reason")?,
            decision_json: read_col(row, TABLE, "decision_json")?,
            config_version: read_col(row, TABLE, "config_version")?,
            result_json: read_col(row, TABLE, "result_json")?,
            created_at: read_col(row, TABLE, "created_at")?,
            updated_at: read_col(row, TABLE, "updated_at")?,
        })
    }
}

/// `task_json` — hand-mapped `Task`; `label` is presentation-only (H#41)
/// and round-trips like the rest.
fn task_to_json(task: &Task) -> Value {
    json!({
        "objective": task.objective,
        "scope": task.scope,
        "done_when": task.done_when,
        "constraints": task.constraints,
        "tier": task.tier.as_ref().map(|t| t.0.as_str()),
        "recovery_of": task.recovery_of.as_ref().map(|r| r.0.as_str()),
        "label": task.label,
        "cwd": task.cwd,
        "retention": task.retention.map(|retention| retention.as_str()),
    })
}

fn task_from_json(value: &Value) -> Result<Task, StoreError> {
    Ok(Task {
        objective: member_str(value, "objective", TABLE, "task_json")?,
        scope: member_str(value, "scope", TABLE, "task_json")?,
        done_when: json_str_arr(
            member(value, "done_when", TABLE, "task_json")?,
            TABLE,
            "task_json",
        )?,
        constraints: json_str_arr(
            member(value, "constraints", TABLE, "task_json")?,
            TABLE,
            "task_json",
        )?,
        tier: member_opt_str(value, "tier", TABLE, "task_json")?.map(Tier),
        recovery_of: member_opt_str(value, "recovery_of", TABLE, "task_json")?.map(RunId),
        label: member_opt_str(value, "label", TABLE, "task_json")?,
        cwd: member_opt_str(value, "cwd", TABLE, "task_json")?,
        retention: enum_opt_decode(
            match value.get("retention") {
                None => None,
                Some(raw) => json_opt_str(raw, TABLE, "task_json")?,
            }
            .as_deref(),
            TABLE,
            "task_json",
            RETENTIONS,
            Retention::as_str,
        )?,
    })
}

fn opt_tier(value: &Value, key: &'static str) -> Result<Option<Tier>, StoreError> {
    member_opt_str(value, key, TABLE, "decision_json").map(|s| s.map(Tier))
}

fn candidate_to_json(candidate: &Candidate) -> Value {
    json!({
        "operating_point": candidate.operating_point.0,
        "provider": candidate.provider.0,
        "tier": candidate.tier.0,
        "harness": candidate.harness.0,
        "args": candidate.args,
    })
}

fn candidate_from_json(value: &Value) -> Result<Candidate, StoreError> {
    const COL: &str = "decision_json";
    Ok(Candidate {
        operating_point: OperatingPointId(member_str(value, "operating_point", TABLE, COL)?),
        provider: Provider(member_str(value, "provider", TABLE, COL)?),
        tier: Tier(member_str(value, "tier", TABLE, COL)?),
        harness: AgentKind(member_str(value, "harness", TABLE, COL)?),
        args: json_str_arr(member(value, "args", TABLE, COL)?, TABLE, COL)?,
    })
}

/// `decision_json` — the F13 record. `exploration` must sit at
/// `$.exploration.assigned`/`$.exploration.executed`: the `outcomes` view's
/// `json_extract` reads that exact path.
pub(in crate::store) fn decision_to_json(decision: &Decision) -> Value {
    json!({
        "judged_tier": decision.judged_tier.0,
        "requested_tier": decision.requested_tier.as_ref().map(|t| t.0.as_str()),
        "policy_cap": decision.policy_cap.as_ref().map(|t| t.0.as_str()),
        "policy_floor": decision.policy_floor.as_ref().map(|t| t.0.as_str()),
        "caller_uplift": decision.caller_uplift.as_ref().map(|t| t.0.as_str()),
        "recovery_minimum": decision.recovery_minimum.as_ref().map(|t| t.0.as_str()),
        "exploration": {
            "assigned": decision.exploration.assigned,
            "executed": decision.exploration.executed,
        },
        "start_tier": decision.start_tier.0,
        "candidates": decision.candidates.iter().map(candidate_to_json).collect::<Vec<_>>(),
        "config_version": decision.config_version.0,
    })
}

fn decision_from_json(value: &Value) -> Result<Decision, StoreError> {
    const COL: &str = "decision_json";
    let exploration = member(value, "exploration", TABLE, COL)?;
    Ok(Decision {
        judged_tier: Tier(member_str(value, "judged_tier", TABLE, COL)?),
        requested_tier: opt_tier(value, "requested_tier")?,
        policy_cap: opt_tier(value, "policy_cap")?,
        policy_floor: opt_tier(value, "policy_floor")?,
        caller_uplift: opt_tier(value, "caller_uplift")?,
        recovery_minimum: opt_tier(value, "recovery_minimum")?,
        exploration: Exploration {
            assigned: member_bool(exploration, "assigned", TABLE, COL)?,
            executed: member_bool(exploration, "executed", TABLE, COL)?,
        },
        start_tier: Tier(member_str(value, "start_tier", TABLE, COL)?),
        candidates: member_arr(value, "candidates", TABLE, COL)?
            .iter()
            .map(candidate_from_json)
            .collect::<Result<_, _>>()?,
        config_version: ConfigVersion(member_str(value, "config_version", TABLE, COL)?),
    })
}

fn topology_to_json(topology: &CreatedTopology) -> Value {
    json!({
        "tab": topology.tab.as_ref().map(|t| t.0.as_str()),
        "panes": topology.panes.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
    })
}

fn topology_from_json(value: &Value) -> Result<CreatedTopology, StoreError> {
    const COL: &str = "result_json";
    Ok(CreatedTopology {
        tab: member_opt_str(value, "tab", TABLE, COL)?.map(TabId),
        panes: member_arr(value, "panes", TABLE, COL)?
            .iter()
            .map(|pane| json_str(pane, TABLE, COL).map(PaneId))
            .collect::<Result<_, _>>()?,
    })
}

fn outcome_to_json(outcome: &LaunchOutcome) -> Value {
    match outcome {
        LaunchOutcome::Launched {
            run,
            operating_point,
            requested_operating_point,
            tier_evidence,
        } => json!({
            "outcome": "launched",
            "run": run.0,
            "operating_point": operating_point.0,
            "requested_operating_point": requested_operating_point.as_ref().map(|o| o.0.as_str()),
            "tier_evidence": decision_to_json(tier_evidence),
        }),
        LaunchOutcome::Abstained { reason } => json!({
            "outcome": "abstained",
            "reason": reason.as_str(),
        }),
        LaunchOutcome::Rejected => json!({"outcome": "rejected"}),
        LaunchOutcome::Failed {
            certainty,
            run,
            created_topology,
        } => json!({
            "outcome": "failed",
            "certainty": certainty.as_str(),
            "run": run.as_ref().map(|r| r.0.as_str()),
            "created_topology": topology_to_json(created_topology),
        }),
    }
}

fn outcome_from_json(value: &Value) -> Result<LaunchOutcome, StoreError> {
    const COL: &str = "result_json";
    match member_str(value, "outcome", TABLE, COL)?.as_str() {
        "launched" => Ok(LaunchOutcome::Launched {
            run: RunId(member_str(value, "run", TABLE, COL)?),
            operating_point: OperatingPointId(member_str(value, "operating_point", TABLE, COL)?),
            requested_operating_point: member_opt_str(
                value,
                "requested_operating_point",
                TABLE,
                COL,
            )?
            .map(OperatingPointId),
            tier_evidence: decision_from_json(member(value, "tier_evidence", TABLE, COL)?)?,
        }),
        "abstained" => Ok(LaunchOutcome::Abstained {
            reason: enum_decode(
                &member_str(value, "reason", TABLE, COL)?,
                TABLE,
                COL,
                ABSTAIN_REASONS,
                AbstainReason::as_str,
            )?,
        }),
        "rejected" => Ok(LaunchOutcome::Rejected),
        "failed" => Ok(LaunchOutcome::Failed {
            certainty: enum_decode(
                &member_str(value, "certainty", TABLE, COL)?,
                TABLE,
                COL,
                CERTAINTIES,
                EffectCertainty::as_str,
            )?,
            run: member_opt_str(value, "run", TABLE, COL)?.map(RunId),
            created_topology: topology_from_json(member(value, "created_topology", TABLE, COL)?)?,
        }),
        other => Err(corrupt(
            TABLE,
            COL,
            format!("unknown outcome spelling {other:?}"),
        )),
    }
}
