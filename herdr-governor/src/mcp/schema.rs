//! The three tools' argument DTOs beside their hand-written
//! `inputSchema`s (spec §6.2, plan §4.11): strict schemas on every tool
//! and every action — unknown fields refused by `deny_unknown_fields`,
//! with a unit test pinning each schema's property set equal to its DTO's
//! field set so the wire contract and the decoder cannot drift. The DTOs
//! map to core types only; `Task::violations` and the refusal codes stay
//! in `governor-core`.

use governor_core::task::{CONSTRAINTS_MAX_ITEMS, DONE_WHEN_MAX_ITEMS, DONE_WHEN_MIN_ITEMS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod task_args;

use task_args::TaskArgs;

/// The wire name of the F7 tool.
pub(super) const STATUS: &str = "herdr_status";
/// The wire name of the F5 tool.
pub(super) const LAUNCH: &str = "herdr_launch";
/// The wire name of the F6 tool.
pub(super) const RUN: &str = "herdr_run";

/// F7 — `herdr_status {eventId?, cursor?}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StatusArgs {
    /// `eventId` — fetch one event body (F7).
    #[serde(default)]
    pub event_id: Option<String>,
    /// `cursor` — resume a paged listing (§4.12 opaque cursor).
    #[serde(default)]
    pub cursor: Option<String>,
}

/// F5 — `herdr_launch {task, idempotencyKey}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LaunchArgs {
    /// `task` — the caller-authored work unit.
    pub task: TaskArgs,
    /// `idempotencyKey` — unique within `(caller key, projectRoot)` (F11).
    pub idempotency_key: String,
}

/// F6 — `herdr_run` arguments: the `action` member picks the variant and
/// every action keeps its own strict field set (§6.2 — unknown fields
/// refused, no aliases).
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum RunArgs {
    /// `observe {runId, cursor?}` — state, settlement, handoff,
    /// acceptance, outbox (§4.12 pages the outbox by `seq`; the cursor is
    /// the opaque continuation of the previous page).
    Observe {
        /// The Run to read.
        run_id: String,
        /// `cursor` — resume the paged outbox listing; absent reads the
        /// first page (F6, §4.12).
        #[serde(default)]
        cursor: Option<String>,
    },
    /// `message {runId, messageKey, text}` — an F17 follow-up.
    Message {
        /// The Run to write to.
        run_id: String,
        /// The caller-supplied key, unique within the Run.
        message_key: String,
        /// The follow-up body.
        text: String,
    },
    /// `ack {eventId}` — an idempotent mailbox ack (F18).
    Ack {
        /// The event to acknowledge.
        event_id: String,
    },
    /// `handover {runIds, successorPaneId}` — F4 ownership transfer.
    Handover {
        /// The Runs to hand over.
        run_ids: Vec<String>,
        /// The F1-verified successor's pane.
        successor_pane_id: String,
    },
    /// `adopt {runIds}` — F19.
    Adopt {
        /// The Runs to adopt.
        run_ids: Vec<String>,
    },
    /// `cancel {runId, closePane?}` — F20 settlement, optional pane close.
    Cancel {
        /// The Run to settle.
        run_id: String,
        /// Also close the pane (default: keep it).
        #[serde(default)]
        close_pane: Option<bool>,
    },
}

/// The `tools/list` result — the three tools with their hand-written
/// `inputSchema`s (§4.11: `additionalProperties:false`, required lists).
/// PR A's list exposes `herdr_status` only (OQ-S); the schemas for all
/// three land together here because the DTO↔schema parity test owns the
/// whole contract — `tools` filters the list it publishes.
#[must_use]
pub(super) fn tool_definitions() -> Value {
    json!({"tools": [status_tool(), launch_tool(), run_tool()]})
}

fn status_tool() -> Value {
    json!({
        "name": STATUS,
        "description": "Governor health, config validity, your unsettled runs, pending recoveries, unread event ids and cooldowns; `eventId` fetches one event body, `cursor` resumes a paged listing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "eventId": {"type": "string"},
                "cursor": {"type": "string"},
            },
            "required": [],
            "additionalProperties": false,
        },
    })
}

fn launch_tool() -> Value {
    let task_schema = json!({
        "type": "object",
        "properties": {
            "objective": {"type": "string", "minLength": 1},
            "scope": {"type": "string", "minLength": 1},
            "doneWhen": {
                "type": "array",
                "items": {"type": "string", "minLength": 1},
                "minItems": DONE_WHEN_MIN_ITEMS,
                "maxItems": DONE_WHEN_MAX_ITEMS,
            },
            "constraints": {
                "type": "array",
                "items": {"type": "string", "minLength": 1},
                "maxItems": CONSTRAINTS_MAX_ITEMS,
            },
            "tier": {"type": "string"},
            "recoveryOf": {"type": "string"},
            "label": {"type": "string", "minLength": 1},
            "cwd": {"type": "string"},
            "retention": {"type": "string", "enum": ["retire", "keep"]},
        },
        "required": ["objective", "scope", "doneWhen"],
        "additionalProperties": false,
    });
    json!({
        "name": LAUNCH,
        "description": "Admit a Task as a new supervised run: `task` is the caller-authored work unit (F5 bounds apply), `idempotencyKey` deduplicates within your project root.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task": task_schema,
                "idempotencyKey": {"type": "string"},
            },
            "required": ["task", "idempotencyKey"],
            "additionalProperties": false,
        },
    })
}

fn run_tool() -> Value {
    let run_ids = || json!({"type": "array", "items": {"type": "string"}});
    let branch = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    json!({
        "name": RUN,
        "description": "Act on runs you own: `observe`, `message`, `ack`, `handover`, `adopt` or `cancel` — the `action` member picks one, with its own strict field set.",
        "inputSchema": {
            "type": "object",
            "oneOf": [
                branch(
                    json!({
                        "action": {"const": "observe"},
                        "runId": {"type": "string"},
                        "cursor": {"type": "string"},
                    }),
                    &["action", "runId"],
                ),
                branch(
                    json!({
                        "action": {"const": "message"},
                        "runId": {"type": "string"},
                        "messageKey": {"type": "string"},
                        "text": {"type": "string"},
                    }),
                    &["action", "runId", "messageKey", "text"],
                ),
                branch(
                    json!({"action": {"const": "ack"}, "eventId": {"type": "string"}}),
                    &["action", "eventId"],
                ),
                branch(
                    json!({
                        "action": {"const": "handover"},
                        "runIds": run_ids(),
                        "successorPaneId": {"type": "string"},
                    }),
                    &["action", "runIds", "successorPaneId"],
                ),
                branch(
                    json!({"action": {"const": "adopt"}, "runIds": run_ids()}),
                    &["action", "runIds"],
                ),
                branch(
                    json!({
                        "action": {"const": "cancel"},
                        "runId": {"type": "string"},
                        "closePane": {"type": "boolean"},
                    }),
                    &["action", "runId"],
                ),
            ],
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use governor_core::identity::ProjectRoot;
    use governor_core::task::Retention;
    use serde_json::{Value, json};

    use super::{
        CONSTRAINTS_MAX_ITEMS, DONE_WHEN_MAX_ITEMS, DONE_WHEN_MIN_ITEMS, LAUNCH, LaunchArgs, RUN,
        RunArgs, STATUS, StatusArgs, TaskArgs, tool_definitions,
    };

    fn tool(name: &str) -> Value {
        tool_definitions()["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools is an array"))
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
            .clone()
    }

    fn property_set(schema: &Value) -> BTreeSet<String> {
        schema["properties"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn serialized_set(value: &Value) -> BTreeSet<String> {
        value
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn task_args() -> TaskArgs {
        TaskArgs {
            objective: "o".into(),
            scope: "s".into(),
            done_when: vec!["d".into()],
            constraints: vec!["c".into()],
            tier: Some("t".into()),
            recovery_of: Some("r".into()),
            label: Some("l".into()),
            cwd: Some("/repo".into()),
            retention: Some(super::task_args::RetentionArg::Keep),
        }
    }

    #[test]
    fn schema_matches_dto_field_sets() {
        let status = tool(STATUS);
        assert_eq!(
            property_set(&status["inputSchema"]),
            serialized_set(
                &serde_json::to_value(StatusArgs {
                    event_id: Some("e".into()),
                    cursor: Some("c".into()),
                })
                .expect("status args serialize")
            ),
            "herdr_status schema properties are exactly the DTO fields"
        );

        let launch = tool(LAUNCH);
        assert_eq!(
            property_set(&launch["inputSchema"]),
            serialized_set(
                &serde_json::to_value(LaunchArgs {
                    task: task_args(),
                    idempotency_key: "k".into(),
                })
                .expect("launch args serialize")
            ),
            "herdr_launch schema properties are exactly the DTO fields"
        );
        assert_eq!(
            property_set(&launch["inputSchema"]["properties"]["task"]),
            serialized_set(&serde_json::to_value(task_args()).expect("task args serialize")),
            "the task subschema's properties are exactly the task DTO fields"
        );

        let run = tool(RUN);
        let branches = run["inputSchema"]["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("herdr_run schema is a oneOf"));
        for args in [
            RunArgs::Observe {
                run_id: "r".into(),
                cursor: Some("c".into()),
            },
            RunArgs::Message {
                run_id: "r".into(),
                message_key: "k".into(),
                text: "t".into(),
            },
            RunArgs::Ack {
                event_id: "e".into(),
            },
            RunArgs::Handover {
                run_ids: vec!["r".into()],
                successor_pane_id: "p".into(),
            },
            RunArgs::Adopt {
                run_ids: vec!["r".into()],
            },
            RunArgs::Cancel {
                run_id: "r".into(),
                close_pane: Some(true),
            },
        ] {
            let wire = serde_json::to_value(&args).expect("run args serialize");
            let action = wire["action"].as_str().expect("the tag serializes");
            let branch = branches
                .iter()
                .find(|b| b["properties"]["action"]["const"] == action)
                .unwrap_or_else(|| panic!("action {action} has a schema branch"));
            assert_eq!(
                property_set(branch),
                serialized_set(&wire),
                "the {action} branch properties are exactly the variant fields"
            );
        }
    }

    #[test]
    fn f5_unknown_field_refused() {
        // Positive controls first — a DTO that refuses everything would
        // pass every negative case below.
        assert!(
            serde_json::from_str::<StatusArgs>(r#"{"eventId":"e","cursor":"c"}"#).is_ok(),
            "the status DTO accepts its declared fields"
        );
        assert!(
            serde_json::from_str::<LaunchArgs>(
                r#"{"task":{"objective":"o","scope":"s","doneWhen":["d"]},"idempotencyKey":"k"}"#
            )
            .is_ok(),
            "the launch DTO accepts its declared fields"
        );
        assert!(
            serde_json::from_str::<RunArgs>(r#"{"action":"cancel","runId":"r","closePane":true}"#)
                .is_ok(),
            "the run DTO accepts a declared variant"
        );

        assert!(
            serde_json::from_str::<StatusArgs>(r#"{"eventId":"e","extra":1}"#).is_err(),
            "the status DTO refuses an unknown field"
        );
        assert!(
            serde_json::from_str::<LaunchArgs>(
                r#"{"task":{"objective":"o","scope":"s","doneWhen":["d"]},"idempotencyKey":"k","surprise":true}"#
            )
            .is_err(),
            "the launch DTO refuses an unknown field"
        );
        assert!(
            serde_json::from_str::<TaskArgs>(
                r#"{"objective":"o","scope":"s","doneWhen":["d"],"bogus":1}"#
            )
            .is_err(),
            "the task DTO refuses an unknown field"
        );
        for raw in [
            r#"{"action":"observe","runId":"r","extra":1}"#,
            r#"{"action":"cancel","runId":"r","closePane":true,"bogus":0}"#,
            r#"{"action":"teleport","runId":"r"}"#,
            r#"{"action":"observe"}"#,
        ] {
            assert!(
                serde_json::from_str::<RunArgs>(raw).is_err(),
                "run args refuse unknown fields, unknown actions and missing fields: {raw}"
            );
        }
    }

    #[test]
    fn f5_retention_wire_spellings_map_to_core() {
        let decode = |member: &str| {
            serde_json::from_str::<TaskArgs>(&format!(
                r#"{{"objective":"o","scope":"s","doneWhen":["d"]{member}}}"#
            ))
            .map(|args| args.into_task().retention)
        };
        assert_eq!(
            decode("").ok(),
            Some(None),
            "absent retention is the default"
        );
        assert_eq!(
            decode(r#","retention":"retire""#).ok(),
            Some(Some(Retention::Retire)),
            "retire maps to the core default"
        );
        assert_eq!(
            decode(r#","retention":"keep""#).ok(),
            Some(Some(Retention::Keep)),
            "keep maps to the core opt-out"
        );
        assert!(
            decode(r#","retention":"hold""#).is_err(),
            "a third spelling refuses at decode (F5 strict schema)"
        );
        assert_eq!(
            tool(LAUNCH)["inputSchema"]["properties"]["task"]["properties"]["retention"]["enum"],
            json!(["retire", "keep"]),
            "the schema's vocabulary is the DTO's"
        );
    }

    #[test]
    fn f5_task_bounds_at_schema() {
        let task = tool(LAUNCH)["inputSchema"]["properties"]["task"].clone();
        let done_when = &task["properties"]["doneWhen"];
        assert_eq!(
            done_when["minItems"],
            json!(DONE_WHEN_MIN_ITEMS),
            "doneWhen's lower bound is the F5 minimum"
        );
        assert_eq!(
            done_when["maxItems"],
            json!(DONE_WHEN_MAX_ITEMS),
            "doneWhen's upper bound is the F5 maximum"
        );
        assert_eq!(
            task["properties"]["constraints"]["maxItems"],
            json!(CONSTRAINTS_MAX_ITEMS),
            "constraints's upper bound is the F5 maximum"
        );
        assert_eq!(
            task["required"],
            json!(["objective", "scope", "doneWhen"]),
            "the schema's required list is the F5 required set"
        );

        // The schema declares the bounds for callers; the server-side
        // enforcement is `Task::violations` at admission — the DTO still
        // deserializes so the refusal is the typed TASK_INVALID, not a
        // protocol fault.
        let over = TaskArgs {
            done_when: vec!["d".into(); DONE_WHEN_MAX_ITEMS + 1],
            ..task_args()
        };
        let violations = over.into_task().violations(&ProjectRoot("/repo".into()));
        assert!(
            violations.contains(&"done_when_bounds"),
            "an over-bound doneWhen reaches the F5 violation list"
        );
    }
}
