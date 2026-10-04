//! `serve` — the listener half of the MCP transport (p5-plan §4.11,
//! ADR-0004): the accept loop on the `0600` socket plus one task per
//! connection. A connection gets exactly one bounded line, exactly one
//! reply, then a close — a second frame is refused by the close itself.
//!
//! Two line shapes are legal. The relay's v1 frame
//! (`{"v":1,"caller":…,"rpc":<request>}`) is the real path: every framed
//! request first proves its caller — F1 binds `relayInstanceId` on the
//! FIRST framed request and verifies it on every later one. `tools/call`
//! posts `Msg::Tool`, whose arm runs the request-time snapshot +
//! resolve/bind and serves the call; every other method posts
//! `Msg::VerifyCaller`, the same resolution for the verdict alone, then
//! `initialize`, `ping` and `tools/list` answer locally — a refused
//! caller is a `-32000` JSON-RPC error carrying the typed code, since
//! these answers have no tool-result channel for `isError` — and an
//! unknown method is `-32601`. A bare JSON-RPC request line is the §4.3
//! lock-probe's vocabulary (`ping` → `{}`): it is answered through
//! `jsonrpc::respond` only — it carries no caller envelope and stays
//! unauthenticated. Every result passes `jsonrpc::result`, so the
//! 60,000-byte bound (N5) is enforced here by construction.
//!
//! Admission stops when the coordinator's shutdown `watch` flips
//! (§4.14 step 1): the accept loop returns instead of taking the next
//! connection, and an in-flight connection whose `Msg::Tool` is still
//! queued reads the dropped `oneshot` as `DAEMON_UNAVAILABLE`.

use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::adapters::herdr::Client;
use crate::daemon::Msg;

/// `answer` — the per-connection task: one bounded line, one reply,
/// one close, and F1's verify-then-answer routing.
mod answer;
use answer::conn;

/// Spawn the accept task (§4.3 step 8). Runs until the shutdown `watch`
/// flips or the handle is aborted at teardown; per-connection handlers
/// are detached tasks so a wedged client never stalls `accept`. `herdr`
/// and `op_timeout` equip each connection to take its request-time
/// `session.snapshot` (§4.11).
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
    herdr: Client,
    op_timeout: Duration,
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
                            tokio::spawn(conn(
                                stream,
                                tx.clone(),
                                herdr.clone(),
                                op_timeout,
                            ));
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;

    use super::{conn, spawn};
    use crate::adapters::herdr::Client;
    use crate::daemon::Msg;
    use crate::daemon::api::{ToolError, ToolResponse};
    use crate::mcp::framing::{decode_reply, encode_request};

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// A client pointed nowhere — snapshot reads fail fast and the `Err`
    /// rides the message, which is what these tests pin.
    fn client() -> Client {
        Client::new("/nonexistent/herdr.sock")
    }

    fn caller() -> CallerEnvelope {
        CallerEnvelope {
            pane_id: PaneId("w6:pKQ".into()),
            project_root: ProjectRoot("/repo".into()),
            relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
        }
    }

    /// One leg over an in-memory pair: write `frame`, return the reply
    /// line plus the stream for a follow-up read. For the bare-probe path
    /// only — a framed request posts `VerifyCaller` first and needs its
    /// verdict answered (`verified_leg` below).
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

    /// Answer the `VerifyCaller` a framed non-tool request posts — F1
    /// gates every framed method on the coordinator's verdict before its
    /// local answer is allowed out.
    async fn answer_verify(rx: &mut mpsc::Receiver<Msg>) {
        let Some(Msg::VerifyCaller { reply, .. }) = rx.recv().await else {
            panic!("a framed non-tool request posts VerifyCaller");
        };
        reply.send(Ok(())).expect("the verdict reaches the task");
    }

    /// One verified leg: write `frame`, answer the `VerifyCaller` it
    /// posts, return the reply line.
    async fn verified_leg(
        stream: &mut BufReader<UnixStream>,
        rx: &mut mpsc::Receiver<Msg>,
        frame: &[u8],
    ) -> Vec<u8> {
        stream
            .get_mut()
            .write_all(frame)
            .await
            .expect("frame write");
        answer_verify(rx).await;
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
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx, client(), TIMEOUT));
        let mut client = BufReader::new(client_stream);

        let rpc = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
        let line = verified_leg(&mut client, &mut rx, &encode_request(&caller(), &rpc)).await;
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
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx, client(), TIMEOUT));
        let mut client = BufReader::new(client_stream);

        let first = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let line = verified_leg(&mut client, &mut rx, &encode_request(&caller(), &first)).await;
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

    /// F1 — a framed non-tool request whose `VerifyCaller` refuses answers
    /// a `-32000` JSON-RPC error carrying the typed refusal code as the
    /// message: `initialize`, `ping` and `tools/list` have no tool-result
    /// channel for `isError` to ride. The envelope and the request-time
    /// evidence post verbatim to the coordinator.
    #[tokio::test]
    async fn serve_maps_a_refused_verify_to_a_typed_server_error() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx, client(), TIMEOUT));
        let mut client = BufReader::new(client_stream);

        let rpc = json!({"jsonrpc": "2.0", "id": "p1", "method": "ping"});
        client
            .get_mut()
            .write_all(&encode_request(&caller(), &rpc))
            .await
            .expect("frame write");
        let Some(Msg::VerifyCaller {
            caller: seen,
            snapshot,
            reply,
            ..
        }) = rx.recv().await
        else {
            panic!("the framed ping posts VerifyCaller");
        };
        assert_eq!(seen.pane_id.0, "w6:pKQ", "the envelope rides verbatim");
        assert!(
            snapshot.is_err(),
            "the request-time snapshot failure rides as evidence"
        );
        reply
            .send(Err(ToolError::new(
                "CALLER_IDENTITY_MISMATCH",
                "the pane's occupant changed",
            )))
            .expect("the verdict reaches the task");

        let mut line = Vec::new();
        client
            .read_until(b'\n', &mut line)
            .await
            .expect("reply read");
        let wire = decode_reply(&line).expect("a v1 reply frame");
        assert_eq!(wire["error"]["code"], -32000);
        assert_eq!(
            wire["error"]["message"], "CALLER_IDENTITY_MISMATCH",
            "the typed refusal code is the error message"
        );
        task.await.expect("conn task joins");
    }

    /// A `tools/call` posts `Msg::Tool` to the coordinator mailbox with
    /// the request-time evidence the arm judges on — the fresh
    /// `session.snapshot` (an `Err` rides, never short-circuits) and the
    /// `canonicalize` of the envelope's `projectRoot` — and the
    /// `ToolResponse` its `oneshot` carries comes back as the wire
    /// result. The caller/argument decode itself is M1's
    /// `decode_call_maps_arguments_to_typed_calls` — this proves the
    /// post-and-answer plumbing the coordinator sits behind.
    #[tokio::test]
    async fn serve_posts_tool_call_and_answers_the_oneshot() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx, client(), TIMEOUT));
        let mut client = BufReader::new(client_stream);

        let root = tempfile::tempdir().expect("tmp");
        let canonical = root.path().canonicalize().expect("canonicalize");
        let root_str = canonical.to_str().expect("utf8").to_owned();
        let caller = CallerEnvelope {
            project_root: ProjectRoot(root_str.clone()),
            ..caller()
        };
        let rpc = json!({
            "jsonrpc": "2.0", "id": "c1", "method": "tools/call",
            "params": {"name": "herdr_status", "arguments": {"cursor": "c9"}},
        });
        let write = tokio::spawn({
            let frame = encode_request(&caller, &rpc);
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

        let Some(Msg::Tool {
            snapshot,
            prepared,
            reply,
            ..
        }) = rx.recv().await
        else {
            panic!("the coordinator mailbox got the tool call");
        };
        assert!(
            snapshot.is_err(),
            "a failed snapshot rides the message — never an identity verdict"
        );
        assert_eq!(
            prepared.resolved_root.as_deref(),
            Some(root_str.as_str()),
            "the connection task canonicalizes the envelope's projectRoot"
        );
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
    /// `{}` reply — no v1 envelope, no `VerifyCaller`, the unauthenticated
    /// path F1 leaves alone — then the close.
    #[tokio::test]
    async fn serve_answers_the_bare_lock_probe() {
        let (server, client_stream) = UnixStream::pair().expect("pair");
        let (tx, _rx) = mpsc::channel::<Msg>(8);
        let task = tokio::spawn(conn(server, tx, client(), TIMEOUT));
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
        let accept = spawn(listener, tx, stop_rx, client(), TIMEOUT);

        stop_tx.send(true).expect("watch flips");
        tokio::time::timeout(Duration::from_secs(5), accept)
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
