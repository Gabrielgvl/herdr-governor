//! The hand-rolled JSON-RPC 2.0 subset the relay and the daemon share
//! (plan §4.11, OQ-A): envelope parse, the protocol-fault codes,
//! `initialize` version negotiation and `ping`. `tools/list` and
//! `tools/call` are routed by `tools` before [`respond`] is consulted, so
//! an unknown method reaching [`respond`] is always `-32601`. A JSON-RPC
//! error is a protocol fault only — a typed tool refusal travels inside a
//! `tools/call` result (`isError` + a `{"code":…}` body), never here.

use serde::Serialize;
use serde_json::{Value, json};

/// `-32700` — the line is not JSON.
const PARSE_ERROR: i64 = -32700;
/// `-32600` — the line is JSON but not a JSON-RPC 2.0 request object
/// (batch arrays are not in the subset).
const INVALID_REQUEST: i64 = -32600;
/// `-32601` — no such method; the subset is `initialize`,
/// `notifications/initialized`, `tools/list`, `tools/call`, `ping`.
const METHOD_NOT_FOUND: i64 = -32601;
/// `-32602` — the method exists but the params fail its shape.
pub const INVALID_PARAMS: i64 = -32602;
/// `-32603` — the handler itself failed.
pub const INTERNAL_ERROR: i64 = -32603;
/// `-32000` — the server-defined code shared by `RESULT_TOO_LARGE` and
/// `DAEMON_UNAVAILABLE` (plan §4.11, N5/N7).
const SERVER_ERROR: i64 = -32000;

/// N5/§4.12 — a serialized `result` member over 60,000 bytes is refused,
/// never truncated. Handlers page under the §4.12 byte budget so this
/// bound is a tripwire, not a path callers reach.
const RESULT_MAX_BYTES: usize = 60_000;

/// §4.11 — the protocol versions `initialize` may echo back; the newest is
/// also the fallback for an unknown or absent request.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const PROTOCOL_VERSIONS: [&str; 3] = [DEFAULT_PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
const SERVER_NAME: &str = "herdr-governor";

/// One decoded request line. A call carries the `id` its response must
/// echo verbatim (string or number); a notification — a request object
/// without `id` — is never answered and never forwarded (§4.11).
#[derive(Debug)]
pub enum Request {
    /// `{jsonrpc:"2.0", id, method, params?}` — expects a response.
    Call {
        /// The id to echo, verbatim.
        id: Value,
        /// The method name.
        method: String,
        /// The `params` member, `Null` when absent.
        params: Value,
    },
    /// `{jsonrpc:"2.0", method, params?}` with no `id` member.
    Notification {
        /// The method name (`notifications/initialized` is the known one).
        method: String,
    },
}

/// One response line — `{jsonrpc:"2.0", id, result}` or
/// `{jsonrpc:"2.0", id, error{code,message}}`, serialized in that key order.
#[derive(Debug)]
pub enum Response {
    /// A result envelope.
    Result {
        /// The echoed request id.
        id: Value,
        /// The result object.
        result: Value,
    },
    /// A protocol-fault envelope.
    Error {
        /// The echoed request id, `Null` when the request had no usable id.
        id: Value,
        /// The wire code.
        code: i64,
        /// The wire message.
        message: String,
    },
}

/// The shared error-envelope constructor.
#[must_use]
fn error(id: Value, code: i64, message: impl Into<String>) -> Response {
    Response::Error {
        id,
        code,
        message: message.into(),
    }
}

/// Decode one line into a [`Request`]. Every protocol violation is a typed
/// error [`Response`], never a silent drop: non-JSON → `PARSE_ERROR`,
/// anything failing the envelope rules → `INVALID_REQUEST`. The error id
/// echoes the request's id when one could be read, `Null` otherwise.
pub fn parse(line: &[u8]) -> Result<Request, Response> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| error(Value::Null, PARSE_ERROR, format!("Parse error: {e}")))?;
    let Value::Object(map) = &value else {
        return Err(error(
            Value::Null,
            INVALID_REQUEST,
            "request is not an object",
        ));
    };
    // A usable id — string or number — echoes into any error below.
    let id = match map.get("id") {
        None => None,
        Some(raw) if raw.is_string() || raw.is_number() => Some(raw.clone()),
        Some(_) => {
            return Err(error(
                Value::Null,
                INVALID_REQUEST,
                "id is not a string or number",
            ));
        }
    };
    let fallback = id.clone().unwrap_or(Value::Null);
    if map.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(error(fallback, INVALID_REQUEST, "jsonrpc is not \"2.0\""));
    }
    let Some(method) = map.get("method").and_then(Value::as_str) else {
        return Err(error(fallback, INVALID_REQUEST, "method is not a string"));
    };
    match id {
        Some(request_id) => Ok(Request::Call {
            id: request_id,
            method: method.to_owned(),
            params: map.get("params").cloned().unwrap_or(Value::Null),
        }),
        None => Ok(Request::Notification {
            method: method.to_owned(),
        }),
    }
}

/// Serialize one response to its wire bytes — no trailing `\n`; the
/// transports add it. `serde_json::to_vec` on these members cannot fail,
/// so `unwrap_or_default` only quiets the typed `Result`.
#[must_use]
pub fn serialize(response: &Response) -> Vec<u8> {
    let wire = match response {
        Response::Result { id, result } => WireResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Response::Error { id, code, message } => WireResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorBody {
                code: *code,
                message,
            }),
        },
    };
    serde_json::to_vec(&wire).unwrap_or_default()
}

/// The N5 bound applied where every handler result passes (§4.11): a
/// serialized `result` over `RESULT_MAX_BYTES` becomes the `-32000
/// RESULT_TOO_LARGE` protocol fault — the whole body is replaced, never
/// truncated mid-value.
#[must_use]
pub(crate) fn result(id: Value, result: Value) -> Response {
    let bytes = serde_json::to_vec(&result).unwrap_or_default();
    if bytes.len() > RESULT_MAX_BYTES {
        return error(id, SERVER_ERROR, "RESULT_TOO_LARGE");
    }
    Response::Result { id, result }
}

/// §4.11 — `{protocolVersion, capabilities:{tools:{}}, serverInfo}`: echo
/// the client's `protocolVersion` when it is one of the known versions,
/// else the newest.
#[must_use]
fn initialize_result(params: &Value) -> Value {
    let negotiated = match params.get("protocolVersion").and_then(Value::as_str) {
        Some(version) if PROTOCOL_VERSIONS.contains(&version) => version,
        Some(_) | None => DEFAULT_PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": negotiated,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

/// Answer the requests the pure layer owns (§4.11): `initialize` and
/// `ping`; every notification is dropped; anything else — `tools/*`
/// included until `tools` routes it first — is `-32601`.
#[must_use]
pub fn respond(request: &Request) -> Option<Response> {
    match request {
        Request::Notification { .. } => None,
        Request::Call { id, method, params } => {
            let body = match method.as_str() {
                "initialize" => initialize_result(params),
                "ping" => json!({}),
                _ => return Some(error(id.clone(), METHOD_NOT_FOUND, "Method not found")),
            };
            Some(result(id.clone(), body))
        }
    }
}

#[derive(Serialize)]
struct WireResponse<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody<'a>>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: i64,
    message: &'a str,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, RESULT_MAX_BYTES, Request, Response,
        SERVER_ERROR, error, initialize_result, parse, respond, result, serialize,
    };

    fn error_parts(response: &Response) -> (i64, String) {
        match response {
            Response::Error { code, message, .. } => (*code, message.clone()),
            Response::Result { .. } => panic!("expected an error response"),
        }
    }

    #[test]
    fn jsonrpc_parse_and_error_codes() {
        let call = parse(
            br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"herdr_status"}}"#,
        )
        .unwrap_or_else(|_| panic!("a well-formed call parses"));
        let Request::Call { id, method, params } = call else {
            panic!("a request with an id is a call, not a notification");
        };
        assert_eq!(id, json!(7), "the id echoes verbatim");
        assert_eq!(method, "tools/call", "the method parses");
        assert_eq!(
            params,
            json!({"name": "herdr_status"}),
            "the params member parses"
        );

        let note = parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .unwrap_or_else(|_| panic!("a request without id parses"));
        let Request::Notification {
            method: note_method,
        } = note
        else {
            panic!("no id member means notification");
        };
        assert_eq!(
            note_method, "notifications/initialized",
            "the notification method parses"
        );

        let parse_failure = parse(b"not json").expect_err("garbage bytes cannot parse");
        let (parse_code, _) = error_parts(&parse_failure);
        assert_eq!(parse_code, PARSE_ERROR, "non-JSON is a parse error");
        let Response::Error { id: failed_id, .. } = parse_failure else {
            panic!("already matched");
        };
        assert_eq!(
            failed_id,
            Value::Null,
            "a parse error cannot correlate an id"
        );

        let invalid: [(&[u8], &str); 7] = [
            (br"[]", "a batch array is outside the subset"),
            (br"42", "a scalar is not a request"),
            (br#"{"id":1,"method":"ping"}"#, "missing jsonrpc member"),
            (
                br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                "wrong protocol version",
            ),
            (br#"{"jsonrpc":"2.0","id":1}"#, "missing method"),
            (
                br#"{"jsonrpc":"2.0","id":1,"method":3}"#,
                "non-string method",
            ),
            (
                br#"{"jsonrpc":"2.0","id":{"x":1},"method":"ping"}"#,
                "non-scalar id",
            ),
        ];
        for (line, why) in invalid {
            let failure = parse(line).expect_err(why);
            let (failure_code, _) = error_parts(&failure);
            assert_eq!(failure_code, INVALID_REQUEST, "{why} is an invalid request");
        }

        let refused = respond(&Request::Call {
            id: json!(9),
            method: "resources/list".into(),
            params: Value::Null,
        });
        let Some(refusal) = refused else {
            panic!("a call always gets a response");
        };
        let (refusal_code, _) = error_parts(&refusal);
        assert_eq!(
            refusal_code, METHOD_NOT_FOUND,
            "an unknown method is -32601"
        );

        assert!(
            respond(&Request::Notification {
                method: "notifications/initialized".into(),
            })
            .is_none(),
            "a notification is never answered"
        );

        let error_bytes = serialize(&error(json!(3), METHOD_NOT_FOUND, "Method not found"));
        assert_eq!(
            String::from_utf8_lossy(&error_bytes),
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"Method not found"}}"#,
            "the error envelope serializes in jsonrpc,id,error order"
        );
        let result_bytes = serialize(&result(json!("s-1"), json!({})));
        assert_eq!(
            String::from_utf8_lossy(&result_bytes),
            r#"{"jsonrpc":"2.0","id":"s-1","result":{}}"#,
            "the result envelope serializes in jsonrpc,id,result order"
        );
    }

    #[test]
    fn initialize_negotiates_known_versions() {
        for version in ["2025-06-18", "2025-03-26", "2024-11-05"] {
            let negotiated = initialize_result(&json!({"protocolVersion": version}));
            assert_eq!(
                negotiated["protocolVersion"], version,
                "a known client version echoes back"
            );
        }
        for params in [
            json!({"protocolVersion": "1999-01-01"}),
            json!({}),
            Value::Null,
        ] {
            let fallback = initialize_result(&params);
            assert_eq!(
                fallback["protocolVersion"], "2025-06-18",
                "an unknown or absent version falls back to the newest"
            );
        }
        let full = initialize_result(&json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(
            full["capabilities"],
            json!({"tools": {}}),
            "the only capability is tools"
        );
        assert_eq!(
            full["serverInfo"],
            json!({"name": "herdr-governor", "version": env!("CARGO_PKG_VERSION")}),
            "serverInfo names the governor at this build's version"
        );
    }

    #[test]
    fn n5_result_over_60000_bytes_is_refused_not_truncated() {
        let big = json!({"body": "x".repeat(RESULT_MAX_BYTES)});
        let response = result(json!(1), big);
        let (code, message) = error_parts(&response);
        assert_eq!(code, SERVER_ERROR, "an oversized result is -32000");
        assert_eq!(
            message, "RESULT_TOO_LARGE",
            "the refusal carries the named code"
        );
        assert!(
            !String::from_utf8_lossy(&serialize(&response)).contains("xxx"),
            "the refusal carries none of the oversized body"
        );

        let fits = json!({"body": "y".repeat(RESULT_MAX_BYTES - 40)});
        let Response::Result { result: body, .. } = result(json!(1), fits.clone()) else {
            panic!("a result inside the bound passes through");
        };
        assert_eq!(body, fits, "an in-bound result is passed verbatim");
    }
}
