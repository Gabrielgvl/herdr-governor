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
use tempfile::tempdir;

use crate::support::e2e::{
    self, Relay, call_request, close_relay, fixture, notification, occupant, occupied_topology,
    relay_conversation, request, spawn_relay, start_daemon, status_call, status_page, stop_daemon,
    tool_code,
};
use crate::support::fake_herdr::{FakeHerdr, Topology};

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
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let (relay, replies) = relay_conversation(
        &sock,
        &cwd,
        Some("w1:p1"),
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
    assert_eq!(names, ["herdr_status"], "PR A lists one tool");

    let page = status_page(&replies[2]);
    assert_eq!(replies[2]["id"], 4);
    assert!(page["health"]["daemon"]["pid"].is_u64());
    assert!(
        page["health"]["herdr"]["freshSecsAgo"].is_u64(),
        "the request-time snapshot lands in health: {page}"
    );

    assert_eq!(replies[3]["id"], 5);
    assert_eq!(replies[3]["result"], json!({}), "ping answers empty");

    let (status, stderr) = close_relay(relay).await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
    stop_daemon(stop, daemon).await;
}

/// S31 — the strict-schema surface through the real relay: every
/// refusal shape the daemon produces crosses the v1 frame intact —
/// `REQUEST_INVALID`/`TOOL_UNKNOWN` as `isError` tool results, JSON-RPC
/// faults as error envelopes — and a served page stays inside the
/// 60,000-byte result bound.
#[tokio::test]
async fn s31_strict_schemas_refuse_through_the_relay() {
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let (relay, replies) = relay_conversation(
        &sock,
        &cwd,
        Some("w1:p1"),
        &[
            call_request(10, "herdr_status", &json!({"surprise": 1})),
            call_request(11, "herdr_status", &json!({"eventId": 7})),
            "{\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"tools/call\"}\n".to_owned(),
            call_request(13, "bogus", &json!({})),
            call_request(14, "herdr_launch", &json!({})),
            request(15, "bogus/unknown", &json!({})),
            "{\"id\":16,\"method\":\"ping\"}\n".to_owned(),
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
        "TOOL_UNKNOWN",
        "declared but unserved in PR A"
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

    let (status, stderr) = close_relay(relay).await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
    stop_daemon(stop, daemon).await;
}

/// S32 (status half) — fifty concurrent `herdr_status` calls, each
/// through its own real relay subprocess on its own occupied pane:
/// every call is answered, every relay binds its own minted identity.
#[tokio::test]
async fn s32_fifty_concurrent_status_calls_answer() {
    const CALLERS: usize = 50;
    let tmp = tempdir().expect("tmp");
    let mut topology = Topology::single_shell();
    for _ in 1..CALLERS {
        topology.create_tab("w1");
    }
    for (i, pane) in topology.panes.iter_mut().enumerate() {
        pane.agent = Some(occupant(&format!("agent-{i}"), &format!("sess-{i}")));
    }
    let fake = FakeHerdr::start(topology);
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    // All fifty relays spawn, every request lands, then every reply is
    // read — the calls are in flight together, and every daemon-side
    // snapshot they fan out to hits the fake at once.
    let replies = tokio::task::spawn_blocking(move || {
        let mut relays: Vec<Relay> = (0..CALLERS)
            .map(|i| spawn_relay(&sock, &cwd, Some(&format!("w1:p{}", i.saturating_add(1)))))
            .collect();
        for relay in &mut relays {
            relay.send(&status_call(1));
        }
        let replies: Vec<Value> = relays.iter_mut().map(Relay::recv).collect();
        for relay in relays {
            let (status, stderr) = relay.close_and_wait();
            assert!(status.success(), "relay exits on EOF: {status} {stderr}");
        }
        replies
    })
    .await
    .expect("the blocking leg joins");

    assert_eq!(replies.len(), CALLERS);
    for reply in &replies {
        assert_eq!(reply["id"], 1, "each caller's own id echoes");
        let page = status_page(reply);
        assert!(page["health"]["daemon"]["pid"].is_u64());
        assert_eq!(page["runs"], json!([]));
    }
    stop_daemon(stop, daemon).await;
    assert_eq!(
        e2e::bindings(&state).len(),
        CALLERS,
        "each relay bound its own minted relayInstanceId"
    );
}
