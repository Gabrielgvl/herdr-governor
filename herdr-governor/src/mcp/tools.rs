//! `tools` — the `tools/list`/`tools/call` ends of the MCP surface,
//! mapped onto `daemon::api` (spec §6.2, plan §4.11): `tools/list`
//! answers the served subset of `schema::tool_definitions` (OQ-S — PR A
//! serves `herdr_status` only; B2 and C4 extend `SERVED`), and
//! `tools/call` decodes `{name, arguments}` through `schema`'s strict
//! DTOs into the `ToolRequest` the coordinator's `Msg::Tool` carries,
//! then encodes the returned `ToolResponse`. A refusal — boundary or
//! daemon — is always an `isError` tool result whose text carries
//! `{"code":…}`; the JSON-RPC `error` member is protocol faults only.

use governor_core::identity::{CallerEnvelope, EventId, IdempotencyKey, MessageKey, PaneId, RunId};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::jsonrpc::{self, Response};
use super::schema::{self, LaunchArgs, RunArgs, StatusArgs};
use crate::daemon::api::{RunAction, ToolCall, ToolError, ToolRequest, ToolResponse};

/// OQ-S — the tools this build lists and serves. `tools/list` and
/// `tools/call` read this one set so the advertised surface and the
/// dispatchable surface cannot drift: PR A is `herdr_status` only; B2
/// adds `herdr_launch`, C4 `herdr_run` (the decode arms already exist in
/// `map_call`).
const SERVED: &[&str] = &[schema::STATUS];

/// `tools/list` — the served subset of the tool definitions, inside the
/// shared 60,000-byte result bound.
#[must_use]
pub fn list_response(id: Value) -> Response {
    let tools = schema::tool_definitions()
        .get("tools")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter(|tool| {
                    SERVED.contains(&tool.get("name").and_then(Value::as_str).unwrap_or_default())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    jsonrpc::result(id, json!({"tools": tools}))
}

/// The `tools/call` params wire shape — `{name, arguments?}`, strict
/// like every other member of this layer; the standard `_meta` is
/// accepted and ignored (the subset has no progress channel).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Map<String, Value>>,
    /// MCP's `_meta` — read once at destructure so it stays a declared,
    /// not dead, member.
    #[serde(rename = "_meta", default)]
    meta: Option<serde::de::IgnoredAny>,
}

/// `tools/call` — decode `params` into the `ToolRequest` the coordinator
/// mailbox carries. Every refusal is a typed `ToolError`, never a
/// JSON-RPC error: the params shape and each tool's strict `arguments`
/// yield `REQUEST_INVALID`; a name outside `SERVED` — unknown *or* not
/// yet serving — yields `TOOL_UNKNOWN`.
pub fn decode_call(caller: CallerEnvelope, params: &Value) -> Result<ToolRequest, ToolError> {
    let CallParams {
        name,
        arguments,
        meta: _meta,
    } = serde_json::from_value::<CallParams>(params.clone()).map_err(|err| {
        ToolError::new(
            ToolError::REQUEST_INVALID,
            format!("tools/call params: {err}"),
        )
    })?;
    if !SERVED.contains(&name.as_str()) {
        return Err(unknown_tool(&name));
    }
    let call = map_call(&name, Value::Object(arguments.unwrap_or_default()))?;
    Ok(ToolRequest { caller, call })
}

/// The `tools/call` wire answer for a `ToolResponse`: `Ok` serializes the
/// result JSON into the text content; `Err` is the typed refusal —
/// `{content:[{type:"text", text:{"code":…,"message":…}}], isError:true}`
/// (§4.11: a typed refusal is a tool result, never a JSON-RPC error — the
/// same channel the relay's `DAEMON_UNAVAILABLE` fabrication rides). The
/// shared `jsonrpc::result` bound still applies to the whole body.
#[must_use]
pub fn call_response(id: Value, response: &ToolResponse) -> Response {
    let (text, is_error) = match &response {
        Ok(body) => (serde_json::to_string(body).unwrap_or_default(), false),
        Err(err) => (
            json!({"code": err.code, "message": err.message}).to_string(),
            true,
        ),
    };
    jsonrpc::result(
        id,
        json!({
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
        }),
    )
}

/// `TOOL_UNKNOWN` — the name is outside `SERVED`, whether it is a known
/// tool this PR does not serve or no tool at all.
fn unknown_tool(name: &str) -> ToolError {
    ToolError::new(
        ToolError::TOOL_UNKNOWN,
        format!("no served tool named {name}"),
    )
}

/// `name` × strict-DTO `arguments` → the typed `ToolCall`. Every known
/// tool maps here — `SERVED` owns which names are reachable — so a decode
/// failure is always `REQUEST_INVALID`; the coordinator owns value
/// validation (`Task::violations`) and the F-rules.
fn map_call(name: &str, arguments: Value) -> Result<ToolCall, ToolError> {
    match name {
        schema::STATUS => {
            let args: StatusArgs = decode_args(name, arguments)?;
            Ok(ToolCall::Status {
                event: args.event_id.map(EventId),
                cursor: args.cursor,
            })
        }
        schema::LAUNCH => {
            let args: LaunchArgs = decode_args(name, arguments)?;
            Ok(ToolCall::Launch {
                key: IdempotencyKey(args.idempotency_key),
                task: args.task.into_task(),
            })
        }
        schema::RUN => {
            let args: RunArgs = decode_args(name, arguments)?;
            Ok(ToolCall::Run(run_action(args)))
        }
        _ => Err(unknown_tool(name)),
    }
}

/// One tool's `arguments` object through its strict DTO — the refusal is
/// `REQUEST_INVALID` with the serde detail attached.
fn decode_args<T: serde::de::DeserializeOwned>(
    name: &str,
    arguments: Value,
) -> Result<T, ToolError> {
    serde_json::from_value(arguments).map_err(|err| {
        ToolError::new(
            ToolError::REQUEST_INVALID,
            format!("{name} arguments: {err}"),
        )
    })
}

/// `herdr_run`'s `action`-tagged DTO onto `RunAction` — the field map
/// only; ownership and state rules are the coordinator's.
fn run_action(args: RunArgs) -> RunAction {
    match args {
        RunArgs::Observe { run_id, cursor } => RunAction::Observe {
            run: RunId(run_id),
            cursor,
        },
        RunArgs::Message {
            run_id,
            message_key,
            text,
        } => RunAction::Message {
            run: RunId(run_id),
            key: MessageKey(message_key),
            text,
        },
        RunArgs::Ack { event_id } => RunAction::Ack {
            event: EventId(event_id),
        },
        RunArgs::Handover {
            run_ids,
            successor_pane_id,
        } => RunAction::Handover {
            runs: run_ids.into_iter().map(RunId).collect(),
            successor_pane: PaneId(successor_pane_id),
        },
        RunArgs::Adopt { run_ids } => RunAction::Adopt {
            runs: run_ids.into_iter().map(RunId).collect(),
        },
        RunArgs::Cancel { run_id, close_pane } => RunAction::Cancel {
            run: RunId(run_id),
            close_pane: close_pane.unwrap_or_default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use governor_core::identity::{
        CallerEnvelope, EventId, PaneId, ProjectRoot, RelayInstanceId, RunId,
    };
    use serde_json::{Value, json};

    use super::{call_response, decode_call, list_response, map_call};
    use crate::daemon::api::{RunAction, ToolCall, ToolError};
    use crate::mcp::jsonrpc::{self, Response};

    fn caller() -> CallerEnvelope {
        CallerEnvelope {
            pane_id: PaneId("w6:pKD".into()),
            project_root: ProjectRoot("/repo".into()),
            relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
        }
    }

    /// §4.11 — a typed refusal is an `isError` tool result whose text
    /// carries `{"code":…}`; the JSON-RPC `error` member is protocol
    /// faults only. Boundary refusals (`decode_call` rejects) and daemon
    /// refusals (`ToolError` from the coordinator) ride the same shape.
    #[test]
    fn tools_call_refusal_is_tool_result_not_rpc_error() {
        let boundary = decode_call(caller(), &json!({"name": "bogus", "arguments": {}}))
            .expect_err("an unknown tool name refuses");
        assert_eq!(boundary.code, ToolError::TOOL_UNKNOWN);
        let daemon = ToolError::new(ToolError::DAEMON_UNAVAILABLE, "socket refused");
        for (err, code) in [
            (boundary, ToolError::TOOL_UNKNOWN),
            (daemon, ToolError::DAEMON_UNAVAILABLE),
        ] {
            let response = call_response(json!(9), &Err(err));
            let Response::Result { result, .. } = &response else {
                panic!("a typed refusal is a result envelope, never an error envelope");
            };
            assert_eq!(
                result["isError"], true,
                "a refused call is an isError tool result"
            );
            assert_eq!(result["content"][0]["type"], "text");
            let text = result["content"][0]["text"]
                .as_str()
                .expect("tool error text is a string");
            let body: Value = serde_json::from_str(text).expect("tool error text is json");
            assert_eq!(body["code"], code, "the text carries the typed code");
            let wire = String::from_utf8(jsonrpc::serialize(&response)).expect("utf8 wire");
            assert!(
                wire.contains("\"result\"") && !wire.contains("\"error\""),
                "the wire line is a result, not an error: {wire}"
            );
        }
    }

    /// OQ-S — PR A's `tools/list` exposes `herdr_status` only; the filter
    /// and the dispatch gate share `SERVED` so they cannot drift.
    #[test]
    fn tools_list_publishes_only_the_served_set() {
        let Response::Result { result, .. } = list_response(json!(1)) else {
            panic!("tools/list answers a result");
        };
        let names: Vec<&str> = result["tools"]
            .as_array()
            .expect("tools is an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name is a string"))
            .collect();
        assert_eq!(names, ["herdr_status"], "PR A lists exactly one tool");
    }

    /// The decode maps arguments through the strict DTOs into typed
    /// domain values — `map_call` covers all three tools; `SERVED` gates
    /// which names the wire accepts, so `herdr_launch`/`herdr_run` decode
    /// is proven here while PR A refuses them above the mapper.
    #[test]
    fn decode_call_maps_arguments_to_typed_calls() {
        let request = decode_call(
            caller(),
            &json!({"name": "herdr_status", "arguments": {"eventId": "e1", "cursor": "c9"}}),
        )
        .expect("status args decode");
        assert_eq!(request.caller.pane_id, PaneId("w6:pKD".into()));
        assert_eq!(
            request.call,
            ToolCall::Status {
                event: Some(EventId("e1".into())),
                cursor: Some("c9".into()),
            },
            "the status call carries typed fields"
        );

        let launch = map_call(
            "herdr_launch",
            json!({
                "task": {"objective": "o", "scope": "s", "doneWhen": ["d"]},
                "idempotencyKey": "k1",
            }),
        )
        .expect("launch args decode");
        let ToolCall::Launch { key, task } = launch else {
            panic!("herdr_launch maps to a Launch call");
        };
        assert_eq!(key.0, "k1");
        assert_eq!(task.objective, "o");

        let run = map_call(
            "herdr_run",
            json!({"action": "observe", "runId": "r7", "cursor": "c3"}),
        )
        .expect("run args decode");
        assert_eq!(
            run,
            ToolCall::Run(RunAction::Observe {
                run: RunId("r7".into()),
                cursor: Some("c3".into()),
            }),
            "observe carries the optional cursor"
        );
    }

    /// The refusal map: params outside `{name, arguments}` and arguments
    /// failing a tool's strict DTO are `REQUEST_INVALID`; a name outside
    /// the served set — unknown or not yet serving — is `TOOL_UNKNOWN`.
    #[test]
    fn decode_call_refusals_are_typed() {
        for params in [
            json!(null),
            json!("x"),
            json!({"arguments": {}}),
            json!({"name": 7}),
            json!({"name": "herdr_status", "arguments": {"surprise": 1}}),
        ] {
            let err = decode_call(caller(), &params).expect_err("malformed call refuses");
            assert_eq!(
                err.code,
                ToolError::REQUEST_INVALID,
                "bad params shape or bad arguments: {params}"
            );
        }
        for name in ["herdr_launch", "herdr_run", "bogus"] {
            let err = decode_call(caller(), &json!({"name": name, "arguments": {}}))
                .expect_err("a name outside the served set refuses");
            assert_eq!(
                err.code,
                ToolError::TOOL_UNKNOWN,
                "unserved or unknown tool: {name}"
            );
        }
    }

    /// The success half of §4.11's `{content, isError}`: `Ok` serializes
    /// the result body as the text content with `isError` false.
    #[test]
    fn call_response_serializes_the_result_body() {
        let response = call_response(json!("r1"), &Ok(json!({"health": {"ok": true}})));
        let Response::Result { result, .. } = response else {
            panic!("an answered call is a result envelope");
        };
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool result text is a string");
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("text is json"),
            json!({"health": {"ok": true}}),
            "the text carries the serialized result body"
        );
    }
}
