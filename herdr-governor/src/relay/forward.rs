//! `relay/forward` — the v1 relay↔daemon framing and the per-request
//! round trip (p5-plan §4.11): `{"v":1,"caller":…,"rpc":<request>}` out
//! on a fresh `UnixStream`, the `{"v":1,"rpc":<response>}` line back,
//! close. Valid notifications (no `id`) are classified out, never
//! forwarded; protocol faults are answered locally with the daemon's
//! own `-32700`/`-32600`; every failure of the leg maps to
//! `DAEMON_UNAVAILABLE` per method (N7).

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use serde_json::{Value, json};
use thiserror::Error;

use super::identity::Identity;
use crate::adapters::herdr::codec::LineAccumulator;
use crate::mcp::{framing, jsonrpc};

/// One stdin line, classified by the loop's needs.
#[derive(Debug)]
pub(super) enum Inbound {
    /// A valid request object with no `id` member — a JSON-RPC
    /// notification. Never forwarded, never answered.
    Notification,
    /// A request object: forwarded verbatim as the envelope's `rpc`;
    /// the `id` and `method` survive for the failure map.
    Request {
        /// The request's `id`, echoed into every reply shape.
        id: Value,
        /// The request's `method` — only `tools/call` changes the
        /// `DAEMON_UNAVAILABLE` shape.
        method: String,
        /// The whole request object, forwarded as `rpc`.
        request: Value,
    },
    /// A protocol fault `jsonrpc::parse` already typed — `-32700` when
    /// the line is not JSON, `-32600` when it is JSON but not a request
    /// object (non-objects, `id`-less objects failing the request rules,
    /// a non-scalar `id`). Answered locally, never forwarded.
    Fault(jsonrpc::Response),
}

/// Classify one stdin line through the daemon's own envelope rules
/// (`mcp::jsonrpc::parse`): a valid `id`-less request drops, a `Call`
/// forwards verbatim, and every violation is the typed fault the daemon
/// would answer — the same `-32700`/`-32600`, never a silent drop.
pub(super) fn classify(line: &[u8]) -> Inbound {
    match jsonrpc::parse(line) {
        Err(response) => Inbound::Fault(response),
        Ok(jsonrpc::Request::Notification { .. }) => Inbound::Notification,
        Ok(jsonrpc::Request::Call { id, method, .. }) => {
            // `parse` proved the line a request object — the verbatim
            // `Value` forwards as `rpc`: unknown members must ride to
            // the daemon, which re-decodes them itself.
            let request = serde_json::from_slice(line).unwrap_or(Value::Null);
            Inbound::Request {
                id,
                method,
                request,
            }
        }
    }
}

/// The derived identity as the frame codec's caller envelope — three
/// small clones per request on a cold leg.
fn envelope(identity: &Identity) -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId(identity.pane_id.clone()),
        project_root: ProjectRoot(identity.project_root.clone()),
        relay_instance_id: RelayInstanceId(identity.relay_instance_id.clone()),
    }
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
    let frame = framing::encode_request(&envelope(identity), request);
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(&frame)?;
    let mut conn = BufReader::new(stream);
    let rpc = framing::decode_reply(&read_line(&mut conn)?).map_err(|_e| ForwardError::BadReply)?;
    Ok(reply_line(&rpc))
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
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread;

    use serde_json::{Value, json};

    use super::{ForwardError, Inbound, classify, envelope, exchange, unavailable_reply};
    use crate::mcp::framing::encode_request;
    use crate::mcp::jsonrpc;
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
        let bytes = encode_request(&envelope(&identity()), &request);
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

    /// Only a VALID request object without `id` drops as a notification —
    /// every other failure is the typed fault `jsonrpc::parse` assigns:
    /// `id`-less objects failing the request rules, non-scalar ids and
    /// JSON non-objects are `-32600`; non-JSON bytes are `-32700`.
    #[test]
    fn relay_drops_notifications() {
        assert!(
            matches!(
                classify(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}"),
                Inbound::Notification
            ),
            "a valid request without id is a notification",
        );
        assert!(
            matches!(
                classify(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":9}}"),
                Inbound::Notification
            ),
            "a notification with params is still a notification",
        );
        for (line, code) in [
            (b"{}".as_slice(), -32600_i64),
            (b"{\"jsonrpc\":\"2.0\",\"method\":7}".as_slice(), -32600_i64),
            (
                b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}".as_slice(),
                -32600_i64,
            ),
            (b"42".as_slice(), -32600_i64),
            (b"[1,2,3]".as_slice(), -32600_i64),
            (b"not json".as_slice(), -32700_i64),
            (b"".as_slice(), -32700_i64),
        ] {
            let Inbound::Fault(jsonrpc::Response::Error { code: got, id, .. }) = classify(line)
            else {
                panic!("{line:?} must classify as a {code} fault");
            };
            assert_eq!(got, code, "{line:?} is a {code} fault");
            assert!(id.is_null(), "{line:?} has no usable id to echo");
        }
    }

    /// The shared codec's strictness now binds the relay: a reply frame
    /// carrying a member outside `{v, rpc}` is `BadReply` — which
    /// `respond` maps to `DAEMON_UNAVAILABLE` — never unwrapped.
    #[test]
    fn exchange_refuses_a_reply_with_unknown_members() {
        let dir = tempfile::tempdir().expect("tmp");
        let socket = dir.path().join("d.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut line = Vec::new();
            BufReader::new(&mut conn)
                .read_until(b'\n', &mut line)
                .expect("frame read");
            conn.write_all(b"{\"v\":1,\"rpc\":{\"id\":1},\"extra\":true}\n")
                .expect("reply write");
        });
        let err = exchange(&socket, &identity(), &json!({"id": 1}))
            .expect_err("a reply with an unknown member is refused");
        assert!(matches!(err, ForwardError::BadReply));
        server.join().expect("server joins");
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
