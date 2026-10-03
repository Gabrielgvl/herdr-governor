//! `startup` — the F27/F28 *child* tests: `daemon` spawned as a real
//! process, so `flock`, the socket and signals behave exactly as they do
//! for the operator. Every wait is a bounded poll — never a sleep.

use std::io::{BufRead as _, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_herdr-governor");
const PROBE_FRAME: &[u8] = br#"{"jsonrpc":"2.0","id":"gov:probe","method":"ping"}"#;
const DEADLINE: Duration = Duration::from_secs(10);

/// A valid fixture: `[daemon]` + a `0600` credential. `herdr_socket`
/// points nowhere — the tick's snapshot fails and is logged, which is
/// what A1 does with Herdr liveness.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let state = root.join("state");
    let config = root.join("config");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("catalog.toml"),
        "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n\
         [catalog]\noperating_points = []\n\n\
         [daemon]\nherdr_socket = \"/nonexistent/herdr.sock\"\n\
         jev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n",
    )
    .unwrap();
    let credentials = config.join("credentials");
    std::fs::write(&credentials, "test-token\n").unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    (state, config)
}

fn spawn_daemon(state: &Path, config: &Path) -> Child {
    Command::new(BIN)
        .args([
            "daemon",
            "--state-dir",
            state.to_str().unwrap(),
            "--config-dir",
            config.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon spawns")
}

/// Poll `until` until it holds or the deadline passes — then panic.
fn wait_for(what: &str, mut until: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        if until() {
            return;
        }
        std::thread::park_timeout(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// The lock probe's read: connect, send the frame, read one line.
fn probe(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _timeout = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(PROBE_FRAME)
        .and_then(|()| stream.write_all(b"\n"))
        .is_err()
    {
        return false;
    }
    let mut reply = String::new();
    let _read = std::io::BufReader::new(stream).read_line(&mut reply);
    reply.contains("\"result\"")
}

fn signal(child: &Child, sig: &str) {
    let status = Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success(), "kill {sig} {}", child.id());
}

/// The child's buffered stderr (read on a thread so a chatty child never
/// deadlocks the pipe).
fn stderr_of(child: &mut Child) -> std::thread::JoinHandle<String> {
    let mut pipe = child.stderr.take().expect("stderr piped");
    std::thread::spawn(move || {
        let mut text = String::new();
        let _read = pipe.read_to_string(&mut text);
        text
    })
}

/// F27 — an invalid catalog refuses with exit 2 and exactly one stderr
/// line: sanitized (no control characters, no second line of the
/// underlying error, ≤ 500 chars) and no file contents.
#[test]
fn f27_invalid_startup_refuses_with_one_sanitized_line() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    std::fs::write(config.join("catalog.toml"), "not = [toml\n").unwrap();

    let mut child = spawn_daemon(&state, &config);
    let stderr = stderr_of(&mut child);
    let status = child.wait().expect("wait");
    let text = stderr.join().expect("stderr");

    assert_eq!(status.code(), Some(2), "config refusal is exit 2: {text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "the refusal is exactly one line (F27): {text:?}"
    );
    let line = lines.first().copied().unwrap_or_default();
    assert!(
        line.len() <= 500,
        "sanitized line ≤ 500 chars: {}",
        line.len()
    );
    assert!(
        !line.chars().any(char::is_control),
        "no control characters: {line:?}"
    );
    assert!(
        !line.contains("tiers") && !line.contains("daemon]"),
        "no catalog contents leak: {line}"
    );
}

/// F28 — a second daemon against a held state dir refuses with exit 3
/// while the holder keeps serving (the probe still answers).
#[test]
fn f28_lock_and_probe_refuse_second_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let sock = state.join("governor.sock");

    let mut holder = spawn_daemon(&state, &config);
    wait_for("holder socket", || probe(&sock));

    let mut second = spawn_daemon(&state, &config);
    let stderr = stderr_of(&mut second);
    let second_status = second.wait().expect("second exits");
    let text = stderr.join().expect("stderr");
    assert_eq!(second_status.code(), Some(3), "lock-held is exit 3: {text}");
    assert!(
        text.contains("socket answers: true"),
        "the refusal reports the live probe: {text}"
    );

    assert_eq!(
        holder.try_wait().expect("try_wait"),
        None,
        "the holder is still running"
    );
    assert!(probe(&sock), "the holder still answers the probe");

    signal(&holder, "-TERM");
    let holder_status = holder.wait().expect("holder exits");
    assert!(
        holder_status.success(),
        "SIGTERM stops cleanly: {holder_status}"
    );
    assert!(!sock.exists(), "teardown removed the socket");
}

/// F28 — kill -9 leaves the socket file behind; the next daemon takes the
/// lock, unlinks the stale socket and binds.
#[test]
fn f28_restart_after_kill_removes_stale_socket_and_binds() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = fixture(tmp.path());
    let sock = state.join("governor.sock");

    let mut first = spawn_daemon(&state, &config);
    wait_for("first socket", || probe(&sock));

    signal(&first, "-9");
    let first_status = first.wait().expect("first exits");
    assert!(!first_status.success(), "SIGKILL is not a clean exit");
    assert!(sock.exists(), "the killed daemon left a stale socket");

    let mut second = spawn_daemon(&state, &config);
    wait_for("second socket", || probe(&sock));
    assert!(probe(&sock), "the restarted daemon answers");

    signal(&second, "-TERM");
    let second_status = second.wait().expect("second exits");
    assert!(
        second_status.success(),
        "the restarted daemon stops cleanly"
    );
}
