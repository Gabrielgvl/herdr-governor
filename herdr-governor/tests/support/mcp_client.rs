//! `mcp_client` — the caller-side end of the harness transport (P5.T1):
//! `McpClient` frames a JSON-RPC request inside a chosen caller envelope
//! and opens a fresh daemon-socket connection per call (the v1 frame,
//! §4.11 one-request-per-connection); `RelayClient` spawns the real
//! `herdr-governor relay` subprocess so a request rides the identity
//! path — `HERDR_PANE_ID`→`paneId`, canonical `cwd`→`projectRoot`, the
//! relay-minted `relayInstanceId`. The rest of the module is the shared
//! request vocabulary (`request`, `status_call`, `notification`), the
//! reply readers (`tool_body`, `tool_code`, `status_page`) and the
//! project-root helpers (`git_init`, `canonical`).

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use herdr_governor::mcp::framing::{decode_reply, encode_request};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

/// The shipped binary — `RelayClient` runs the same `relay` argv the
/// owner-mode Herdr runtime runs.
const BIN: &str = env!("CARGO_BIN_EXE_herdr-governor");

/// A caller envelope from its wire spellings — tests pick the pane,
/// project root and relay instance deliberately (F1's relay-derivation
/// is what the subprocess client is for). `pane`/`project_root` must
/// resolve through `identity::resolve` (a canonical, git-anchored root
/// and an existing pane) and `relay_instance` must satisfy the
/// envelope validator's 32-lowercase-hex shape.
#[must_use]
pub fn caller_envelope(pane: &str, project_root: &str, relay_instance: &str) -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId(pane.to_owned()),
        project_root: ProjectRoot(project_root.to_owned()),
        relay_instance_id: RelayInstanceId(relay_instance.to_owned()),
    }
}

/// A JSON-RPC request — the request vocabulary a test drives a client
/// with (`send`/`call` serialize `Value`s directly). `id` may be any
/// wire id shape — including missing for a notification.
#[must_use]
pub fn request(id: impl Into<Value>, method: &str, params: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "method": method,
        "params": params,
    })
}

/// A `tools/call` request.
#[must_use]
pub fn call_request(id: impl Into<Value>, name: &str, arguments: &Value) -> Value {
    request(
        id,
        "tools/call",
        &json!({"name": name, "arguments": arguments}),
    )
}

/// A `herdr_status` call — the paged status tool the suites read back.
#[must_use]
pub fn status_call(id: impl Into<Value>) -> Value {
    call_request(id, "herdr_status", &json!({}))
}

/// A notification: no `id`, never forwarded to the daemon, never
/// answered.
#[must_use]
pub fn notification(method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": method,
    })
}

/// The tool result's `content[0].text` parsed — a status page or the
/// `{"code","message"}` refusal body.
#[must_use]
pub fn tool_body(reply: &Value) -> Value {
    let Some(text) = reply["result"]["content"][0]["text"].as_str() else {
        panic!("the tool result carries content[0].text: {reply}");
    };
    serde_json::from_str(text).expect("the tool body is json")
}

/// The typed refusal code an `isError` tool result carries.
#[must_use]
pub fn tool_code(reply: &Value) -> String {
    assert_eq!(
        reply["result"]["isError"], true,
        "the call was refused: {reply}"
    );
    tool_body(reply)["code"]
        .as_str()
        .expect("the refusal body carries a code")
        .to_owned()
}

/// The status page a `herdr_status` reply carries — asserts the call
/// was served (`isError` false).
#[must_use]
pub fn status_page(reply: &Value) -> Value {
    assert_eq!(
        reply["result"]["isError"], false,
        "the status call was served: {reply}"
    );
    tool_body(reply)
}

/// The path canonicalized — the `projectRoot` spelling the daemon's
/// `canonicalize` read produces and compares against.
#[must_use]
pub fn canonical(path: &Path) -> String {
    path.canonicalize()
        .expect("canonical path")
        .to_str()
        .expect("utf8 path")
        .to_owned()
}

/// `git init` in `dir` — the relay's git-toplevel `projectRoot`
/// derivation needs a real worktree (`GIT_OPTIONAL_LOCKS=0`, the
/// relay's own hygiene; `GIT_DIR`/`GIT_WORK_TREE` scrubbed so a stray
/// session env can't redirect the discovery).
pub fn git_init(dir: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git init runs")
        .status;
    assert!(status.success(), "git init {dir:?}");
}

/// A direct v1-frame client: each `call` opens a fresh connection to the
/// daemon socket, writes the one frame, reads the one reply.
#[derive(Debug)]
pub struct McpClient {
    sock: PathBuf,
    caller: CallerEnvelope,
}

impl McpClient {
    /// The client's identity — `caller` rides every frame verbatim.
    #[must_use]
    pub fn new(sock: &Path, caller: CallerEnvelope) -> Self {
        Self {
            sock: sock.to_path_buf(),
            caller,
        }
    }

    /// One JSON-RPC request's whole leg → the un-enveloped response
    /// (`{"v":1,"rpc":<response>}` stripped). Asserts the server closes
    /// after its one reply — the §4.11 rule a leaked connection would
    /// break.
    pub async fn call(&self, rpc: &Value) -> Value {
        let stream = UnixStream::connect(&self.sock).await.expect("connect");
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(&encode_request(&self.caller, rpc))
            .await
            .expect("frame write");
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .await
            .expect("reply read");
        assert!(read > 0, "the daemon closed without a reply");
        let reply = decode_reply(&line).expect("a v1 reply frame");
        let mut trailing = Vec::new();
        let extra = reader
            .read_until(b'\n', &mut trailing)
            .await
            .expect("trailing read");
        assert_eq!(extra, 0, "the connection closes after its one reply");
        reply
    }

    /// `tools/call` sugar: builds the request envelope for `name` +
    /// `arguments`, returns the decoded response.
    pub async fn call_tool(&self, id: Value, name: &str, arguments: Value) -> Value {
        self.call(&call_request(id, name, &arguments)).await
    }
}

/// The real `herdr-governor relay` as a child: JSON-RPC requests on
/// stdin, replies on stdout — the caller envelope is the relay's own
/// derivation, so a call through it proves the transport end to end.
#[derive(Debug)]
pub struct RelayClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<JoinHandle<String>>,
}

impl RelayClient {
    /// Spawn `relay --socket <sock>` with `cwd` as the invocation dir —
    /// the `projectRoot` derivation's input — and `HERDR_PANE_ID` set to
    /// `pane_id` (the ambient value is always scrubbed first, so `None`
    /// is a provably unset pane for the identity-refusal legs;
    /// `GIT_DIR`/`GIT_WORK_TREE` get the same scrub so a stray session
    /// env can't redirect the git-toplevel derivation).
    #[must_use]
    pub fn spawn(sock: &Path, cwd: &Path, pane_id: Option<&str>) -> Self {
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
        if let Some(pane) = pane_id {
            command.env("HERDR_PANE_ID", pane);
        }
        let mut child = command.spawn().expect("herdr-governor relay spawns");
        let stdin = child.stdin.take().expect("relay stdin");
        let stdout = BufReader::new(child.stdout.take().expect("relay stdout"));
        let mut pipe = child.stderr.take().expect("relay stderr");
        let stderr = tokio::spawn(async move {
            let mut text = String::new();
            pipe.read_to_string(&mut text).await.ok();
            text
        });
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr: Some(stderr),
        }
    }

    /// Write one JSON-RPC line — requests and notifications alike (a
    /// notification simply produces no reply to `recv`).
    pub async fn send(&mut self, rpc: &Value) {
        let mut line = serde_json::to_vec(rpc).expect("request encodes");
        line.push(b'\n');
        let stdin = self.stdin.as_mut().expect("relay stdin open");
        stdin.write_all(&line).await.expect("relay stdin write");
        stdin.flush().await.expect("relay stdin flush");
    }

    /// Read one reply line — the daemon's `rpc` payload verbatim (the
    /// v1 envelope never escapes the relay).
    pub async fn recv(&mut self) -> Value {
        let mut reply = Vec::new();
        let read = self
            .stdout
            .read_until(b'\n', &mut reply)
            .await
            .expect("relay reply");
        assert!(read > 0, "the relay closed stdout before a reply");
        serde_json::from_slice(&reply).expect("the relay reply is json")
    }

    /// One JSON-RPC request → the relay's reply line decoded.
    pub async fn call(&mut self, rpc: &Value) -> Value {
        self.send(rpc).await;
        self.recv().await
    }

    /// Write every line then read `replies` replies — the multi-line
    /// scripts the conversation tests drive (dropped notifications
    /// produce no reply, so `replies` may be less than `rpcs.len()`).
    pub async fn exchange_all(&mut self, rpcs: &[Value], replies: usize) -> Vec<Value> {
        for rpc in rpcs {
            self.send(rpc).await;
        }
        let mut out = Vec::with_capacity(replies);
        for _ in 0..replies {
            out.push(self.recv().await);
        }
        out
    }

    /// Close stdin — the session end the relay waits for (A1/S30; the
    /// drop is the EOF, a pipe has no `shutdown`) — then report the exit
    /// status plus the drained stderr.
    pub async fn close(mut self) -> (ExitStatus, String) {
        drop(self.stdin.take());
        let status = self.child.wait().await.expect("relay waits");
        let stderr = self
            .stderr
            .take()
            .expect("relay stderr")
            .await
            .expect("stderr drains");
        (status, stderr)
    }
}

impl Drop for RelayClient {
    fn drop(&mut self) {
        // A test that never calls `close` still leaves no relay behind.
        self.child.start_kill().ok();
    }
}
