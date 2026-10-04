//! P5.R1 — the stdio relay end to end (p5-plan §4.11, S30; F1, N4, N7):
//! a child `herdr-governor relay` runs `relay::run` in a real process so
//! the F1 identity derivation, the per-request fresh connection,
//! notification dropping, the daemon-down failure map, the stdin-EOF exit
//! and the 8 MB RSS bound are exercised over real pipes and a real unix
//! socket — the `store_probe` precedent for a child-process suite. I1
//! wired the `relay` subcommand to `relay::run`, so the suite runs the
//! real binary the harness itself would spawn.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tempfile::tempdir;

    /// The shipped relay binary — the OQ-R split keeps the std-only
    /// transport under the N4 bound with the daemon's deps GC'd.
    const BIN: &str = env!("CARGO_BIN_EXE_herdr-relay");
    /// N4: one stateless relay stays at or under 8 MB RSS.
    const RSS_LIMIT_KB: u64 = 8 * 1024;

    /// A request line the harness would write on the relay's stdin.
    fn request(id: u64, method: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {},
        })
        .to_string()
            + "\n"
    }

    /// A notification line: no `id` member, never forwarded.
    fn notification(method: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {},
        })
        .to_string()
            + "\n"
    }

    fn canonical(path: &Path) -> String {
        fs::canonicalize(path)
            .expect("canonicalize test path")
            .to_string_lossy()
            .into_owned()
    }

    /// A stand-in daemon on a unix socket: one line per connection,
    /// `{"v":1,"caller":…,"rpc":…}` recorded, answered
    /// `{"v":1,"rpc":{"jsonrpc":"2.0","id":<rpc.id>,"result":{"conn":<n>}}}`
    /// where `conn` is this socket's accept ordinal — the proof that every
    /// request rode a fresh connection.
    struct FakeDaemon {
        path: PathBuf,
        accepts: Arc<AtomicUsize>,
        frames: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl FakeDaemon {
        fn start(dir: &Path) -> Self {
            let path = dir.join("governor.sock");
            let listener = UnixListener::bind(&path).expect("bind fake daemon socket");
            let accepts = Arc::new(AtomicUsize::new(0));
            let frames = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let accepts_t = accepts.clone();
                let frames_t = frames.clone();
                let stop_t = stop.clone();
                thread::spawn(move || {
                    for accepted in listener.incoming() {
                        if stop_t.load(Ordering::SeqCst) {
                            break;
                        }
                        match accepted {
                            Ok(mut stream) => serve(&mut stream, &accepts_t, &frames_t),
                            Err(_) => break,
                        }
                    }
                })
            };
            Self {
                path,
                accepts,
                frames,
                stop,
                thread: Some(thread),
            }
        }

        fn accept_count(&self) -> usize {
            self.accepts.load(Ordering::SeqCst)
        }

        fn frames(&self) -> Vec<Value> {
            self.frames.lock().expect("frames lock").clone()
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Nudge `accept` so the loop observes the flag and exits.
            let _ignored = UnixStream::connect(&self.path);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("fake daemon thread");
            }
        }
    }

    fn serve(conn: &mut UnixStream, accepts: &AtomicUsize, frames: &Mutex<Vec<Value>>) {
        accepts.fetch_add(1, Ordering::SeqCst);
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&mut *conn);
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                return;
            }
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        frames.lock().expect("frames lock").push(frame.clone());
        let id = frame.pointer("/rpc/id").cloned().unwrap_or(Value::Null);
        let conn_ord = accepts.load(Ordering::SeqCst);
        let reply =
            json!({"v": 1, "rpc": {"jsonrpc": "2.0", "id": id, "result": {"conn": conn_ord}}});
        let mut bytes = reply.to_string().into_bytes();
        bytes.push(b'\n');
        let _ignored = conn.write_all(&bytes);
    }

    /// A spawned `herdr-governor relay` with piped stdin/stdout.
    struct RelayChild {
        child: Child,
        stdout: BufReader<ChildStdout>,
    }

    impl RelayChild {
        fn send(&mut self, line: &str) {
            self.child
                .stdin
                .as_mut()
                .expect("relay stdin")
                .write_all(line.as_bytes())
                .expect("write to relay stdin");
            self.child
                .stdin
                .as_mut()
                .expect("relay stdin")
                .flush()
                .expect("flush relay stdin");
        }

        /// Read one reply line off the relay's stdout.
        fn recv(&mut self) -> Value {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read relay stdout");
            assert!(n > 0, "relay closed stdout before a reply");
            serde_json::from_str(&line).expect("relay reply is json")
        }

        /// Close stdin (the EOF the harness sends at session end) and wait.
        fn close_and_wait(mut self) -> (ExitStatus, String) {
            drop(self.child.stdin.take());
            let status = self.child.wait().expect("wait on relay");
            let mut stderr = String::new();
            if let Some(mut err) = self.child.stderr.take() {
                let _ignored = err.read_to_string(&mut stderr);
            }
            (status, stderr)
        }
    }

    fn spawn_relay(socket: &Path, cwd: &Path, pane_id: Option<&str>) -> RelayChild {
        let mut command = Command::new(BIN);
        command
            .arg("--socket")
            .arg(socket)
            .current_dir(cwd)
            // The session env carries a real HERDR_PANE_ID; scrub it so the
            // child sees exactly what the case sets.
            .env_remove("HERDR_PANE_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pane) = pane_id {
            command.env("HERDR_PANE_ID", pane);
        }
        let mut child = command.spawn().expect("spawn herdr-governor relay");
        let stdout = BufReader::new(child.stdout.take().expect("relay stdout"));
        RelayChild { child, stdout }
    }

    /// Spawn the relay, send one request, return the recorded caller
    /// envelope and the reply. The daemon may outlive several spawns:
    /// exactly one new frame must arrive per exchange.
    fn one_exchange(daemon: &FakeDaemon, cwd: &Path, pane_id: Option<&str>) -> (Value, Value) {
        let before = daemon.frames().len();
        let mut relay = spawn_relay(&daemon.path, cwd, pane_id);
        relay.send(&request(1, "tools/list"));
        let reply = relay.recv();
        let frames = daemon.frames();
        assert_eq!(
            frames.len(),
            before.saturating_add(1),
            "exactly one forwarded frame per request",
        );
        let (status, stderr) = relay.close_and_wait();
        assert!(status.success(), "relay exit: {status} {stderr}");
        (frames.last().cloned().expect("a frame was recorded"), reply)
    }

    /// `git init` in `dir`, same `GIT_OPTIONAL_LOCKS=0` hygiene the relay
    /// itself applies.
    fn git_init(dir: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .status()
            .expect("run git init");
        assert!(status.success(), "git init in {}", dir.display());
    }

    /// F1 — `projectRoot` is realpath of the git toplevel inside a
    /// worktree (a nested cwd resolves to the root, not the subdir), and
    /// realpath of the invocation cwd outside one (ADR-0004).
    #[test]
    fn f1_relay_derives_root_from_git_toplevel_or_cwd() {
        let tmp = tempdir().expect("tempdir");
        let daemon = FakeDaemon::start(tmp.path());

        // Inside a repo subdirectory: the root wins over the subdir.
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("sub/dir")).expect("repo subdir");
        git_init(&repo);
        let (repo_frame, reply) = one_exchange(&daemon, &repo.join("sub/dir"), Some("w-test:r1"));
        assert_eq!(repo_frame["v"], 1);
        assert_eq!(
            repo_frame["caller"]["projectRoot"].as_str(),
            Some(canonical(&repo).as_str()),
            "projectRoot is the git toplevel, not the subdir",
        );
        assert_eq!(repo_frame["caller"]["paneId"], "w-test:r1");
        assert_eq!(repo_frame["rpc"]["method"], "tools/list");
        assert_eq!(reply["id"], 1);

        // Outside a worktree: realpath of the invocation cwd.
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).expect("plain dir");
        let (plain_frame, _) = one_exchange(&daemon, &plain, Some("w-test:r2"));
        assert_eq!(
            plain_frame["caller"]["projectRoot"].as_str(),
            Some(canonical(&plain).as_str()),
        );

        // No HERDR_PANE_ID: the request still goes out — the daemon
        // refuses CALLER_IDENTITY_INVALID; the relay never fabricates.
        let (nopane_frame, _) = one_exchange(&daemon, &plain, None);
        assert_eq!(nopane_frame["caller"]["paneId"], "");
    }

    /// F1/ADR-0004 — `relayInstanceId` is a random 128-bit id rendered
    /// lowercase hex, minted once per process: a respawn gets a new one.
    #[test]
    fn f1_relay_instance_id_is_32_hex_and_fresh_per_process() {
        let tmp = tempdir().expect("tempdir");
        let daemon = FakeDaemon::start(tmp.path());
        let (first, _) = one_exchange(&daemon, tmp.path(), Some("w-test:r1"));
        let (second, _) = one_exchange(&daemon, tmp.path(), Some("w-test:r1"));
        for frame in [&first, &second] {
            let id = frame["caller"]["relayInstanceId"]
                .as_str()
                .expect("relayInstanceId is a string");
            assert_eq!(id.len(), 32, "128-bit id is 32 hex chars: {id}");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "lowercase hex only: {id}",
            );
        }
        assert_ne!(
            first["caller"]["relayInstanceId"], second["caller"]["relayInstanceId"],
            "a respawned relay mints a fresh id",
        );
    }

    /// S30 — every request opens a fresh connection to the daemon socket
    /// (no upstream session state to strand calls behind a restart);
    /// notifications are dropped without a connection or a reply.
    #[test]
    fn relay_forwards_each_request_on_a_fresh_connection() {
        let tmp = tempdir().expect("tempdir");
        let daemon = FakeDaemon::start(tmp.path());
        let mut relay = spawn_relay(&daemon.path, tmp.path(), Some("w-test:r1"));

        relay.send(&request(1, "initialize"));
        relay.send(&notification("notifications/initialized"));
        relay.send(&request(3, "tools/list"));
        relay.send(&request(4, "tools/call"));

        let (first, third, fourth) = (relay.recv(), relay.recv(), relay.recv());
        assert_eq!(first["id"], 1);
        assert_eq!(first["result"]["conn"], 1);
        assert_eq!(third["id"], 3);
        assert_eq!(third["result"]["conn"], 2);
        assert_eq!(fourth["id"], 4);
        assert_eq!(fourth["result"]["conn"], 3);
        // The notification produced no connection: the request replies
        // already carry accepts 1, 2, 3. A bounded wait confirms no late
        // phantom connection from the dropped line.
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline && daemon.accept_count() == 3 {
            thread::yield_now();
        }
        assert_eq!(daemon.accept_count(), 3, "three requests, three accepts");
        assert_eq!(daemon.frames().len(), 3);
        let (status, stderr) = relay.close_and_wait();
        assert!(status.success(), "relay exit: {status} {stderr}");
    }

    /// A1 — the harness ends the server on stdin EOF; exit 0, nothing
    /// more. The socket need not exist: with no request there is nothing
    /// to forward.
    #[test]
    fn relay_exits_zero_on_stdin_eof() {
        let tmp = tempdir().expect("tempdir");
        let mut command = Command::new(BIN);
        command
            .arg("--socket")
            .arg(tmp.path().join("absent.sock"))
            .current_dir(tmp.path())
            .env_remove("HERDR_PANE_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn herdr-governor relay");
        let status = child.wait().expect("wait on relay");
        assert!(status.success(), "stdin EOF exits 0: {status}");
    }

    /// N7 — a daemon that is not there is `DAEMON_UNAVAILABLE`, mapped by
    /// method: `tools/call` gets the typed tool error (`isError` result
    /// whose text carries `{"code":"DAEMON_UNAVAILABLE"}`), every other
    /// method the JSON-RPC `-32000` error.
    #[test]
    fn n7_daemon_down_yields_daemon_unavailable_tool_error() {
        let tmp = tempdir().expect("tempdir");
        let absent = tmp.path().join("absent.sock");
        let mut relay = spawn_relay(&absent, tmp.path(), Some("w-test:r1"));

        relay.send(&request(7, "tools/call"));
        let reply = relay.recv();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["type"], "text");
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("tool error text");
        let code: Value = serde_json::from_str(text).expect("tool error text is json");
        assert_eq!(code["code"], "DAEMON_UNAVAILABLE");

        relay.send(&request(8, "initialize"));
        let refused = relay.recv();
        assert_eq!(refused["id"], 8);
        assert_eq!(refused["error"]["code"], -32000);
        assert_eq!(refused["error"]["message"], "DAEMON_UNAVAILABLE");

        let (status, stderr) = relay.close_and_wait();
        assert!(status.success(), "relay exit: {status} {stderr}");
    }

    /// F4 — an oversized stdin line earns its one `-32700` and nothing
    /// more: a valid request written behind it in the same `send` still
    /// forwards and gets its reply, and a line that crosses the bound
    /// while still pending drains through its newline instead of
    /// re-parsing its tail as a second fault. Replies arrive in wire
    /// order; the fourth read must hit the post-EOF silence.
    #[test]
    fn f4_oversized_line_never_loses_the_next_request() {
        let tmp = tempdir().expect("tempdir");
        let absent = tmp.path().join("absent.sock");
        let mut relay = spawn_relay(&absent, tmp.path(), Some("w-test:r1"));

        // The completed case: the oversized line's newline and a ping
        // ride one write together. `request` supplies its own newline.
        relay.send(&format!(
            "{}\n{}",
            "x".repeat(1024 * 1024 + 64),
            request(7, "ping")
        ));
        // The pending-overflow case: the bound is crossed before any
        // newline arrives, so the line's tail must drain, not re-parse.
        relay.send(&format!(
            "{}\n{}",
            "x".repeat(1024 * 1024 + 9_000),
            request(9, "ping")
        ));
        drop(relay.child.stdin.take());
        let mut raw = String::new();
        relay.stdout.read_to_string(&mut raw).expect("read");
        let replies: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).expect("relay reply is json"))
            .collect();
        assert_eq!(
            replies.len(),
            4,
            "exactly four replies — one fault and one answer per pair"
        );
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(
            replies[1]["id"], 7,
            "the ping behind the completed oversized line answers"
        );
        assert_eq!(replies[2]["id"], Value::Null);
        assert_eq!(replies[2]["error"]["code"], -32700);
        assert_eq!(
            replies[3]["id"], 9,
            "the ping behind the pending overflow answers"
        );

        let (status, stderr) = relay.close_and_wait();
        assert!(status.success(), "relay exit: {status} {stderr}");
    }

    /// N4 — the relay holds no state: after a hundred round trips its RSS
    /// is still under the 8 MB bound, read off `/proc/<pid>/status` while
    /// the child is alive.
    #[test]
    fn n4_relay_rss_under_8mb() {
        let tmp = tempdir().expect("tempdir");
        let daemon = FakeDaemon::start(tmp.path());
        let mut relay = spawn_relay(&daemon.path, tmp.path(), Some("w-test:r1"));
        for id in 0..100 {
            relay.send(&request(id, "tools/call"));
            let reply = relay.recv();
            assert_eq!(reply["id"], id);
        }
        let status_text = fs::read_to_string(format!("/proc/{}/status", relay.child.id()))
            .expect("read relay /proc status");
        let rss_kb: u64 = status_text
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kb| kb.parse().ok())
            .expect("VmRSS field present");
        assert!(
            rss_kb < RSS_LIMIT_KB,
            "relay RSS {rss_kb} kB exceeds the N4 bound of {RSS_LIMIT_KB} kB",
        );
        let (status, stderr) = relay.close_and_wait();
        assert!(status.success(), "relay exit: {status} {stderr}");
    }
}
