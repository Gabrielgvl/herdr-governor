//! `relay/forward` — the v1 relay↔daemon framing and the per-request
//! round trip (p5-plan §4.11): `{"v":1,"caller":…,"rpc":<request>}` out
//! on a fresh `UnixStream`, the `{"v":1,"rpc":<response>}` line back,
//! close. Notifications (no `id`) are classified out, never forwarded;
//! every failure of the leg maps to `DAEMON_UNAVAILABLE` per method (N7).

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use super::identity::Identity;
use crate::adapters::herdr::codec::LineAccumulator;

/// One stdin line, classified by the members the loop needs.
#[derive(Debug)]
pub(super) enum Inbound {
    /// No `id` member — a JSON-RPC notification. Never forwarded, never
    /// answered.
    Notification,
    /// `id` present (any JSON-RPC-legal value): forwarded verbatim as
    /// the envelope's `rpc`; the `id` and `method` survive for the
    /// failure map.
    Request {
        /// The request's `id`, echoed into every reply shape.
        id: Value,
        /// The request's `method` ("" when absent or not a string) —
        /// only `tools/call` changes the `DAEMON_UNAVAILABLE` shape.
        method: String,
        /// The whole request object, forwarded as `rpc`.
        request: Value,
    },
    /// Not a decodable JSON object — a protocol fault answered locally
    /// with `-32700`, never forwarded.
    Malformed,
}

/// Classify one stdin line. Anything the relay cannot see an `id`/`method`
/// in is either a notification to drop or a parse fault to answer — it is
/// never wrapped into an envelope it does not describe.
pub(super) fn classify(line: &[u8]) -> Inbound {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Inbound::Malformed;
    };
    let Value::Object(map) = value else {
        return Inbound::Malformed;
    };
    let Some(id) = map.get("id").cloned() else {
        return Inbound::Notification;
    };
    let method = map
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Inbound::Request {
        id,
        method,
        request: Value::Object(map),
    }
}

/// The relay→daemon frame (§4.11): the v1 envelope carrying the F1
/// caller and the request verbatim.
#[derive(Serialize)]
struct Frame<'a> {
    v: u8,
    caller: Caller<'a>,
    rpc: &'a Value,
}

/// The F1 caller envelope: relay-attached framing, never a tool
/// argument.
#[derive(Serialize)]
struct Caller<'a> {
    #[serde(rename = "paneId")]
    pane_id: &'a str,
    #[serde(rename = "projectRoot")]
    project_root: &'a str,
    #[serde(rename = "relayInstanceId")]
    relay_instance_id: &'a str,
}

/// One request's newline-terminated v1 frame. `None` where the envelope
/// itself cannot serialize — handled like a failed leg by the caller.
pub(super) fn frame(identity: &Identity, request: &Value) -> Option<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&Frame {
        v: 1,
        caller: Caller {
            pane_id: &identity.pane_id,
            project_root: &identity.project_root,
            relay_instance_id: &identity.relay_instance_id,
        },
        rpc: request,
    })
    .ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

/// Every way the daemon leg can fail; the caller maps all of them to
/// `DAEMON_UNAVAILABLE` (N7).
#[derive(Debug, Error)]
pub(super) enum ForwardError {
    /// Connect, write or read failure on the daemon socket — a down or
    /// gone daemon.
    #[error("daemon socket io: {0}")]
    Io(#[from] io::Error),
    /// A reply that is not a `{"v":1,"rpc":…}` line: over the 1 MiB
    /// bound, not JSON, wrong version, or no `rpc` member.
    #[error("daemon reply is not a v1 rpc envelope")]
    BadReply,
}

/// One request's whole leg: frame, fresh connect, write, read the reply
/// line, close. Returns the daemon's `rpc` payload serialized and
/// newline-terminated — exactly what the harness is owed on stdio.
pub(super) fn exchange(
    socket: &Path,
    identity: &Identity,
    request: &Value,
) -> Result<Vec<u8>, ForwardError> {
    let frame = frame(identity, request).ok_or(ForwardError::BadReply)?;
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(&frame)?;
    let mut conn = BufReader::new(stream);
    unwrap_rpc(&read_line(&mut conn)?)
}

/// Read one bounded line off the daemon connection; a clean EOF before
/// any line is an `UnexpectedEof` transport failure.
fn read_line(conn: &mut BufReader<UnixStream>) -> Result<Vec<u8>, ForwardError> {
    let mut acc = LineAccumulator::new();
    loop {
        if let Some(line) = acc.take_line().map_err(|_e| ForwardError::BadReply)? {
            return Ok(line);
        }
        let chunk = conn.fill_buf()?;
        if chunk.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon closed without a reply",
            )
            .into());
        }
        acc.push(chunk).map_err(|_e| ForwardError::BadReply)?;
        let used = chunk.len();
        conn.consume(used);
    }
}

/// Unwrap `{"v":1,"rpc":<response>}` and re-serialize `<response>` — the
/// only part the harness ever sees.
fn unwrap_rpc(line: &[u8]) -> Result<Vec<u8>, ForwardError> {
    let value: Value = serde_json::from_slice(line).map_err(|_e| ForwardError::BadReply)?;
    if value.get("v") != Some(&json!(1)) {
        return Err(ForwardError::BadReply);
    }
    let rpc = value.get("rpc").ok_or(ForwardError::BadReply)?;
    let mut bytes = serde_json::to_vec(rpc).map_err(|_e| ForwardError::BadReply)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Serialize a reply value plus its newline; a `Value` always
/// serializes, so the fallback is only a type-level obligation.
fn reply_line(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// The N7 failure map: for `tools/call` the refusal rides the
/// tool-result channel — an `isError` result whose text carries
/// `{"code":"DAEMON_UNAVAILABLE"}` (a typed refusal is a tool result,
/// never a JSON-RPC error, §4.11); every other method gets the JSON-RPC
/// `-32000` error.
pub(super) fn unavailable_reply(id: &Value, method: &str) -> Vec<u8> {
    let reply = if method == "tools/call" {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": "{\"code\":\"DAEMON_UNAVAILABLE\"}"}],
                "isError": true,
            },
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": "DAEMON_UNAVAILABLE"},
        })
    };
    reply_line(&reply)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Inbound, classify, frame, unavailable_reply};
    use crate::relay::identity::Identity;

    fn identity() -> Identity {
        Identity {
            pane_id: "w0:pAA".to_owned(),
            project_root: "/repo".to_owned(),
            relay_instance_id: "ab".repeat(16),
        }
    }

    /// The v1 envelope is exactly `{"v":1,"caller":{…},"rpc":<request>}`
    /// newline-terminated — the daemon decodes these exact member names.
    #[test]
    fn relay_frame_shape() {
        let request = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {}});
        let bytes = frame(&identity(), &request).expect("frame serializes");
        assert!(bytes.ends_with(b"\n"), "frames are newline-terminated");
        let decoded: Value =
            serde_json::from_slice(&bytes[..bytes.len() - 1]).expect("frame is json");
        assert_eq!(
            decoded,
            json!({
                "v": 1,
                "caller": {
                    "paneId": "w0:pAA",
                    "projectRoot": "/repo",
                    "relayInstanceId": "abababababababababababababababab",
                },
                "rpc": request,
            }),
            "the v1 envelope shape is pinned",
        );
    }

    /// Notifications are dropped: no `id` member means no connection and
    /// no reply. An explicit `id: null` is a request — the member is
    /// present — and a non-object line is a local parse fault.
    #[test]
    fn relay_drops_notifications() {
        assert!(
            matches!(
                classify(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}"),
                Inbound::Notification
            ),
            "a line without id is a notification",
        );
        assert!(
            matches!(
                classify(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":9}}"),
                Inbound::Notification
            ),
            "a notification with params is still a notification",
        );
        assert!(
            matches!(
                classify(b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}"),
                Inbound::Request { .. }
            ),
            "an explicit null id is a request, not a notification",
        );
        assert!(
            matches!(classify(b"not json"), Inbound::Malformed),
            "a non-JSON line is a parse fault",
        );
        assert!(
            matches!(classify(b"[1,2,3]"), Inbound::Malformed),
            "a JSON non-object is a parse fault",
        );
        assert!(
            matches!(classify(b""), Inbound::Malformed),
            "an empty line is a parse fault",
        );
    }

    /// N7 — the daemon-down answer keeps the request's `id` and shapes
    /// per method: `tools/call` gets the tool-result `isError` form,
    /// everything else the JSON-RPC `-32000` error.
    #[test]
    fn relay_maps_connect_failure_per_method() {
        let reply: Value =
            serde_json::from_slice(&unavailable_reply(&json!(3), "tools/call")).expect("json");
        assert_eq!(reply["id"], 3);
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["type"], "text");
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("tool error text is a string");
        let code: Value = serde_json::from_str(text).expect("tool error text is json");
        assert_eq!(code["code"], "DAEMON_UNAVAILABLE");

        for method in ["initialize", "tools/list", "ping"] {
            let refused: Value =
                serde_json::from_slice(&unavailable_reply(&json!("req-9"), method)).expect("json");
            assert_eq!(refused["id"], "req-9");
            assert_eq!(refused["error"]["code"], -32000);
            assert_eq!(refused["error"]["message"], "DAEMON_UNAVAILABLE");
        }
    }
}
