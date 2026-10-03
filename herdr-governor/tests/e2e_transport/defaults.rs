//! `defaults` — the ADR-0004 default-path contract on real children: with
//! `XDG_STATE_HOME`/`XDG_CONFIG_HOME` set away from `$HOME`, a flagless
//! `daemon` and a flagless `relay` must still meet on the HOME-based
//! socket (`~/.local/state/herdr-governor/governor.sock`) — `HOME` is the
//! one documented env exception, so XDG must never split the pair. The
//! pin is a real `ping` round-trip: the relay's request reaches the
//! daemon only when both sides derived the same path.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use tempfile::tempdir;

use crate::support::e2e::{BIN, await_for, catalog, signal, stderr_of};

/// Write the `[daemon]` fixture (`catalog.toml` + `0600` credentials) at
/// `dir` — the same fixture `support::e2e::fixture` lays out, placed at
/// an explicit path because this test controls the env defaults.
fn fixture_at(dir: &Path) {
    fs::create_dir_all(dir).expect("config dir");
    fs::write(
        dir.join("catalog.toml"),
        catalog(Path::new("/nonexistent/herdr.sock"), 60),
    )
    .expect("catalog");
    let credentials = dir.join("credentials");
    fs::write(&credentials, "test-token\n").expect("credentials");
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).expect("chmod");
}

/// A flagless `BIN` child under the test's env: `HOME` set, the XDG vars
/// divergent, and the knobs a stray session could inject scrubbed. All
/// three stdio legs are piped — `stderr_of` drains the error leg so a
/// chatty child can't fill the pipe.
fn spawn(args: &[&str], home: &Path, xdg_state: &Path, xdg_config: &Path) -> Child {
    Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", xdg_state)
        .env("XDG_CONFIG_HOME", xdg_config)
        .env_remove("GOV_DAEMON_SEAM")
        .env_remove("HERDR_PANE_ID")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("child spawns")
}

/// F1+F7 — the default socket path is `~/.local/state/herdr-governor/
/// governor.sock` on BOTH sides, overridable by flags only: a `daemon`
/// spawned with no `--state-dir`/`--config-dir` under divergent XDG vars
/// binds the HOME path (and reads the HOME config), and a `relay`
/// spawned with no `--socket` under the same env reaches it.
#[tokio::test]
async fn default_socket_is_home_based_for_daemon_and_relay() {
    let tmp = tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let xdg_state = tmp.path().join("xdg-state");
    let xdg_config = tmp.path().join("xdg-config");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&xdg_state).expect("xdg state");
    fs::create_dir_all(&xdg_config).expect("xdg config");
    // A valid catalog under BOTH derivations: whichever the child reads,
    // startup can proceed — the verdict is WHERE the socket lands, never
    // a missing fixture.
    fixture_at(&home.join(".config/herdr-governor"));
    fixture_at(&xdg_config.join("herdr-governor"));

    let mut daemon = spawn(&["daemon"], &home, &xdg_state, &xdg_config);
    let stderr = stderr_of(&mut daemon);

    let home_sock = home.join(".local/state/herdr-governor/governor.sock");
    await_for("the HOME-based default socket to bind", || {
        home_sock.exists()
    })
    .await;
    assert!(
        !xdg_state.join("herdr-governor/governor.sock").exists(),
        "the daemon never binds under XDG_STATE_HOME"
    );

    // The relay under the same env and no --socket: one `ping` line
    // round-trips only when it dialed the daemon's own default.
    let mut relay = spawn(&["relay"], &home, &xdg_state, &xdg_config);
    relay
        .stdin
        .as_mut()
        .expect("relay stdin")
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
        .and_then(|()| relay.stdin.as_mut().expect("relay stdin").flush())
        .expect("write ping");
    let exchange = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        BufReader::new(relay.stdout.take().expect("relay stdout"))
            .read_line(&mut line)
            .expect("read relay reply");
        (relay, line)
    })
    .await
    .expect("relay exchange joins");
    let (mut relay_child, reply) = exchange;
    assert!(
        reply.contains("\"result\""),
        "the relay reached the daemon on the shared default: {reply}"
    );

    drop(relay_child.stdin.take());
    let _relay_exit = relay_child.wait().expect("relay exits on EOF");
    signal(&daemon, "-TERM");
    let status = daemon.wait().expect("daemon exits");
    assert!(status.success(), "clean SIGTERM teardown: {status}");
    let _stderr = stderr.join().expect("stderr drained");
}
