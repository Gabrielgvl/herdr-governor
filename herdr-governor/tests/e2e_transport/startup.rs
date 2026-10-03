//! `startup` — the F27/F28 *child* tests: `daemon` spawned as a real
//! process, so `flock`, the socket and signals behave exactly as they do
//! for the operator. Every wait is a bounded poll — never a sleep.

use std::path::Path;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, await_for, fixture, probe};

/// A fixture daemon pointing at a nowhere Herdr socket — the tick's
/// snapshot fails and is logged, which is what A1 does with Herdr
/// liveness.
fn dirs() -> DaemonDirs {
    fixture(&Catalog::inert(
        Path::new("/nonexistent/herdr.sock"),
        "http://127.0.0.1:9",
    ))
}

/// F27 — an invalid catalog refuses with exit 2 and exactly one stderr
/// line: sanitized (no control characters, no second line of the
/// underlying error, ≤ 500 chars) and no file contents.
#[tokio::test]
async fn f27_invalid_startup_refuses_with_one_sanitized_line() {
    let dirs = dirs();
    std::fs::write(dirs.config_dir().join("catalog.toml"), "not = [toml\n")
        .expect("broken catalog");

    let daemon = TestDaemon::spawn_raw(&dirs.settings(), None);
    let (status, text) = daemon.wait().await;

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
#[tokio::test]
async fn f28_lock_and_probe_refuse_second_daemon() {
    let dirs = dirs();
    let sock = dirs.socket_path();

    let mut holder = TestDaemon::spawn_child(&dirs.settings(), None).await;
    await_for("the holder answers", || probe(&sock)).await;

    let second = TestDaemon::spawn_raw(&dirs.settings(), None);
    let (second_status, text) = second.wait().await;
    assert_eq!(second_status.code(), Some(3), "lock-held is exit 3: {text}");
    assert!(
        text.contains("socket answers: true"),
        "the refusal reports the live probe: {text}"
    );

    assert!(holder.alive(), "the holder is still running");
    assert!(probe(&sock), "the holder still answers the probe");

    holder.signal("-TERM");
    let (holder_status, holder_err) = holder.wait().await;
    assert!(
        holder_status.success(),
        "SIGTERM stops cleanly: {holder_status} {holder_err}"
    );
    assert!(!sock.exists(), "teardown removed the socket");
}

/// F28 — kill -9 leaves the socket file behind; the next daemon takes the
/// lock, unlinks the stale socket and binds.
#[tokio::test]
async fn f28_restart_after_kill_removes_stale_socket_and_binds() {
    let dirs = dirs();
    let sock = dirs.socket_path();

    let first = TestDaemon::spawn_child(&dirs.settings(), None).await;
    await_for("the first answers", || probe(&sock)).await;

    first.signal("-9");
    let (first_status, _first_err) = first.wait().await;
    assert!(!first_status.success(), "SIGKILL is not a clean exit");
    assert!(sock.exists(), "the killed daemon left a stale socket");

    let second = TestDaemon::spawn_raw(&dirs.settings(), None);
    await_for("the successor answers", || probe(&sock)).await;
    assert!(probe(&sock), "the restarted daemon answers");

    second.signal("-TERM");
    let (second_status, second_err) = second.wait().await;
    assert!(
        second_status.success(),
        "the restarted daemon stops cleanly: {second_err}"
    );
}
