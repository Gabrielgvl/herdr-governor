//! Codec-layer tests — framing, the 1 MiB bound, envelope decode, the
//! error-code map, and fixture round-trips. No sockets; the fake-server
//! leg is P4.H2's.

use serde_json::{Value, json};

use super::super::codec::{
    Frame, HerdrError, LineAccumulator, MAX_FRAME_BYTES, decode_frame, encode_request,
    error_to_typed, typed,
};
use super::super::ops::AgentPrompted;
use super::super::types::{AgentStatus, Observed, SessionKind, SessionSnapshot};
use super::epoch;

const TRACE: &str =
    include_str!("../../../../../tests/fixtures/contract/a2-tools-daemon-trace.jsonl");
const A46: &str = include_str!("../../../../../tests/fixtures/contract/a46-identity-evidence.json");

/// Every `tx`/`rx` wire frame recorded in the tools-daemon trace.
fn trace_frames() -> Vec<(String, String)> {
    let mut frames = Vec::new();
    for line in TRACE.lines() {
        let rec: Value = serde_json::from_str(line).expect("trace record");
        for key in ["tx", "rx"] {
            if let Some(frame) = rec.get(key).and_then(Value::as_str) {
                frames.push((key.to_owned(), frame.trim_end_matches('\n').to_owned()));
            }
        }
    }
    assert!(frames.len() > 20, "fixture supplies frames");
    frames
}

/// a2 trace + protocol-22 envelopes: outbound frames carry
/// `{id, method, params}`; inbound frames decode as exactly one of
/// result / error / event; the tools-daemon's `{"type":…}` framing is not
/// a valid protocol-22 envelope (no `id`) and must be malformed.
#[test]
fn codec_roundtrips_fixture_envelopes() {
    let mut requests = 0;
    let mut handshake = 0;
    let mut probe = 0;
    let mut results = 0;
    let mut errors = 0;
    let mut malformed = 0;
    for (dir, frame) in trace_frames() {
        // The trace deliberately records non-parseable probes: the
        // over-limit frames and their `{\n` truncated heads.
        let raw: Value = if let Ok(raw) = serde_json::from_str(&frame) {
            raw
        } else {
            probe += 1;
            continue;
        };
        if dir == "tx" {
            // The trace records the session handshake too —
            // {"type":"hello"} carries no request id. Request frames are
            // {id, method, params}; the trace also sends deliberately
            // malformed requests (e.g. params:[]) to probe the server's
            // invalid_request path — they count as malformed, not valid.
            if raw.get("method").is_some_and(Value::is_string) {
                assert!(
                    raw.get("id").is_some_and(Value::is_string),
                    "tx id: {frame}"
                );
                if raw.get("params").is_some_and(Value::is_object) {
                    requests += 1;
                } else {
                    malformed += 1;
                }
            } else {
                assert!(
                    raw.get("type").is_some_and(Value::is_string),
                    "handshake: {frame}"
                );
                handshake += 1;
            }
            continue;
        }
        match decode_frame(frame.as_bytes()) {
            Ok(Frame::Result { .. }) => results += 1,
            Ok(Frame::Error { .. }) => errors += 1,
            Ok(Frame::Event { .. }) => panic!("rx event frame outside a subscription: {frame}"),
            Err(HerdrError::Malformed { .. }) => malformed += 1,
            Err(other) => panic!("unexpected error class for {frame}: {other:?}"),
        }
    }
    assert!(
        results > 5 && errors >= 2 && malformed > 5 && requests > 5 && handshake > 0 && probe > 0,
        "{requests}/{handshake}/{probe}/{results}/{errors}/{malformed}"
    );

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

/// The a46 capture's real snapshot decodes — every `required` member the
/// fixture schema pins — and `agent_rows` carries the occupant fields the
/// identity functions consume (no harness names asserted: structure only).
#[test]
fn snapshot_decodes_a46_capture() {
    let fixture: Value = serde_json::from_str(A46).expect("a46 fixture");
    let snapshot: SessionSnapshot =
        serde_json::from_value(fixture["captures"]["initial-shell"]["snapshot"].clone())
            .expect("snapshot decodes");
    assert_eq!(snapshot.protocol, 22);
    assert_eq!(snapshot.panes[0].pane_id, "w1:p1");
    assert_eq!(snapshot.panes[0].terminal_id, "term_65c9180ad9c2f1");
    assert!(snapshot.agents.is_empty());
    assert!(snapshot.agent_rows().is_empty(), "no agents → no rows");

    // `first-native-ready` carries an agent row with no `agent_session`
    // (evidence: the session arrives after readiness) — the row must not
    // invent one.
    let ready: SessionSnapshot =
        serde_json::from_value(fixture["captures"]["first-native-ready"]["snapshot"].clone())
            .expect("ready snapshot decodes");
    let rows = ready.agent_rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.pane_id, "w1:p1");
    assert_eq!(row.terminal_id, "term_65c9180ad9c2f1");
    assert!(row.agent.is_some(), "kind present");
    assert!(row.name.is_some(), "name present on agent surface");
    assert!(
        row.native_session.is_none(),
        "session absent before discovery"
    );
    assert_eq!(row.status, Some(AgentStatus::Idle));

    // `warmup-killed` is the first capture carrying a session — a
    // `kind:"path"` native handle.
    let warmed: SessionSnapshot =
        serde_json::from_value(fixture["captures"]["warmup-killed"]["snapshot"].clone())
            .expect("warmed snapshot decodes");
    assert!(warmed.agent_rows()[0].native_session.is_some());
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
