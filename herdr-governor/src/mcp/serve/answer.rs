//! `serve/answer` — the per-connection task `spawn` detaches: one
//! bounded line in, one reply out, the socket closed behind it. The v1
//! frame's `rpc` request first posts `Msg::VerifyCaller` — the Tool
//! arm's own F1 resolve/bind for the verdict alone — unless it is
//! `tools/call`, which posts `Msg::Tool` instead; only a passing verdict
//! reaches the local answers (`initialize`, `ping`, `tools/list`,
//! `-32601`). The bare-request dialect (the §4.3 lock-probe) carries no
//! caller envelope and stays unauthenticated.

use std::time::Duration;

use governor_core::identity::CallerEnvelope;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::adapters::herdr::{Client, MAX_FRAME_BYTES};
use crate::daemon::Msg;
use crate::daemon::api::{ToolError, ToolRequest, ToolResponse};
use crate::mcp::framing::{self, Inbound};
use crate::mcp::jsonrpc::{self, Request, Response};
use crate::mcp::tools;

/// One connection: read exactly one bounded line, answer it, close. A
/// line over `MAX_FRAME_BYTES` (no newline inside the bound), a `write`
/// error or EOF ends the connection silently; a second frame can never
/// arrive — the socket is gone after the first reply.
pub(super) async fn conn(
    stream: UnixStream,
    tx: mpsc::Sender<Msg>,
    herdr: Client,
    op_timeout: Duration,
) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let bound = u64::try_from(MAX_FRAME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    let read = (&mut reader).take(bound).read_until(b'\n', &mut line).await;
    match read {
        Ok(n) if n > 0 && line.last() == Some(&b'\n') => {}
        _ => return,
    }
    // The bound counts payload bytes before the `\n` (§4.11): `line`
    // still carries its terminator, so the decode sees it stripped.
    let Some(payload) = line.strip_suffix(b"\n") else {
        return;
    };
    let reply = match framing::decode_request(payload) {
        Ok(inbound) => answer_frame(inbound, &tx, &herdr, op_timeout)
            .await
            .map(|rpc| framing::encode_reply(&rpc)),
        Err(_) => answer_bare(payload).map(|response| {
            let mut bytes = jsonrpc::serialize(&response);
            bytes.push(b'\n');
            bytes
        }),
    };
    if let Some(bytes) = reply {
        let _written = reader.get_mut().write_all(&bytes).await;
    }
}

/// The v1-frame path: decode the forwarded `rpc` request and answer it —
/// the reply is the inner JSON-RPC response `framing::encode_reply`
/// re-envelopes. A notification or a `respond`-less call yields `None`
/// and the connection closes without a reply.
async fn answer_frame(
    inbound: Inbound,
    tx: &mpsc::Sender<Msg>,
    herdr: &Client,
    op_timeout: Duration,
) -> Option<Value> {
    let bytes = serde_json::to_vec(&inbound.rpc).unwrap_or_default();
    let request = match jsonrpc::parse(&bytes) {
        Ok(request) => request,
        Err(response) => return Some(value_of(&response)),
    };
    answer_call(request, inbound.caller, tx, herdr, op_timeout)
        .await
        .map(|response| value_of(&response))
}

/// Route one decoded request: `tools/call` posts `Msg::Tool` (the arm
/// resolves, binds and serves the call); every other framed method posts
/// `Msg::VerifyCaller` — the Tool arm's own F1 resolution for the
/// verdict alone — and only a passing verdict reaches the local answer:
/// `tools/list` through `tools`, `initialize`/`ping` and the `-32601`
/// unknown-method fault through `respond`. A refused verdict is the
/// typed code inside a `-32000` error — the local answers have no
/// tool-result channel to carry `isError`.
async fn answer_call(
    request: Request,
    caller: CallerEnvelope,
    tx: &mpsc::Sender<Msg>,
    herdr: &Client,
    op_timeout: Duration,
) -> Option<Response> {
    let Request::Call { id, method, params } = request else {
        return None;
    };
    match method.as_str() {
        "tools/call" => Some(call(id, caller, &params, tx, herdr, op_timeout).await),
        _ => match verify(&caller, tx, herdr, op_timeout).await {
            Ok(()) if method == "tools/list" => Some(tools::list_response(id)),
            Ok(()) => jsonrpc::respond(&Request::Call { id, method, params }),
            Err(refusal) => Some(Response::Error {
                id,
                code: jsonrpc::SERVER_ERROR,
                message: refusal.code.to_owned(),
            }),
        },
    }
}

/// `tools/call`: strict-decode the params into a `ToolRequest` (a
/// `ToolError` refusal is already the wire answer), gather the
/// request-time evidence and post it to the coordinator, then encode
/// whatever comes back. A coordinator that cannot answer — mailbox
/// closed at shutdown — maps to `DAEMON_UNAVAILABLE` (§4.14 step 1's
/// in-flight rule, N7's code).
async fn call(
    id: Value,
    caller: CallerEnvelope,
    params: &Value,
    tx: &mpsc::Sender<Msg>,
    herdr: &Client,
    op_timeout: Duration,
) -> Response {
    let tool_response = match tools::decode_call(caller, params) {
        Ok(request) => post(request, tx, herdr, op_timeout).await,
        Err(error) => Err(error),
    };
    tools::call_response(id, &tool_response)
}

/// Hand one `ToolRequest` to the coordinator and wait on its `oneshot`.
/// The request-time evidence §4.2/§4.6 assigns the connection task
/// rides the message: a fresh `session.snapshot` — an `Err` here is
/// `DAEMON_UNAVAILABLE` in the arm, never an identity verdict — and the
/// `canonicalize` of the envelope's `projectRoot` (`None` when the path
/// does not resolve, H#3). Either send or receive failing means the
/// coordinator is gone — the reply is the typed `DAEMON_UNAVAILABLE`
/// the relay also fabricates.
async fn post(
    request: ToolRequest,
    tx: &mpsc::Sender<Msg>,
    herdr: &Client,
    op_timeout: Duration,
) -> ToolResponse {
    let (reply, wait) = oneshot::channel();
    let snapshot = herdr.session_snapshot(op_timeout).await;
    let resolved_root = canonical_root(&request.caller.project_root.0).await;
    let msg = Msg::Tool {
        request,
        snapshot: Box::new(snapshot),
        resolved_root,
        reply,
    };
    if tx.send(msg).await.is_err() {
        return Err(unavailable());
    }
    wait.await.unwrap_or_else(|_| Err(unavailable()))
}

/// The framed non-tool request's F1 check — the same request-time
/// evidence `post` gathers, riding `Msg::VerifyCaller` for the verdict
/// alone since the method's answer needs no tool dispatch: a fresh
/// `session.snapshot` (an `Err` is `DAEMON_UNAVAILABLE`, never an
/// identity verdict) and the `canonicalize` of the envelope's
/// `projectRoot`. A dead mailbox is `DAEMON_UNAVAILABLE` here too.
async fn verify(
    caller: &CallerEnvelope,
    tx: &mpsc::Sender<Msg>,
    herdr: &Client,
    op_timeout: Duration,
) -> Result<(), ToolError> {
    let (reply, wait) = oneshot::channel();
    let snapshot = herdr.session_snapshot(op_timeout).await;
    let resolved_root = canonical_root(&caller.project_root.0).await;
    let msg = Msg::VerifyCaller {
        caller: caller.clone(),
        snapshot: Box::new(snapshot),
        resolved_root,
        reply,
    };
    if tx.send(msg).await.is_err() {
        return Err(unavailable());
    }
    wait.await.unwrap_or_else(|_| Err(unavailable()))
}

/// The `realpath` of the relay-attached `projectRoot` (§4.6): `Some`
/// only when the path resolves to a UTF-8 name — the arm compares it to
/// the envelope string and refuses on mismatch, never re-anchored.
async fn canonical_root(project_root: &str) -> Option<String> {
    tokio::fs::canonicalize(project_root)
        .await
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
}

fn unavailable() -> ToolError {
    ToolError::new(
        ToolError::DAEMON_UNAVAILABLE,
        "the coordinator is not serving",
    )
}

/// The bare-request path — the §4.3 lock-probe's dialect. `parse`
/// failures are already the error `Response`; a decoded request answers
/// through `respond` (`ping`/`initialize`, `-32601` otherwise — without a
/// caller envelope no tool can be served); a notification earns silence.
fn answer_bare(line: &[u8]) -> Option<Response> {
    match jsonrpc::parse(line) {
        Ok(request) => jsonrpc::respond(&request),
        Err(response) => Some(response),
    }
}

/// A `Response` as the `Value` a reply frame envelopes — the serialized
/// form round-tripped, since `serialize` owns the member order.
fn value_of(response: &Response) -> Value {
    serde_json::from_slice(&jsonrpc::serialize(response)).unwrap_or_default()
}
