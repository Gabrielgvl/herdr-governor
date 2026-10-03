//! The e2e child machinery (P5.A4): real `herdr-governor` children —
//! `daemon` and `relay` — plus the fixture/poll helpers the e2e suites
//! drive on top of `FakeHerdr`. The daemon leg is `startup`'s child
//! shape generalized over a scripted `herdr_socket`; the relay leg is
//! the harness's own spawn shape — R1's `relay_e2e` proved it against a
//! fake daemon socket, and these helpers aim the same child at the
//! real `daemon::run` boundary.
//!
//! Blocking child I/O never runs on the test's runtime thread: the
//! async helpers move the work into `spawn_blocking` so `FakeHerdr`
//! and any in-process daemon keep driving on `current_thread`.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use herdr_governor::adapters::herdr::SessionKind;
use herdr_governor::daemon::{self, DaemonError, Settings};
use herdr_governor::store::Store;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::fake_herdr::Topology;
use super::fake_herdr::topology::{Occupant, SessionRef};

/// The shipped binary under test.
pub const BIN: &str = env!("CARGO_BIN_EXE_herdr-governor");

/// Every wait is a bounded poll against this deadline — never a sleep.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// The `[daemon]` catalog text — `cooldown_secs` is the knob F27's
/// reload test varies so the adopted digest moves.
#[must_use]
pub fn catalog(herdr_socket: &Path, cooldown_secs: u64) -> String {
    format!(
        "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = {cooldown_secs}\n\n\
         [catalog]\noperating_points = []\n\n\
         [daemon]\nherdr_socket = \"{}\"\n\
         jev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n",
        herdr_socket.display()
    )
}

/// A valid fixture: `[daemon]` + a `0600` credential, `herdr_socket`
/// naming the Herdr session the daemon's client dials — the FakeHerdr
/// socket for the status leg. Same shape as `startup`'s.
#[must_use]
pub fn fixture(root: &Path, herdr_socket: &Path) -> (PathBuf, PathBuf) {
    let state = root.join("state");
    let config = root.join("config");
    fs::create_dir_all(&state).expect("state dir");
    fs::create_dir_all(&config).expect("config dir");
    fs::write(config.join("catalog.toml"), catalog(herdr_socket, 60)).expect("catalog");
    let credentials = config.join("credentials");
    fs::write(&credentials, "test-token\n").expect("credentials");
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).expect("chmod");
    (state, config)
}

/// The path canonicalized — the string the daemon's `canonicalize`
/// read produces and compares to `projectRoot`.
#[must_use]
pub fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonicalize test path")
        .to_str()
        .expect("utf8 path")
        .to_owned()
}

/// `git init` in `dir` — the relay's git-toplevel derivation needs a
/// real worktree (`GIT_OPTIONAL_LOCKS=0`, the relay's own hygiene;
/// `GIT_DIR`/`GIT_WORK_TREE` scrubbed so a stray harness env can't
/// redirect discovery).
pub fn git_init(dir: &Path) {
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .expect("run git init");
    assert!(status.success(), "git init in {}", dir.display());
}

/// An occupant with a native session (`kind:"id"`, like a harness mint).
#[must_use]
pub fn occupant(name: &str, session: &str) -> Occupant {
    Occupant {
        name: name.to_owned(),
        kind: "harness-x".to_owned(),
        status: "idle".to_owned(),
        session: Some(SessionRef {
            kind: SessionKind::Id,
            value: session.to_owned(),
        }),
    }
}

/// A `single_shell` topology whose `w1:p1` holds an occupied pane —
/// the caller the relay's envelope resolves to.
#[must_use]
pub fn occupied_topology() -> Topology {
    let mut topology = Topology::single_shell();
    topology.panes[0].agent = Some(occupant("gov-caller", "sess-1"));
    topology
}

// — The daemon children ————————————————————————————————————————————

/// `herdr-governor daemon --state-dir --config-dir`, stderr piped for
/// postmortems (drain it via `stderr_of` so a chatty child can't fill
/// the pipe).
#[must_use]
pub fn spawn_daemon(state: &Path, config: &Path) -> Child {
    Command::new(BIN)
        .args([
            "daemon",
            "--state-dir",
            state.to_str().expect("state utf8"),
            "--config-dir",
            config.to_str().expect("config utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon spawns")
}

/// The child's buffered stderr, read on a thread so a chatty child
/// never deadlocks the pipe.
#[must_use]
pub fn stderr_of(child: &mut Child) -> std::thread::JoinHandle<String> {
    let mut pipe = child.stderr.take().expect("stderr piped");
    std::thread::spawn(move || {
        let mut text = String::new();
        let _read = pipe.read_to_string(&mut text);
        text
    })
}

/// `kill <sig> <pid>` — the operator's signal, exactly as delivered.
pub fn signal(child: &Child, sig: &str) {
    let status = Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success(), "kill {sig} {}", child.id());
}

/// The §4.3 lock-probe dialect: connect, bare `ping`, read one line.
#[must_use]
pub fn probe(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _timeout = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(br#"{"jsonrpc":"2.0","id":"gov:probe","method":"ping"}"#)
        .and_then(|()| stream.write_all(b"\n"))
        .is_err()
    {
        return false;
    }
    let mut reply = String::new();
    let _read = BufReader::new(stream).read_line(&mut reply);
    reply.contains("\"result\"")
}

/// Poll `until` until it holds or the deadline passes — then panic.
/// Async so the test's runtime keeps driving the fake and any
/// in-process daemon while the child makes progress.
pub async fn await_for(what: &str, mut until: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        if until() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Spawn `daemon::run` in-process and poll for the bound socket.
pub async fn start_daemon(
    state: &Path,
    config: &Path,
) -> (
    PathBuf,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<ExitCode, DaemonError>>,
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

/// Stop an in-process daemon through its oneshot and await teardown.
pub async fn stop_daemon(
    stop: oneshot::Sender<()>,
    daemon: tokio::task::JoinHandle<Result<ExitCode, DaemonError>>,
) {
    stop.send(()).expect("stop");
    let code = daemon.await.expect("join").expect("run exits ok");
    assert_eq!(code, ExitCode::SUCCESS, "clean stop after serving");
}

// — The relay child ————————————————————————————————————————————————

/// A spawned `herdr-governor relay` with piped stdin/stdout — the
/// harness's own spawn shape.
#[derive(Debug)]
pub struct Relay {
    child: Child,
    stdout: BufReader<ChildStdout>,
}

impl Relay {
    /// Write one stdin line verbatim (it already carries its `\n`).
    pub fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("relay stdin");
        stdin
            .write_all(line.as_bytes())
            .expect("write to relay stdin");
        stdin.flush().expect("flush relay stdin");
    }

    /// Read one reply line off the relay's stdout.
    #[must_use]
    pub fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read relay stdout");
        assert!(n > 0, "relay closed stdout before a reply");
        serde_json::from_str(&line).expect("relay reply is json")
    }

    /// Close stdin (the EOF the harness sends at session end) and wait.
    #[must_use]
    pub fn close_and_wait(mut self) -> (ExitStatus, String) {
        drop(self.child.stdin.take());
        let status = self.child.wait().expect("wait on relay");
        let mut stderr = String::new();
        if let Some(mut err) = self.child.stderr.take() {
            let _ignored = err.read_to_string(&mut stderr);
        }
        (status, stderr)
    }
}

/// Spawn the relay: `--socket` to the daemon, `current_dir` the
/// session cwd (the `projectRoot` derivation reads it), `HERDR_PANE_ID`
/// and the git env scrubbed then set — the case's own env, never the
/// session's.
#[must_use]
pub fn spawn_relay(sock: &Path, cwd: &Path, pane: Option<&str>) -> Relay {
    let mut command = Command::new(BIN);
    command
        .arg("relay")
        .arg("--socket")
        .arg(sock)
        .current_dir(cwd)
        .env_remove("HERDR_PANE_ID")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(pane_id) = pane {
        command.env("HERDR_PANE_ID", pane_id);
    }
    let mut child = command.spawn().expect("spawn herdr-governor relay");
    let stdout = BufReader::new(child.stdout.take().expect("relay stdout"));
    Relay { child, stdout }
}

/// One line through an existing relay on a blocking thread: the relay
/// moves in and back out so the sync stdio never parks the runtime.
pub async fn relay_exchange(mut relay: Relay, line: &str) -> (Relay, Value) {
    let text = line.to_owned();
    tokio::task::spawn_blocking(move || {
        relay.send(&text);
        let reply = relay.recv();
        (relay, reply)
    })
    .await
    .expect("relay exchange joins")
}

/// One line through the relay `slot` is holding: take it out, run the
/// exchange on a blocking thread, put the relay back — the `Option`
/// slot keeps the move-in/move-out honest without shadowed names.
pub async fn exchange_on(slot: &mut Option<Relay>, line: &str) -> Value {
    let relay = slot.take().expect("a live relay in the slot");
    let (back, reply) = relay_exchange(relay, line).await;
    *slot = Some(back);
    reply
}

/// Spawn a relay and run `lines` through it on one blocking thread,
/// reading `replies` replies (dropped notifications produce none).
pub async fn relay_conversation(
    sock: &Path,
    cwd: &Path,
    pane: Option<&str>,
    lines: &[String],
    replies: usize,
) -> (Relay, Vec<Value>) {
    let sock_owned = sock.to_path_buf();
    let cwd_owned = cwd.to_path_buf();
    let pane_owned = pane.map(str::to_owned);
    let script = lines.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut relay = spawn_relay(&sock_owned, &cwd_owned, pane_owned.as_deref());
        for line in &script {
            relay.send(line);
        }
        let out = (0..replies).map(|_| relay.recv()).collect();
        (relay, out)
    })
    .await
    .expect("relay conversation joins")
}

/// Stdin EOF (the session end) and the exit wait, on a blocking thread.
pub async fn close_relay(relay: Relay) -> (ExitStatus, String) {
    tokio::task::spawn_blocking(move || relay.close_and_wait())
        .await
        .expect("relay wait joins")
}

// — Request lines and reply reads ——————————————————————————————————

/// A JSON-RPC request line the harness would write.
#[must_use]
pub fn request(id: u64, method: &str, params: &Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
        + "\n"
}

/// A `tools/call` request line.
#[must_use]
pub fn call_request(id: u64, name: &str, arguments: &Value) -> String {
    request(
        id,
        "tools/call",
        &json!({"name": name, "arguments": arguments}),
    )
}

/// A `herdr_status` call — the one tool PR A serves.
#[must_use]
pub fn status_call(id: u64) -> String {
    call_request(id, "herdr_status", &json!({}))
}

/// A notification line: no `id`, never forwarded.
#[must_use]
pub fn notification(method: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": method,
    })
    .to_string()
        + "\n"
}

/// The tool result's `content[0].text` parsed — a status page or the
/// `{"code","message"}` refusal body.
#[must_use]
pub fn tool_body(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    serde_json::from_str(text).expect("tool body is json")
}

/// The typed refusal code an `isError` tool result carries.
#[must_use]
pub fn tool_code(reply: &Value) -> String {
    assert_eq!(
        reply["result"]["isError"], true,
        "expected a refused call: {reply}"
    );
    tool_body(reply)["code"]
        .as_str()
        .expect("refusal code")
        .to_owned()
}

/// The status page a `herdr_status` reply carries — asserts the call
/// was served (`isError` false).
#[must_use]
pub fn status_page(reply: &Value) -> Value {
    assert_eq!(
        reply["result"]["isError"], false,
        "the status call served: {reply}"
    );
    tool_body(reply)
}

// — Store and check-config reads ———————————————————————————————————

/// The `relay_bindings` rows joined to their caller:
/// `(relay_instance_id, pane_at_bind, bound_at, native_session)`. Read
/// after the daemon is down — `Store::open` is never concurrent with a
/// running daemon in these tests.
#[must_use]
pub fn bindings(state: &Path) -> Vec<(String, String, String, String)> {
    let store = Store::open(&state.join("governor.db")).expect("store opens");
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT b.relay_instance_id, b.pane_id_at_bind, b.bound_at, c.native_session
             FROM relay_bindings b JOIN callers c ON c.caller_id = b.caller_id",
        )
        .expect("bindings query");
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })
    .expect("bindings rows")
    .collect::<Result<Vec<_>, _>>()
    .expect("bindings collect")
}

/// `herdr-governor check-config --config-dir <dir>` → the catalog's
/// content digest off the `catalog ok: config=<d>` line.
#[must_use]
pub fn check_config_version(config: &Path) -> String {
    let out = Command::new(BIN)
        .arg("check-config")
        .arg("--config-dir")
        .arg(config)
        .output()
        .expect("check-config spawns");
    assert!(
        out.status.success(),
        "check-config: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout utf8");
    stdout
        .trim()
        .strip_prefix("catalog ok: config=")
        .and_then(|rest| rest.split_whitespace().next())
        .expect("config= digest")
        .to_owned()
}
