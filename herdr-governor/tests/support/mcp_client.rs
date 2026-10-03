//! `mcp_client` — the caller-side end of the harness transport (P5.T1):
//! `McpClient` frames a JSON-RPC request inside a chosen caller envelope
//! and opens a fresh daemon-socket connection per call (the v1 frame,
//! §4.11 one-request-per-connection); `RelayClient` spawns the real
//! `herdr-governor relay` subprocess so a request rides the identity
//! path — `HERDR_PANE_ID`→`paneId`, canonical `cwd`→`projectRoot`, the
//! relay-minted `relayInstanceId`.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use herdr_governor::mcp::framing::{decode_reply, encode_request};
use serde_json::Value;
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
        self.call(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .await
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
    /// is a provably unset pane for the identity-refusal legs).
    #[must_use]
    pub fn spawn(sock: &Path, cwd: &Path, pane_id: Option<&str>) -> Self {
        let mut command = Command::new(BIN);
        command
            .arg("relay")
            .arg("--socket")
            .arg(sock)
            .current_dir(cwd)
            .env_remove("HERDR_PANE_ID")
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

    /// One JSON-RPC request → the relay's reply line decoded (the
    /// daemon's `rpc` payload verbatim — the v1 envelope never escapes
    /// the relay).
    pub async fn call(&mut self, rpc: &Value) -> Value {
        let mut line = serde_json::to_vec(rpc).expect("request encodes");
        line.push(b'\n');
        let stdin = self.stdin.as_mut().expect("relay stdin open");
        stdin.write_all(&line).await.expect("relay stdin write");
        stdin.flush().await.expect("relay stdin flush");
        let mut reply = Vec::new();
        let read = self
            .stdout
            .read_until(b'\n', &mut reply)
            .await
            .expect("relay reply");
        assert!(read > 0, "the relay closed stdout before a reply");
        serde_json::from_slice(&reply).expect("the relay reply is json")
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
