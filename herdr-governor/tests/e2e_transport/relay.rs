//! `relay` — the S30/S31/S32 e2e (P5.A4): the real `herdr-governor
//! relay` subprocess on piped stdio in front of a real `daemon::run`
//! and a `FakeHerdr`. R1's `relay_e2e` proved the subprocess's own
//! contract against a scripted socket; these tests aim the same child
//! at the real daemon so the two binaries' v1 framing and their
//! caller/tool surfaces compose end to end. `transport`/`identity`
//! cover the socket directly and cannot see a framing drift only the
//! shipped relay produces.

use std::fs;

use serde_json::{Value, json};

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, bindings, fixture};
use crate::support::fake_herdr::topology::{occupant, occupied_topology};
use crate::support::fake_herdr::{FakeHerdr, Topology};
use crate::support::mcp_client::{
    RelayClient, call_request, notification, request, status_call, status_page, tool_code,
};

/// A fixture daemon against `fake` — the inert catalog points the
/// daemon's Herdr client at the fake's socket.
fn dirs_for(fake: &FakeHerdr) -> DaemonDirs {
    fixture(&Catalog::inert(fake.socket_path(), "http://127.0.0.1:9"))
}

/// S30 — the real relay against the real daemon: the harness's own
/// request vocabulary round-trips through the v1 frame both ends
/// implement, in send order, with notifications never producing a
/// reply line. A framing drift between the two binaries — a member
/// renamed, `v` bumped, the newline dropped — surfaces here as a
/// refused leg; the fake-socket suite cannot see it (it accepts
/// anything) and the direct-socket suite cannot see it (it frames by
/// hand).
#[tokio::test]
async fn s30_relay_round_trip_against_the_real_daemon() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let replies = relay
        .exchange_all(
            &[
                request(1, "initialize", &json!({"protocolVersion": "2025-06-18"})),
                notification("notifications/initialized"),
                request(3, "tools/list", &json!({})),
                status_call(4),
                request(5, "ping", &json!({})),
            ],
            4,
        )
        .await;

    let init = &replies[0];
    assert_eq!(init["id"], 1, "the request id echoes verbatim");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "herdr-governor");

    let list = &replies[1];
    assert_eq!(
        list["id"], 3,
        "the notification produced no line — the next reply is the request after it"
    );
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(
        names,
        ["herdr_status", "herdr_launch", "herdr_run"],
        "the served surface lists every tool this build answers"
    );

    let page = status_page(&replies[2]);
    assert_eq!(replies[2]["id"], 4);
    assert!(page["health"]["daemon"]["pid"].is_u64());
    assert!(
        page["health"]["herdr"]["freshSecsAgo"].is_u64(),
        "the request-time snapshot lands in health: {page}"
    );

    assert_eq!(replies[3]["id"], 5);
    assert_eq!(replies[3]["result"], json!({}), "ping answers empty");

    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
    daemon.shutdown().await;
}

/// S31 — the strict-schema surface through the real relay: every
/// refusal shape the daemon produces crosses the v1 frame intact —
/// `REQUEST_INVALID`/`TOOL_UNKNOWN` as `isError` tool results, JSON-RPC
/// faults as error envelopes — and a served page stays inside the
/// 60,000-byte result bound.
#[tokio::test]
async fn s31_strict_schemas_refuse_through_the_relay() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let replies = relay
        .exchange_all(
            &[
                call_request(10, "herdr_status", &json!({"surprise": 1})),
                call_request(11, "herdr_status", &json!({"eventId": 7})),
                json!({"jsonrpc": "2.0", "id": 12, "method": "tools/call"}),
                call_request(13, "bogus", &json!({})),
                call_request(14, "herdr_run", &json!({})),
                request(15, "bogus/unknown", &json!({})),
                json!({"id": 16, "method": "ping"}),
                status_call(17),
            ],
            8,
        )
        .await;

    let refused = |reply: &Value, id: u64| {
        assert_eq!(reply["id"], id, "the request id echoes verbatim");
        tool_code(reply)
    };
    assert_eq!(
        refused(&replies[0], 10),
        "REQUEST_INVALID",
        "unknown arguments member"
    );
    assert_eq!(
        refused(&replies[1], 11),
        "REQUEST_INVALID",
        "wrong argument type"
    );
    assert_eq!(
        refused(&replies[2], 12),
        "REQUEST_INVALID",
        "call params missing"
    );
    assert_eq!(
        refused(&replies[3], 13),
        "TOOL_UNKNOWN",
        "unknown tool name"
    );
    assert_eq!(
        refused(&replies[4], 14),
        "REQUEST_INVALID",
        "herdr_run is served — empty arguments fail its strict DTO"
    );
    assert_eq!(replies[5]["id"], 15);
    assert_eq!(
        replies[5]["error"]["code"], -32601,
        "an unknown JSON-RPC method is a protocol fault"
    );
    assert_eq!(replies[6]["id"], 16);
    assert_eq!(
        replies[6]["error"]["code"], -32600,
        "a request without jsonrpc is an invalid-request fault"
    );

    let page = status_page(&replies[7]);
    assert_eq!(replies[7]["id"], 17);
    assert!(page["health"]["daemon"]["pid"].is_u64());
    let result_len = serde_json::to_vec(&replies[7]["result"])
        .expect("result serializes")
        .len();
    assert!(
        result_len <= 60_000,
        "the result stays inside the N5 bound: {result_len}"
    );

    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
    daemon.shutdown().await;
}

/// S32 (status half) — fifty concurrent `herdr_status` calls, each
/// through its own real relay subprocess on its own occupied pane:
/// every call is answered, every relay binds its own minted identity.
#[tokio::test]
async fn s32_fifty_concurrent_status_calls_answer() {
    const CALLERS: usize = 50;
    let mut topology = Topology::single_shell();
    for _ in 1..CALLERS {
        topology.create_tab("w1");
    }
    for (i, pane) in topology.panes.iter_mut().enumerate() {
        pane.agent = Some(occupant(&format!("agent-{i}"), &format!("sess-{i}")));
    }
    let fake = FakeHerdr::start(topology);
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    // All fifty relays spawn, every request lands, then every reply is
    // read — the calls are in flight together, and every daemon-side
    // snapshot they fan out to hits the fake at once.
    let mut relays: Vec<RelayClient> = (0..CALLERS)
        .map(|i| {
            RelayClient::spawn(
                &daemon.socket_path(),
                &cwd,
                Some(&format!("w1:p{}", i.saturating_add(1))),
            )
        })
        .collect();
    for relay in &mut relays {
        relay.send(&status_call(1)).await;
    }
    let mut replies = Vec::with_capacity(CALLERS);
    for relay in &mut relays {
        replies.push(relay.recv().await);
    }
    for relay in relays {
        let (status, stderr) = relay.close().await;
        assert!(status.success(), "relay exits on EOF: {status} {stderr}");
    }

    assert_eq!(replies.len(), CALLERS);
    for reply in &replies {
        assert_eq!(reply["id"], 1, "each caller's own id echoes");
        let page = status_page(reply);
        assert!(page["health"]["daemon"]["pid"].is_u64());
        assert_eq!(page["runs"], json!([]));
    }
    daemon.shutdown().await;
    assert_eq!(
        bindings(&dirs.store_path()).len(),
        CALLERS,
        "each relay bound its own minted relayInstanceId"
    );
}
