//! Codec-layer tests — framing, the 1 MiB bound, envelope decode, the
//! error-code map, and fixture round-trips. No sockets; the fake-server
//! leg is P4.H2's.

use serde_json::{Value, json};

use super::super::codec::{
    Frame, HerdrError, LineAccumulator, MAX_FRAME_BYTES, decode_frame, encode_request,
    error_to_typed, typed,
};
use super::super::ops::AgentPrompted;
use super::super::types::{Observed, SessionKind, SessionSnapshot};
use super::epoch;

/// The literal encode/decode round-trip: an encoded request's member set
/// is exactly `{id, method, params}` plus the line terminator, and a result
/// line decodes and re-encodes losslessly. The a2-trace classification
/// moved to `tests/herdr_fixtures.rs` (run-time fixture read) and its
/// public-client decode leg is `contract_fake_herdr_fixture_replay`.
#[test]
fn codec_encode_decode_literals() {
    // Encode → parse → the member set is exactly the request envelope.
    let enc = encode_request("gov:7", "pane.get", &json!({"pane_id": "w1:p1"})).expect("encode");
    let sent: Value = serde_json::from_slice(&enc).expect("encoded json");
    assert_eq!(sent["method"], "pane.get");
    assert_eq!(sent["id"], "gov:7");
    assert_eq!(sent["params"], json!({"pane_id": "w1:p1"}));
    assert_eq!(sent.as_object().map(serde_json::Map::len), Some(3));
    assert_eq!(enc.last(), Some(&b'\n'));

    // And a result line decodes and re-encodes losslessly.
    let line = br#"{"id":"gov:7","result":{"type":"pong","version":"0.9.1","protocol":22}}"#;
    match decode_frame(line).expect("result frame") {
        Frame::Result { id, result } => {
            assert_eq!(id, "gov:7");
            assert_eq!(
                json!({"id": id, "result": result}),
                serde_json::from_slice::<Value>(line).expect("reparse")
            );
        }
        Frame::Error { .. } | Frame::Event { .. } => panic!("expected result frame"),
    }
}

#[test]
fn frame_split_across_writes_parses() {
    let mut acc = LineAccumulator::new();
    let line =
        b"{\"id\":\"gov:1\",\"result\":{\"type\":\"pong\",\"version\":\"0.9.1\",\"protocol\":22}}";
    for (i, byte) in line.iter().enumerate() {
        acc.push(&[*byte]).expect("push");
        assert!(acc.take_line().expect("take").is_none(), "byte {i}");
    }
    acc.push(b"\n").expect("newline");
    let got = acc.take_line().expect("take").expect("complete line");
    assert_eq!(got, line);
    assert!(matches!(decode_frame(&got), Ok(Frame::Result { .. })));
}

#[test]
fn frame_bound_accepts_exactly_1mib() {
    let mut acc = LineAccumulator::new();
    acc.push(&vec![b'x'; MAX_FRAME_BYTES])
        .expect("in-bounds push");
    acc.push(b"\n").expect("newline");
    let line = acc.take_line().expect("take").expect("line");
    assert_eq!(line.len(), MAX_FRAME_BYTES);
}

#[test]
fn frame_bound_rejects_over_1mib() {
    // An unterminated over-bound push fails early (the memory bound).
    let mut acc = LineAccumulator::new();
    assert!(matches!(
        acc.push(&vec![b'x'; MAX_FRAME_BYTES + 1]),
        Err(HerdrError::FrameTooLarge)
    ));
    // And a line that completes over the bound fails at take.
    let mut over = LineAccumulator::new();
    over.push(&vec![b'x'; MAX_FRAME_BYTES]).expect("in-bounds");
    over.push(b"y\n").expect("the newline lands");
    assert!(matches!(over.take_line(), Err(HerdrError::FrameTooLarge)));
}

/// Every error code the evidence records maps somewhere typed: the
/// promoted codes get dedicated variants; the rest pass through `Server`
/// verbatim. `id:""` is `Uncorrelated` and a foreign id is malformed.
#[test]
fn error_code_maps_to_variants() {
    for (code, want) in [
        ("agent_pane_busy", "AgentPaneBusy"),
        ("agent_not_found", "AgentNotFound"),
        ("pane_not_found", "PaneNotFound"),
        ("timeout", "Timeout"),
        ("invalid_request", "Server"),
        ("internal_error", "Server"),
        ("unsupported_event_wait_match", "Server"),
        ("never_heard_of_it", "Server"),
    ] {
        let err = error_to_typed("gov:1", "gov:1", code, "m".to_owned());
        let name = format!("{err:?}")
            .split(['{', '('])
            .next()
            .unwrap_or("?")
            .trim()
            .to_owned();
        assert_eq!(name, want, "code {code}");
    }
    assert!(matches!(
        error_to_typed("gov:1", "", "invalid_request", "m".to_owned()),
        HerdrError::Uncorrelated { .. }
    ));
    assert!(matches!(
        error_to_typed("gov:1", "stray", "pane_not_found", "m".to_owned()),
        HerdrError::Malformed { .. }
    ));
}

/// A failed arm reports under the derived id `<request>:sub:<i>:probe`
/// (recorded in the subscription evidence) — parsed as
/// `SubscriptionFailed`, not a bare code.
#[test]
fn derived_subscription_id_parses_index() {
    assert!(matches!(
        error_to_typed(
            "gov:9",
            "gov:9:sub:0:probe",
            "pane_not_found",
            "m".to_owned()
        ),
        HerdrError::SubscriptionFailed { index: 0, .. }
    ));
    let err = error_to_typed(
        "gov:9",
        "gov:9:sub:12:probe",
        "internal_error",
        "m".to_owned(),
    );
    let HerdrError::SubscriptionFailed { index, code, .. } = err else {
        panic!("expected SubscriptionFailed, got {err:?}")
    };
    assert_eq!(index, 12);
    assert_eq!(code, "internal_error");
    // Missing `:probe`, a non-numeric index, or a prefix-collision id are
    // not the derived shape — a foreign id is malformed, not a server
    // error.
    for bad in ["gov:9:sub:0", "gov:9:sub:x:probe", "gov:99:sub:0:probe"] {
        assert!(matches!(
            error_to_typed("gov:9", bad, "pane_not_found", "m".to_owned()),
            HerdrError::Malformed { .. }
        ));
    }
}

/// The `agent_prompted` ack decodes with the a3-recorded fields: `name`,
/// `pane_id`, and `agent_session.kind` where a session exists. (The a3
/// fixture's distilled ack carries only `agent_session_kind`; the wire
/// record it distills is a full `AgentInfo`.)
#[test]
fn agent_prompted_ack_shape() {
    let reply = json!({
        "type": "agent_prompted",
        "agent": {
            "agent": "harness-x",
            "agent_session": {"agent": "harness-x", "kind": "path", "source": "harness-x", "value": "/t/s.jsonl"},
            "agent_status": "idle",
            "focused": true,
            "name": "a3-x-1",
            "pane_id": "w3:p1",
            "revision": 4,
            "tab_id": "w3:t1",
            "terminal_id": "term_1",
            "workspace_id": "w3"
        }
    });
    let ack = typed::<AgentPrompted>(
        Observed {
            epoch: epoch(),
            value: reply,
        },
        "agent_prompted",
    )
    .expect("agent_prompted decodes")
    .value;
    assert_eq!(ack.agent.name.as_deref(), Some("a3-x-1"));
    assert_eq!(ack.agent.pane_id, "w3:p1");
    assert_eq!(
        ack.agent.agent_session.map(|s| s.kind),
        Some(SessionKind::Path)
    );
}

#[test]
fn malformed_lines_are_typed() {
    let lines: &[&[u8]] = &[
        b"{",
        b"[1,2]",
        b"\"x\"",
        b"{}",
        b"{\"id\":\"a\"}",
        b"{\"id\":5,\"result\":{}}",
        b"{\"result\":{}}",
    ];
    for &line in lines {
        assert!(
            matches!(decode_frame(line), Err(HerdrError::Malformed { .. })),
            "{line:?}"
        );
    }
}

#[test]
fn agent_row_maps_unknown_status_to_none() {
    let snapshot: SessionSnapshot = serde_json::from_value(json!({
        "version": "0.9.1", "protocol": 22,
        "workspaces": [], "tabs": [], "layouts": [],
        "panes": [],
        "agents": [{
            "pane_id": "w1:p9", "tab_id": "w1:t1", "workspace_id": "w1",
            "terminal_id": "term_9", "revision": 0, "focused": false,
            "agent_status": "unknown"
        }]
    }))
    .expect("snapshot");
    assert_eq!(snapshot.agent_rows()[0].status, None);
}
