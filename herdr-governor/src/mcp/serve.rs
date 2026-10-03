//! `serve` — the listener half of the MCP transport (p5-plan §4.11,
//! ADR-0004): the accept loop on the `0600` socket plus one task per
//! connection. A connection gets exactly one bounded line, exactly one
//! reply, then a close — a second frame is refused by the close itself.
//!
//! Two line shapes are legal. The relay's v1 frame
//! (`{"v":1,"caller":…,"rpc":<request>}`) is the real path: `initialize`
//! and `ping` answer locally, `tools/list`/`tools/call` route through
//! `tools` — a call posts `Msg::Tool` to the coordinator and its
//! `ToolResponse` comes back as the `isError`/`content` result — and
//! everything else is `-32601`. A bare JSON-RPC request line is the §4.3
//! lock-probe's vocabulary (`ping` → `{}`): it is answered through
//! `jsonrpc::respond` only — no caller envelope exists to serve a tool.
//! Every result passes `jsonrpc::result`, so the 60,000-byte bound (N5)
//! is enforced here by construction.
//!
//! Admission stops when the coordinator's shutdown `watch` flips
//! (§4.14 step 1): the accept loop returns instead of taking the next
//! connection, and an in-flight connection whose `Msg::Tool` is still
//! queued reads the dropped `oneshot` as `DAEMON_UNAVAILABLE`.

use std::time::Duration;

use governor_core::identity::CallerEnvelope;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::framing::{self, Inbound};
use super::jsonrpc::{self, Request, Response};
use super::tools;
use crate::adapters::herdr::MAX_FRAME_BYTES;
use crate::daemon::Msg;
use crate::daemon::api::{ToolError, ToolRequest, ToolResponse};

/// Spawn the accept task (§4.3 step 8). Runs until the shutdown `watch`
/// flips or the handle is aborted at teardown; per-connection handlers
/// are detached tasks so a wedged client never stalls `accept`.
///
/// `select!` expands to an internal `%` over its arms — the written arms
/// hold no arithmetic.
#[expect(
    clippy::integer_division_remainder_used,
    reason = "tokio::select! expands to an internal `%` index over its arms; the written arms have no arithmetic"
)]
pub(crate) fn spawn(
    listener: UnixListener,
    tx: mpsc::Sender<Msg>,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // §4.14 step 1 — stop admission first: a flipped (or
                // dropped) watch means the coordinator is shutting down
                // and the next accept has nothing to serve.
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            tokio::spawn(conn(stream, tx.clone()));
                        }
                        Err(_) => {
                            // EMFILE/EAGAIN-style storms: yield briefly,
                            // keep accepting — the listener is still valid.
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
    })
}

/// One connection: read exactly one bounded line, answer it, close. A
/// line over `MAX_FRAME_BYTES` (no newline inside the bound), a `write`
/// error or EOF ends the connection silently; a second frame can never
/// arrive — the socket is gone after the first reply.
async fn conn(stream: UnixStream, tx: mpsc::Sender<Msg>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let bound = u64::try_from(MAX_FRAME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    let read = (&mut reader).take(bound).read_until(b'\n', &mut line).await;
    match read {
        Ok(n) if n > 0 && line.last() == Some(&b'\n') => {}
        _ => return,
    }
    let reply = match framing::decode_request(&line) {
        Ok(inbound) => answer_frame(inbound, &tx)
            .await
            .map(|rpc| framing::encode_reply(&rpc)),
        Err(_) => answer_bare(&line).map(|response| {
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
async fn answer_frame(inbound: Inbound, tx: &mpsc::Sender<Msg>) -> Option<Value> {
    let bytes = serde_json::to_vec(&inbound.rpc).unwrap_or_default();
    let request = match jsonrpc::parse(&bytes) {
        Ok(request) => request,
        Err(response) => return Some(value_of(&response)),
    };
    answer_call(request, inbound.caller, tx)
        .await
        .map(|response| value_of(&response))
}

/// Route one decoded request: `tools/list` and `tools/call` are the MCP
/// surface (`tools` owns both mappings and the caller envelope rides in
/// the frame, never the params); `initialize`/`ping`/anything else is
/// `respond`'s protocol vocabulary.
async fn answer_call(
    request: Request,
    caller: CallerEnvelope,
    tx: &mpsc::Sender<Msg>,
) -> Option<Response> {
    let Request::Call { id, method, params } = request else {
        return None;
    };
    match method.as_str() {
        "tools/list" => Some(tools::list_response(id)),
        "tools/call" => Some(call(id, caller, &params, tx).await),
        _ => jsonrpc::respond(&Request::Call { id, method, params }),
    }
}

/// `tools/call`: strict-decode the params into a `ToolRequest` (a
/// `ToolError` refusal is already the wire answer), post it to the
/// coordinator, and encode whatever comes back. A coordinator that
/// cannot answer — mailbox closed at shutdown — maps to
/// `DAEMON_UNAVAILABLE` (§4.14 step 1's in-flight rule, N7's code).
async fn call(
    id: Value,
    caller: CallerEnvelope,
    params: &Value,
    tx: &mpsc::Sender<Msg>,
) -> Response {
    let tool_response = match tools::decode_call(caller, params) {
        Ok(request) => post(request, tx).await,
        Err(error) => Err(error),
    };
    tools::call_response(id, &tool_response)
}

/// Hand one `ToolRequest` to the coordinator and wait on its `oneshot`.
/// Either send or receive failing means the coordinator is gone — the
/// reply is the typed `DAEMON_UNAVAILABLE` the relay also fabricates.
async fn post(request: ToolRequest, tx: &mpsc::Sender<Msg>) -> ToolResponse {
    let (reply, wait) = oneshot::channel();
    if tx.send(Msg::Tool { request, reply }).await.is_err() {
        return Err(unavailable());
    }
    wait.await.unwrap_or_else(|_| Err(unavailable()))
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

#[cfg(test)]
mod tests {
    use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;

    use super::{conn, spawn};
    use crate::daemon::Msg;
    use crate::daemon::api::ToolResponse;
    use crate::mcp::framing::{decode_reply, encode_request};

    fn caller() -> CallerEnvelope {
        CallerEnvelope {
            pane_id: PaneId("w6:pKQ".into()),
            project_root: ProjectRoot("/repo".into()),
            relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
        }
    }

    /// One leg over an in-memory pair: write `frame`, return the reply
    /// line plus the stream for a follow-up read.
    async fn leg(stream: &mut BufReader<UnixStream>, frame: &[u8]) -> Vec<u8> {
        stream
            .get_mut()
            .write_all(frame)
            .await
            .expect("frame write");
        let mut line = Vec::new();
        let read = stream
            .read_until(b'\n', &mut line)
            .await
            .expect("reply read");
        assert!(read > 0, "the server closed without a reply");
        line
    }

    /// One request, one reply, close: the reply is the v1-enveloped
    /// JSON-RPC answer and the next read hits EOF.
    #[tokio::test]
    async fn serve_closes_after_one_reply() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, _rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx));
        let mut client = BufReader::new(client_stream);

        let rpc = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
        let line = leg(&mut client, &encode_request(&caller(), &rpc)).await;
        let reply = decode_reply(&line).expect("a v1 reply frame");
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"], json!({}), "ping answers an empty result");

        let mut extra = Vec::new();
        let trailing = client
            .read_until(b'\n', &mut extra)
            .await
            .expect("trailing read");
        assert_eq!(trailing, 0, "the connection is closed after the reply");
        task.await.expect("conn task joins");
    }

    /// A second frame on the same connection is refused: the server
    /// writes its one reply and closes, so the second frame can only
    /// meet EOF — never a second reply.
    #[tokio::test]
    async fn serve_refuses_second_frame_on_a_connection() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, _rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx));
        let mut client = BufReader::new(client_stream);

        let first = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let line = leg(&mut client, &encode_request(&caller(), &first)).await;
        assert_eq!(decode_reply(&line).expect("reply")["id"], 1);

        let second = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
        // The write may still land in the closing socket's buffer; what
        // must never arrive is a second reply.
        let _sent = client
            .get_mut()
            .write_all(&encode_request(&caller(), &second))
            .await;
        let mut extra = Vec::new();
        let trailing = client
            .read_until(b'\n', &mut extra)
            .await
            .expect("trailing read");
        assert_eq!(trailing, 0, "no second reply — the connection refused");
        task.await.expect("conn task joins");
    }

    /// A `tools/call` posts `Msg::Tool` to the coordinator mailbox and
    /// the `ToolResponse` its `oneshot` carries comes back as the wire
    /// result. The caller/argument decode itself is M1's
    /// `decode_call_maps_arguments_to_typed_calls` — this proves the
    /// post-and-answer plumbing the coordinator sits behind.
    #[tokio::test]
    async fn serve_posts_tool_call_and_answers_the_oneshot() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx));
        let mut client = BufReader::new(client_stream);

        let rpc = json!({
            "jsonrpc": "2.0", "id": "c1", "method": "tools/call",
            "params": {"name": "herdr_status", "arguments": {"cursor": "c9"}},
        });
        let write = tokio::spawn({
            let frame = encode_request(&caller(), &rpc);
            async move {
                client.get_mut().write_all(&frame).await.expect("write");
                let mut line = Vec::new();
                client
                    .read_until(b'\n', &mut line)
                    .await
                    .expect("reply read");
                line
            }
        });

        let Some(Msg::Tool { reply, .. }) = rx.recv().await else {
            panic!("the coordinator mailbox got the tool call");
        };
        let response: ToolResponse = Ok(json!({"health": {"ok": true}}));
        reply.send(response).expect("oneshot answers");

        let line = write.await.expect("client leg joins");
        let wire = decode_reply(&line).expect("a v1 reply frame");
        assert_eq!(wire["result"]["isError"], false);
        let text = wire["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text");
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("tool body"),
            json!({"health": {"ok": true}}),
            "the coordinator's body comes back verbatim"
        );
        task.await.expect("conn task joins");
    }

    /// The §4.3 lock-probe's dialect: a bare JSON-RPC `ping` gets a bare
    /// `{}` reply — no v1 envelope — then the close.
    #[tokio::test]
    async fn serve_answers_the_bare_lock_probe() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, _rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx));
        let mut client = BufReader::new(client_stream);

        let line = leg(
            &mut client,
            br#"{"jsonrpc":"2.0","id":"gov:probe","method":"ping"}
"#
            .as_slice(),
        )
        .await;
        let reply: Value = serde_json::from_slice(&line).expect("a bare reply");
        assert_eq!(reply["id"], "gov:probe");
        assert_eq!(reply["result"], json!({}));
        task.await.expect("conn task joins");
    }

    /// Admission stops on the shutdown watch: once the flag flips, the
    /// accept task returns instead of taking the next connection.
    #[tokio::test]
    async fn serve_stops_admitting_on_shutdown() {
        let dir = tempfile::tempdir().expect("tmp");
        let listener = tokio::net::UnixListener::bind(dir.path().join("g.sock")).expect("bind");
        let (tx, _rx) = mpsc::channel::<Msg>(8);
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let accept = spawn(listener, tx, stop_rx);

        stop_tx.send(true).expect("watch flips");
        tokio::time::timeout(std::time::Duration::from_secs(5), accept)
            .await
            .expect("the accept task exits on shutdown")
            .expect("accept joins");
        assert!(
            UnixStream::connect(dir.path().join("g.sock"))
                .await
                .is_err(),
            "a dropped listener refuses new connections"
        );
    }
}
