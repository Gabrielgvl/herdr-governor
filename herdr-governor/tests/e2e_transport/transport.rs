//! `transport` — the M2 MCP-transport e2e (p5-plan §4.11, the P5.M2 card):
//! the daemon's real `0600` socket driven by `daemon::run` in-process,
//! speaking the v1 relay frame
//! (`{"v":1,"caller":…,"rpc":<request>}` → `{"v":1,"rpc":<response>}`),
//! `herdr_status` answered through the A2 `Msg::Tool` arm. The in-file
//! client/fixture duplicates `startup.rs`'s shape on purpose —
//! T1 promotes a shared `mcp_client`/`TestDaemon` later (the A2 precedent).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::{self, DaemonError, Settings};
use herdr_governor::mcp::framing::{decode_reply, encode_request};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::support::fake_herdr::topology::{Occupant, SessionRef};
use crate::support::fake_herdr::{FakeHerdr, Topology};

/// A valid fixture: `[daemon]` + a `0600` credential, `herdr_socket`
/// naming the Herdr session the daemon's client dials — a nowhere path
/// for the plumbing tests (the tick's snapshot fails and is logged,
/// which is what A1 does with Herdr liveness), the `FakeHerdr` socket
/// for the status leg. Same shape as `startup.rs`'s.
fn fixture(root: &Path, herdr_socket: &Path) -> (PathBuf, PathBuf) {
    let state = root.join("state");
    let config = root.join("config");
    fs::create_dir_all(&state).expect("state dir");
    fs::create_dir_all(&config).expect("config dir");
    fs::write(
        config.join("catalog.toml"),
        format!(
            "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n\
             [catalog]\noperating_points = []\n\n\
             [daemon]\nherdr_socket = \"{}\"\n\
             jev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n",
            herdr_socket.display()
        ),
    )
    .expect("catalog");
    let credentials = config.join("credentials");
    fs::write(&credentials, "test-token\n").expect("credentials");
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).expect("chmod");
    (state, config)
}

fn caller() -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId("w6:pKQ".into()),
        project_root: ProjectRoot("/repo".into()),
        relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
    }
}

/// Spawn the daemon in-process and poll for the bound socket — bounded,
/// never a fixed sleep.
async fn start_daemon(
    state: &Path,
    config: &Path,
) -> (
    PathBuf,
    oneshot::Sender<()>,
    JoinHandle<Result<ExitCode, DaemonError>>,
) {
    let settings = Settings {
        state_dir: state.to_path_buf(),
        config_dir: config.to_path_buf(),
        herdr_socket: None,
        reconcile_secs: None,
    };
    let (stop, stop_rx) = oneshot::channel::<()>();
    let daemon = tokio::spawn(daemon::run(settings, None, Some(stop_rx)));
    let sock = state.join("governor.sock");
    for _ in 0..400 {
        if sock.exists() {
            return (sock, stop, daemon);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the listener never bound");
}

/// One request's whole leg on a fresh connection: v1 frame out, the
/// `{"v":1,"rpc":…}` line back, EOF behind it.
async fn round_trip(sock: &Path, caller: &CallerEnvelope, rpc: &Value) -> Value {
    let stream = UnixStream::connect(sock).await.expect("connect");
    let mut reader = tokio::io::BufReader::new(stream);
    reader
        .get_mut()
        .write_all(&encode_request(caller, rpc))
        .await
        .expect("frame write");
    let mut line = Vec::new();
    let read = reader
        .read_until(b'\n', &mut line)
        .await
        .expect("reply read");
    assert!(read > 0, "the daemon closed without a reply");
    let rpc_reply = decode_reply(&line).expect("a v1 reply frame");
    // One reply, then the server closes — no second line ever arrives.
    let mut extra = Vec::new();
    let trailing = reader
        .read_until(b'\n', &mut extra)
        .await
        .expect("trailing read");
    assert_eq!(trailing, 0, "the connection closes after its one reply");
    rpc_reply
}

/// PR A's surface (OQ-S): `tools/list` over the real socket serves exactly
/// `herdr_status` — `herdr_launch`/`herdr_run` stay unlisted until B2/C4.
#[tokio::test]
async fn transport_tools_list_exposes_status_only_in_pr_a() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (state, config) = fixture(tmp.path(), Path::new("/nonexistent/herdr.sock"));
    let (sock, stop, daemon) = start_daemon(&state, &config).await;

    let reply = round_trip(
        &sock,
        &caller(),
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(reply["id"], 1, "the request id echoes verbatim");
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name is a string"))
        .collect();
    assert_eq!(names, ["herdr_status"], "PR A lists exactly one tool");

    stop.send(()).expect("stop");
    let code = daemon.await.expect("join").expect("run exits ok");
    assert_eq!(code, ExitCode::SUCCESS, "clean stop after serving");
    assert!(!sock.exists(), "teardown removed the socket");
}

/// One task per connection (§4.11): concurrent connections carry
/// independent request/reply pairs — distinct ids echo back on their own
/// connection with no cross-talk.
#[tokio::test]
async fn transport_concurrent_connections_are_independent() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (state, config) = fixture(tmp.path(), Path::new("/nonexistent/herdr.sock"));
    let (sock, stop, daemon) = start_daemon(&state, &config).await;

    let mut legs = Vec::new();
    for id in 0..8u64 {
        let leg_sock = sock.clone();
        let caller = caller();
        legs.push(tokio::spawn(async move {
            let request = json!({"jsonrpc": "2.0", "id": id, "method": "ping"});
            (id, round_trip(&leg_sock, &caller, &request).await)
        }));
    }
    for leg in legs {
        let (id, reply) = leg.await.expect("leg joins");
        assert_eq!(reply["id"], id, "connection {id} got its own reply");
        assert_eq!(reply["result"], json!({}), "ping answers an empty result");
    }

    stop.send(()).expect("stop");
    let code = daemon.await.expect("join").expect("run exits ok");
    assert_eq!(code, ExitCode::SUCCESS, "clean stop after serving");
}

/// The A2×M2 seam pinned end to end: a v1 `tools/call` frame for
/// `herdr_status` on the real socket runs `mcp::serve`'s conn task —
/// request-time `session.snapshot` + `canonicalize` — into the
/// coordinator's `Msg::Tool` arm, which resolves and binds the caller
/// (F1) and answers the §4.12 status page.
#[tokio::test]
async fn transport_herdr_status_serves_over_the_real_socket() {
    let tmp = tempfile::tempdir().expect("tmp");
    // The caller occupies `w1:p1` with a native session — the snapshot's
    // one agent row F1 resolves the envelope's `paneId` to.
    let mut topology = Topology::single_shell();
    topology.panes[0].agent = Some(Occupant {
        name: "gov-caller".into(),
        kind: "harness-x".into(),
        status: "idle".into(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: "caller-session".into(),
        }),
    });
    let fake = FakeHerdr::start(topology);
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;

    // `projectRoot` must equal its own `canonicalize` — the tempdir's
    // canonical path makes the connection task's realpath read agree.
    let root = tmp
        .path()
        .canonicalize()
        .expect("canonical tmp")
        .to_str()
        .expect("utf8")
        .to_owned();
    let caller = CallerEnvelope {
        pane_id: PaneId("w1:p1".into()),
        project_root: ProjectRoot(root),
        relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
    };
    let reply = round_trip(
        &sock,
        &caller,
        &json!({
            "jsonrpc": "2.0", "id": "s1", "method": "tools/call",
            "params": {"name": "herdr_status", "arguments": {}},
        }),
    )
    .await;

    assert_eq!(reply["id"], "s1", "the request id echoes verbatim");
    assert_eq!(reply["result"]["isError"], false, "the status tool serves");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    let page: Value = serde_json::from_str(text).expect("a status page body");
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

    stop.send(()).expect("stop");
    let code = daemon.await.expect("join").expect("run exits ok");
    assert_eq!(code, ExitCode::SUCCESS, "clean stop after serving");
    assert!(!sock.exists(), "teardown removed the socket");
}
