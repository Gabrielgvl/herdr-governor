//! `effect` — the `effects` row ↔ `Effect`, plus the hand-mapped JSON for
//! `target_json` (`EffectTarget`) and `result_json` (`EffectReceipt`).
//! `planned_at`/`completed_at` are store-stamped; `dispatched_at` is the
//! core field (F24 reads it as repair-window evidence).
//!
//! `result_json` on a `failed` row holds the writer's `{"error": …}` cause
//! (OQ-11), which is not a typed receipt: it decodes to `receipt: None`.
//! Any other object must carry a `receipt` tag.

use rusqlite::Row;
use serde_json::{Value, json};

use governor_core::identity::{
    AgentKind, AgentName, ChildIdentity, EffectId, EffectKey, HerdrIncarnation, LaunchId,
    NativeSession, PaneId, RunId, TabId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectTarget,
};
use governor_core::routing::PlacementPlan;

use crate::store::error::StoreError;
use crate::store::rows::judgment::{record_from_json, record_to_json};
use crate::store::rows::{
    Params, corrupt, enum_decode, enum_opt_decode, hex_encode, hex_opt_decode, json_parse,
    json_write, member, member_opt_str, member_str, read_col, ts_encode, ts_opt_decode,
    ts_opt_encode,
};

const TABLE: &str = "effects";

pub(in crate::store) const KINDS: &[EffectKind] = &[
    EffectKind::JevEvaluate,
    EffectKind::TabCreate,
    EffectKind::PaneSplit,
    EffectKind::AgentStart,
    EffectKind::Prompt,
    EffectKind::Close,
];

pub(in crate::store) const STATES: &[EffectState] = &[
    EffectState::Planned,
    EffectState::Dispatching,
    EffectState::Acknowledged,
    EffectState::Failed,
    EffectState::Unconfirmed,
];

const CERTAINTIES: &[EffectCertainty] = &[EffectCertainty::Absent, EffectCertainty::Unknown];

/// The `effects` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct EffectRow {
    effect_id: String,
    effect_key: String,
    kind: String,
    subject_launch_id: Option<String>,
    subject_run_id: Option<String>,
    target_json: Option<String>,
    payload_digest: Option<String>,
    state: String,
    certainty: Option<String>,
    result_json: Option<String>,
    planned_at: String,
    dispatched_at: Option<String>,
    completed_at: Option<String>,
}

impl EffectRow {
    /// Encode `effect`; `planned_at`/`completed_at` are the writer's stamps.
    pub(in crate::store) fn from_core(
        effect: &Effect,
        planned_at: Timestamp,
        completed_at: Option<Timestamp>,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            effect_id: effect.id.0.clone(),
            effect_key: effect.key.0.clone(),
            kind: effect.kind.as_str().into(),
            subject_launch_id: effect.subject_launch.as_ref().map(|l| l.0.clone()),
            subject_run_id: effect.subject_run.as_ref().map(|r| r.0.clone()),
            target_json: effect
                .target
                .as_ref()
                .map(|t| json_write(&target_to_json(t), TABLE, "target_json"))
                .transpose()?,
            payload_digest: effect.payload_digest.map(hex_encode),
            state: effect.state.as_str().into(),
            certainty: effect.certainty.map(|c| c.as_str().into()),
            result_json: effect
                .receipt
                .as_ref()
                .map(|r| json_write(&receipt_to_json(r), TABLE, "result_json"))
                .transpose()?,
            planned_at: ts_encode(planned_at, TABLE, "planned_at")?,
            dispatched_at: ts_opt_encode(effect.dispatched_at, TABLE, "dispatched_at")?,
            completed_at: ts_opt_encode(completed_at, TABLE, "completed_at")?,
        })
    }

    /// The checked decode back to `Effect`, with the Appendix B CHECK constraints
    /// restated: a subject is required and `failed` needs a certainty.
    pub(in crate::store) fn to_core(&self) -> Result<Effect, StoreError> {
        if self.subject_launch_id.is_none() && self.subject_run_id.is_none() {
            return Err(corrupt(
                TABLE,
                "subject_run_id",
                "an effect serves a launch or a run",
            ));
        }
        let state = enum_decode(&self.state, TABLE, "state", STATES, EffectState::as_str)?;
        let certainty = enum_opt_decode(
            self.certainty.as_deref(),
            TABLE,
            "certainty",
            CERTAINTIES,
            EffectCertainty::as_str,
        )?;
        if state == EffectState::Failed && certainty.is_none() {
            return Err(corrupt(TABLE, "certainty", "'failed' requires a certainty"));
        }
        Ok(Effect {
            id: EffectId(self.effect_id.clone()),
            key: EffectKey(self.effect_key.clone()),
            kind: enum_decode(&self.kind, TABLE, "kind", KINDS, EffectKind::as_str)?,
            subject_launch: self.subject_launch_id.clone().map(LaunchId),
            subject_run: self.subject_run_id.clone().map(RunId),
            target: self
                .target_json
                .as_deref()
                .map(|text| target_from_json(&json_parse(text, TABLE, "target_json")?))
                .transpose()?,
            payload_digest: hex_opt_decode(
                self.payload_digest.as_deref(),
                TABLE,
                "payload_digest",
            )?,
            state,
            certainty,
            receipt: self
                .result_json
                .as_deref()
                .map(|text| receipt_from_json(&json_parse(text, TABLE, "result_json")?))
                .transpose()?
                .flatten(),
            dispatched_at: ts_opt_decode(self.dispatched_at.as_deref(), TABLE, "dispatched_at")?,
        })
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("effect_id", self.effect_id.clone().into()),
            ("effect_key", self.effect_key.clone().into()),
            ("kind", self.kind.clone().into()),
            ("subject_launch_id", self.subject_launch_id.clone().into()),
            ("subject_run_id", self.subject_run_id.clone().into()),
            ("target_json", self.target_json.clone().into()),
            ("payload_digest", self.payload_digest.clone().into()),
            ("state", self.state.clone().into()),
            ("certainty", self.certainty.clone().into()),
            ("result_json", self.result_json.clone().into()),
            ("planned_at", self.planned_at.clone().into()),
            ("dispatched_at", self.dispatched_at.clone().into()),
            ("completed_at", self.completed_at.clone().into()),
        ]
    }

    /// Pull the row's columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            effect_id: read_col(row, TABLE, "effect_id")?,
            effect_key: read_col(row, TABLE, "effect_key")?,
            kind: read_col(row, TABLE, "kind")?,
            subject_launch_id: read_col(row, TABLE, "subject_launch_id")?,
            subject_run_id: read_col(row, TABLE, "subject_run_id")?,
            target_json: read_col(row, TABLE, "target_json")?,
            payload_digest: read_col(row, TABLE, "payload_digest")?,
            state: read_col(row, TABLE, "state")?,
            certainty: read_col(row, TABLE, "certainty")?,
            result_json: read_col(row, TABLE, "result_json")?,
            planned_at: read_col(row, TABLE, "planned_at")?,
            dispatched_at: read_col(row, TABLE, "dispatched_at")?,
            completed_at: read_col(row, TABLE, "completed_at")?,
        })
    }
}

/// `ChildIdentity` as a JSON object (the six F2 parts, `native_session`
/// nullable).
fn identity_to_json(identity: &ChildIdentity) -> Value {
    json!({
        "herdr_incarnation": identity.herdr_incarnation.0,
        "terminal_id": identity.terminal_id.0,
        "agent_kind": identity.agent_kind.0,
        "agent_name": identity.agent_name.0,
        "native_session": identity.native_session.as_ref().map(|s| s.0.as_str()),
        "pane_id": identity.pane_id.0,
    })
}

fn identity_from_json(value: &Value, column: &'static str) -> Result<ChildIdentity, StoreError> {
    Ok(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(member_str(value, "herdr_incarnation", TABLE, column)?),
        terminal_id: TerminalId(member_str(value, "terminal_id", TABLE, column)?),
        agent_kind: AgentKind(member_str(value, "agent_kind", TABLE, column)?),
        agent_name: AgentName(member_str(value, "agent_name", TABLE, column)?),
        native_session: member_opt_str(value, "native_session", TABLE, column)?.map(NativeSession),
        pane_id: PaneId(member_str(value, "pane_id", TABLE, column)?),
    })
}

/// `target_json` — tagged by `target`: `existing_tab {tab}`,
/// `caller_context {pane}`, `agent_pane {plan, tab?}`, `child {identity}`.
fn target_to_json(target: &EffectTarget) -> Value {
    match target {
        EffectTarget::ExistingTab(tab) => json!({"target": "existing_tab", "tab": tab.0}),
        EffectTarget::CallerContext(pane) => json!({"target": "caller_context", "pane": pane.0}),
        EffectTarget::AgentPane(PlacementPlan::ExistingTab { tab }) => {
            json!({"target": "agent_pane", "plan": "existing_tab", "tab": tab.0})
        }
        EffectTarget::AgentPane(PlacementPlan::NewTab) => {
            json!({"target": "agent_pane", "plan": "new_tab"})
        }
        EffectTarget::Child(identity) => {
            json!({"target": "child", "identity": identity_to_json(identity)})
        }
    }
}

fn target_from_json(value: &Value) -> Result<EffectTarget, StoreError> {
    const COL: &str = "target_json";
    match member_str(value, "target", TABLE, COL)?.as_str() {
        "existing_tab" => Ok(EffectTarget::ExistingTab(TabId(member_str(
            value, "tab", TABLE, COL,
        )?))),
        "caller_context" => Ok(EffectTarget::CallerContext(PaneId(member_str(
            value, "pane", TABLE, COL,
        )?))),
        "agent_pane" => match member_str(value, "plan", TABLE, COL)?.as_str() {
            "existing_tab" => Ok(EffectTarget::AgentPane(PlacementPlan::ExistingTab {
                tab: TabId(member_str(value, "tab", TABLE, COL)?),
            })),
            "new_tab" => Ok(EffectTarget::AgentPane(PlacementPlan::NewTab)),
            other => Err(corrupt(
                TABLE,
                COL,
                format!("unknown placement plan {other:?}"),
            )),
        },
        "child" => Ok(EffectTarget::Child(identity_from_json(
            member(value, "identity", TABLE, COL)?,
            COL,
        )?)),
        other => Err(corrupt(
            TABLE,
            COL,
            format!("unknown target spelling {other:?}"),
        )),
    }
}

/// `result_json` — tagged by `receipt`: `agent_started {identity}`,
/// `judgments {set, judgments}`, `tab_created {tab, pane}`,
/// `pane_created {pane}`.
fn receipt_to_json(receipt: &EffectReceipt) -> Value {
    match receipt {
        EffectReceipt::AgentStarted { identity } => {
            json!({"receipt": "agent_started", "identity": identity_to_json(identity)})
        }
        EffectReceipt::Judgments(record) => {
            let mut value = record_to_json(record);
            if let Some(map) = value.as_object_mut() {
                map.entry("receipt")
                    .or_insert_with(|| Value::from("judgments"));
            }
            value
        }
        EffectReceipt::TabCreated { tab, pane } => {
            json!({"receipt": "tab_created", "tab": tab.0, "pane": pane.0})
        }
        EffectReceipt::PaneCreated { pane } => json!({"receipt": "pane_created", "pane": pane.0}),
    }
}

/// `result_json` for a result-commit write: the receipt's JSON, or `NULL`
/// when the write carries none.
pub(in crate::store) fn result_json(
    receipt: Option<&EffectReceipt>,
) -> Result<Option<String>, StoreError> {
    receipt
        .map(|r| json_write(&receipt_to_json(r), TABLE, "result_json"))
        .transpose()
}

/// `None` for the writer's `{"error": …}` failure cause (OQ-11); otherwise
/// the tagged receipt.
fn receipt_from_json(value: &Value) -> Result<Option<EffectReceipt>, StoreError> {
    const COL: &str = "result_json";
    if value.get("receipt").is_none() && value.get("error").is_some() {
        return Ok(None);
    }
    let receipt = match member_str(value, "receipt", TABLE, COL)?.as_str() {
        "agent_started" => EffectReceipt::AgentStarted {
            identity: identity_from_json(member(value, "identity", TABLE, COL)?, COL)?,
        },
        "judgments" => EffectReceipt::Judgments(record_from_json(value)?),
        "tab_created" => EffectReceipt::TabCreated {
            tab: TabId(member_str(value, "tab", TABLE, COL)?),
            pane: PaneId(member_str(value, "pane", TABLE, COL)?),
        },
        "pane_created" => EffectReceipt::PaneCreated {
            pane: PaneId(member_str(value, "pane", TABLE, COL)?),
        },
        other => {
            return Err(corrupt(
                TABLE,
                COL,
                format!("unknown receipt spelling {other:?}"),
            ));
        }
    };
    Ok(Some(receipt))
}
