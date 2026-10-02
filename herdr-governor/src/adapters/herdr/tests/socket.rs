//! Socket-level unit tests — one real `UnixListener` driving the client's
//! own half of the protocol (the full fake-server contract suite is
//! P4.H2's). `timeout_elapsed_is_typed` runs on the paused Tokio clock.

use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncReadExt as _;

use super::super::codec::HerdrError;
use super::super::types::{AgentStatus, SubEvent, SubscriptionSpec};
use super::{accept_request, socket, write_line};

/// The client-side deadline elapses while the server sits silent → the
/// typed `DeadlineExceeded`, and the half-open connection is dropped.
/// Paused clock: no wall-clock sleep.
#[tokio::test(start_paused = true)]
async fn timeout_elapsed_is_typed() {
    let (_dir, _listener, client) = socket();
    let err = client
        .ping(Duration::from_millis(50))
        .await
        .expect_err("no listener accept → the deadline wins");
    assert!(matches!(err, HerdrError::DeadlineExceeded), "{err:?}");
}

/// Silent EOF on an armed subscription → `StreamClosed`, then the channel
/// ends — the recorded teardown shape (zero bytes, no error frame).
#[tokio::test]
async fn subscribe_eof_is_stream_closed() {
    let (_dir, listener, client) = socket();
    let server = tokio::spawn(async move {
        let (mut stream, id) = accept_request(&listener).await;
        write_line(
            &mut stream,
            json!({"id": id, "result": {"type": "subscription_started"}}),
        )
        .await;
        drop(stream);
    });
    let mut sub = client
        .subscribe(
            vec![SubscriptionSpec::ScrollChanged {
                pane_id: "w1:p1".to_owned(),
            }],
            Duration::from_secs(5),
        )
        .await
        .expect("armed");
    match sub.next().await {
        Some(Err(HerdrError::StreamClosed)) => {}
        other => panic!("expected StreamClosed, got {other:?}"),
    }
    assert!(
        sub.next().await.is_none(),
        "channel ends after the terminal error"
    );
    server.await.expect("server task");
}

/// The armed stream decodes the three confirmed event kinds, then EOF —
/// the `a2` evidence's event envelope `{event, data}` carried faithfully.
#[tokio::test]
async fn subscribe_streams_typed_events() {
    let (_dir, listener, client) = socket();
    let server = tokio::spawn(async move {
        let (mut stream, id) = accept_request(&listener).await;
        write_line(
            &mut stream,
            json!({"id": id, "result": {"type": "subscription_started"}}),
        )
        .await;
        write_line(
            &mut stream,
            json!({
                "event": "pane.scroll_changed",
                "data": {"pane_id": "w1:p1", "workspace_id": "w1",
                         "scroll": {"max_offset_from_bottom": 267,
                                    "offset_from_bottom": 120,
                                    "viewport_rows": 40}}
            }),
        )
        .await;
        write_line(
            &mut stream,
            json!({
                "event": "pane.agent_status_changed",
                "data": {"pane_id": "w1:p1", "workspace_id": "w1",
                         "agent_status": "idle", "agent": "harness-x",
                         "state_labels": {}}
            }),
        )
        .await;
        write_line(
            &mut stream,
            json!({
                "event": "pane.output_matched",
                "data": {"pane_id": "w1:p1", "matched_line": "GOV-MARKER",
                         "read": {"pane_id": "w1:p1", "tab_id": "w1:t1",
                                  "workspace_id": "w1", "revision": 7,
                                  "source": "recent", "format": "text",
                                  "text": "GOV-MARKER\n", "truncated": false}}
            }),
        )
        .await;
        drop(stream);
    });
    let mut sub = client
        .subscribe(vec![], Duration::from_secs(5))
        .await
        .expect("armed (an empty list is accepted, per evidence)");
    match sub.next().await {
        Some(Ok(SubEvent::ScrollChanged {
            pane_id, scroll, ..
        })) => {
            assert_eq!(pane_id, "w1:p1");
            assert_eq!(scroll.offset_from_bottom, 120);
        }
        other => panic!("scroll_changed, got {other:?}"),
    }
    match sub.next().await {
        Some(Ok(SubEvent::AgentStatusChanged { agent_status, .. })) => {
            assert_eq!(agent_status, AgentStatus::Idle);
        }
        other => panic!("agent_status_changed, got {other:?}"),
    }
    match sub.next().await {
        Some(Ok(SubEvent::OutputMatched {
            matched_line, read, ..
        })) => {
            assert_eq!(matched_line, "GOV-MARKER");
            assert_eq!(read.revision, 7);
        }
        other => panic!("output_matched, got {other:?}"),
    }
    assert!(matches!(
        sub.next().await,
        Some(Err(HerdrError::StreamClosed))
    ));
    server.await.expect("server task");
}

/// A failed arm: the server reports it under the derived id
/// `<request>:sub:<i>:probe` and closes — typed `SubscriptionFailed`.
#[tokio::test]
async fn subscribe_failure_derived_id() {
    let (_dir, listener, client) = socket();
    let server = tokio::spawn(async move {
        let (mut stream, id) = accept_request(&listener).await;
        write_line(
            &mut stream,
            json!({
                "id": format!("{id}:sub:0:probe"),
                "error": {"code": "pane_not_found", "message": "pane w9:p9 not found"}
            }),
        )
        .await;
        drop(stream);
    });
    let err = client
        .subscribe(
            vec![SubscriptionSpec::ScrollChanged {
                pane_id: "w9:p9".to_owned(),
            }],
            Duration::from_secs(5),
        )
        .await
        .expect_err("arm fails");
    let HerdrError::SubscriptionFailed { index, code, .. } = err else {
        panic!("expected SubscriptionFailed, got {err:?}")
    };
    assert_eq!(index, 0);
    assert_eq!(code, "pane_not_found");
    server.await.expect("server task");
}

/// One unary request per connection: the client takes the reply and the
/// server sees EOF — nothing further is ever sent on the conn (the A2
/// connection discipline).
#[tokio::test]
async fn unary_closes_after_one_reply() {
    let (_dir, listener, client) = socket();
    let server = tokio::spawn(async move {
        let (mut stream, id) = accept_request(&listener).await;
        write_line(
            &mut stream,
            json!({"id": id, "result": {"type": "pong", "version": "0.9.1", "protocol": 22}}),
        )
        .await;
        let mut more = [0u8; 8];
        let n = stream.get_mut().read(&mut more).await.expect("read eof");
        assert_eq!(n, 0, "client closed the unary connection");
    });
    let pong = client.ping(Duration::from_secs(5)).await.expect("pong");
    assert_eq!(pong.value.protocol, 22);
    server.await.expect("server task");
}

/// Each connect mints a fresh epoch: two pings see `seq` 0 then 1, and the
/// reply's epoch is the connection's.
#[tokio::test]
async fn conn_epoch_increments_per_connect() {
    let (_dir, listener, client) = socket();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, id) = accept_request(&listener).await;
            write_line(
                &mut stream,
                json!({"id": id, "result": {"type": "pong", "version": "0.9.1", "protocol": 22}}),
            )
            .await;
        }
    });
    let first = client.ping(Duration::from_secs(5)).await.expect("pong1");
    let second = client.ping(Duration::from_secs(5)).await.expect("pong2");
    assert_eq!(first.epoch.seq, 0);
    assert_eq!(second.epoch.seq, 1);
    assert!(first.epoch.socket_inode > 0, "inode observed");
    server.await.expect("server task");
}
