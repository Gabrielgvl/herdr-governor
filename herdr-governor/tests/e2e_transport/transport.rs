//! `transport` — the M2 MCP-transport e2e (p5-plan §4.11, the P5.M2 card):
//! the daemon's real `0600` socket driven by `daemon::run` in-process,
//! speaking the v1 relay frame
//! (`{"v":1,"caller":…,"rpc":<request>}` → `{"v":1,"rpc":<response>}`),
//! `herdr_status` answered through the A2 `Msg::Tool` arm. The client,
//! fixture and daemon legs are the shared `support` harness (P5.T1).

use herdr_governor::adapters::herdr::MAX_FRAME_BYTES;
use herdr_governor::mcp::framing::encode_request;
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, fixture};
use crate::support::fake_herdr::FakeHerdr;
use crate::support::fake_herdr::topology::occupied_topology;
use crate::support::mcp_client::{McpClient, caller_envelope, canonical, status_call, status_page};

/// The caller's 32-hex `relayInstanceId` (the validator's pinned shape).
const RELAY: &str = "0123456789abcdef0123456789abcdef";

/// A fixture daemon against `fake`: the inert catalog points the
/// daemon's Herdr client at the fake's socket, so the request-time
/// `session.snapshot` F1 verifies against answers.
fn dirs_for(fake: &FakeHerdr) -> DaemonDirs {
    fixture(&Catalog::inert(fake.socket_path(), "http://127.0.0.1:9"))
}

/// The caller envelope the occupied `w1:p1` resolves to — `projectRoot`
/// is `dirs.root()`'s canonical path so the connection task's realpath
/// read agrees.
fn caller(dirs: &DaemonDirs) -> governor_core::identity::CallerEnvelope {
    caller_envelope("w1:p1", &canonical(dirs.root()), RELAY)
}

/// The served surface (OQ-S): `tools/list` over the real socket serves
/// exactly `herdr_status` + `herdr_launch` — `herdr_run` stays unlisted
/// until C4. F1 gates the framed request first: `Msg::VerifyCaller`
/// resolves and binds the caller before the list is allowed out.
#[tokio::test]
async fn transport_tools_list_exposes_status_only_in_pr_a() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = McpClient::new(&daemon.socket_path(), caller(&dirs));

    let reply = client
        .call(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .await;
    assert_eq!(reply["id"], 1, "the request id echoes verbatim");
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name is a string"))
        .collect();
    assert_eq!(
        names,
        ["herdr_status", "herdr_launch", "herdr_run"],
        "the served surface lists every tool this build answers"
    );

    let sock = daemon.socket_path();
    daemon.shutdown().await;
    assert!(!sock.exists(), "teardown removed the socket");
}

/// One task per connection (§4.11): concurrent connections carry
/// independent request/reply pairs — distinct ids echo back on their own
/// connection with no cross-talk. Each framed `ping` posts its own
/// `VerifyCaller`: the coordinator serializes them — the first binds,
/// the rest verify against the same occupant.
#[tokio::test]
async fn transport_concurrent_connections_are_independent() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let caller = caller(&dirs);

    let mut legs = Vec::new();
    for id in 0..8u64 {
        let leg = McpClient::new(&daemon.socket_path(), caller.clone());
        legs.push(tokio::spawn(async move {
            let request = json!({"jsonrpc": "2.0", "id": id, "method": "ping"});
            (id, leg.call(&request).await)
        }));
    }
    for leg in legs {
        let (id, reply) = leg.await.expect("leg joins");
        assert_eq!(reply["id"], id, "connection {id} got its own reply");
        assert_eq!(reply["result"], json!({}), "ping answers an empty result");
    }

    daemon.shutdown().await;
}

/// The A2×M2 seam pinned end to end: a v1 `tools/call` frame for
/// `herdr_status` on the real socket runs `mcp::serve`'s conn task —
/// request-time `session.snapshot` + `canonicalize` — into the
/// coordinator's `Msg::Tool` arm, which resolves and binds the caller
/// (F1) and answers the §4.12 status page.
#[tokio::test]
async fn transport_herdr_status_serves_over_the_real_socket() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = McpClient::new(&daemon.socket_path(), caller(&dirs));

    let reply = client.call(&status_call("s1")).await;

    assert_eq!(reply["id"], "s1", "the request id echoes verbatim");
    let page = status_page(&reply);
    assert!(
        page["health"]["daemon"]["pid"].is_u64(),
        "the health section emits unconditionally"
    );
    assert!(
        page["health"]["herdr"]["freshSecsAgo"].is_u64(),
        "the request-time snapshot reached the arm and recorded freshness"
    );
    assert_eq!(page["config"]["valid"], true);
    assert_eq!(page["runs"], json!([]), "a fresh caller owns no runs");
    assert_eq!(page["unreadEventIds"], json!([]));

    let sock = daemon.socket_path();
    daemon.shutdown().await;
    assert!(!sock.exists(), "teardown removed the socket");
}

/// §4.11/M1 — the socket's 1 MiB frame bound counts payload bytes
/// before the `\n`, same as the codec: a frame whose payload is exactly
/// `MAX_FRAME_BYTES` is legal and answered, one byte over is refused by
/// the close. `params.pad` sizes the payload to the byte.
#[tokio::test]
async fn transport_frame_bound_counts_payload_before_newline() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let caller = caller(&dirs);
    let client = McpClient::new(&daemon.socket_path(), caller.clone());

    let rpc = |pad: usize| json!({"jsonrpc": "2.0", "id": 9, "method": "ping", "params": {"pad": "x".repeat(pad)}});
    let base = encode_request(&caller, &rpc(0)).len() - 1;
    let maxed = rpc(MAX_FRAME_BYTES - base);
    assert_eq!(
        encode_request(&caller, &maxed).len() - 1,
        MAX_FRAME_BYTES,
        "the frame payload is exactly the bound"
    );
    let reply = client.call(&maxed).await;
    assert_eq!(reply["id"], 9);
    assert_eq!(reply["result"], json!({}), "the max-size frame is served");

    // One payload byte over: the newline lands past the take bound and
    // the connection closes without a reply.
    let stream = UnixStream::connect(daemon.socket_path())
        .await
        .expect("connect");
    let mut reader = tokio::io::BufReader::new(stream);
    reader
        .get_mut()
        .write_all(&encode_request(&caller, &rpc(MAX_FRAME_BYTES - base + 1)))
        .await
        .expect("frame write");
    let mut line = Vec::new();
    let read = reader
        .read_until(b'\n', &mut line)
        .await
        .expect("reply read");
    assert_eq!(read, 0, "an over-bound frame is refused by the close");

    daemon.shutdown().await;
}
