//! `process` — the lexical §4.18 tripwire plus the process-level
//! contract: `check_config`'s exit codes and `run`'s bring-up, serve and
//! teardown over real files.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::daemon::paths::Paths;
use crate::daemon::settings::Settings;
use crate::daemon::{DaemonError, check_config, lock, run};

// — `log` tripwire (§4.18) —————————————————————————————————————————————

/// The content-carrying field names a `tracing::` call must never log —
/// follow-up text, handoff bodies, argv, env, payloads. `message` is the
/// implicit format-arg field; naming it explicitly means the same leak.
const FORBIDDEN_FIELDS: &[&str] = &[
    "text", "body", "content", "payload", "argv", "env", "raw", "message",
];

/// Is `word` present in `line` as a whole identifier (bounded by
/// non-`[A-Za-z0-9_]` characters)?
fn word_at(line: &str, word: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let word_ok = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut hits = Vec::new();
    let mut start = 0;
    while let Some(found) = line.get(start..).and_then(|rest| rest.find(word)) {
        let at = start.saturating_add(found);
        let before_ok = at == 0 || bytes.get(at.saturating_sub(1)).is_none_or(|b| !word_ok(*b));
        let end = at.saturating_add(word.len());
        let after_ok = bytes.get(end).is_none_or(|b| !word_ok(*b));
        if before_ok && after_ok {
            hits.push(at);
        }
        start = at.saturating_add(1);
    }
    hits
}

/// `log_helpers_take_only_ids_sizes_digests` — the lexical tripwire: scan
/// every `src/daemon/**` file; any `tracing::` line carrying one of the
/// forbidden names as a field (`name =`, `?name`, `%name`) fails.
#[test]
fn log_helpers_take_only_ids_sizes_digests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                files.push(path);
            } else {
                // Not a Rust source file — nothing to scan.
            }
        }
    }
    assert!(files.len() >= 10, "the daemon tree is scanned");
    let mut offenders = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).expect("read source");
        for (lineno, line) in source.lines().enumerate() {
            if !line.contains("tracing::") {
                continue;
            }
            for word in FORBIDDEN_FIELDS {
                for at in word_at(line, word) {
                    let after = line
                        .get(at.saturating_add(word.len())..)
                        .unwrap_or_default()
                        .trim_start();
                    let before = line.get(..at).unwrap_or_default().trim_end();
                    let is_field =
                        after.starts_with('=') || before.ends_with('?') || before.ends_with('%');
                    if is_field {
                        offenders.push(format!(
                            "{}:{}: forbidden field `{word}` in `{line}`",
                            file.display(),
                            lineno.saturating_add(1)
                        ));
                    }
                }
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

// — `check_config` —————————————————————————————————————————————————————

fn write_config(dir: &Path, daemon_table: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("mkdir config");
    let catalog = format!(
        "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n[catalog]\noperating_points = []\n\n{daemon_table}"
    );
    let path = dir.join("catalog.toml");
    fs::write(&path, catalog).expect("write catalog");
    let credentials = dir.join("credentials");
    fs::write(&credentials, "test-token\n").expect("write credentials");
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).expect("chmod");
    dir.to_path_buf()
}

/// `check-config` exit codes: valid → ok; decode/`NoDaemonTable`/read → 2.
#[tokio::test]
async fn check_config_exit_codes() {
    let tmp = tempfile::tempdir().expect("tmp");

    // Valid catalog + [daemon] → Ok with the summary counts.
    let dir = write_config(
        &tmp.path().join("good"),
        "[daemon]\nherdr_socket = \"/x/herdr.sock\"\njev_base_url = \"https://a.invalid\"\njev_model = \"m\"\n",
    );
    let summary = check_config(&dir).await.expect("valid config checks ok");
    assert_eq!(summary.version.0.len(), 64, "sha256 digest");
    assert_eq!(summary.operating_points, 0);
    assert_eq!(summary.tiers, 1);
    assert!(summary.line().contains("catalog ok:"));

    // Broken TOML → Config error → exit 2.
    let bad_dir = tmp.path().join("bad");
    fs::create_dir_all(&bad_dir).expect("mkdir");
    fs::write(bad_dir.join("catalog.toml"), "not = [toml\n").expect("write");
    let err = check_config(&bad_dir).await.expect_err("broken toml fails");
    assert!(
        matches!(err, DaemonError::Config(_)),
        "decode error: {err:?}"
    );
    assert_eq!(err.code(), ExitCode::from(2));

    // Valid core catalog but missing [daemon] → NoDaemonTable → exit 2.
    let nod_dir = write_config(&tmp.path().join("nod"), "");
    let nod_err = check_config(&nod_dir).await.expect_err("no [daemon] fails");
    assert!(
        matches!(nod_err, DaemonError::NoDaemonTable),
        "missing table: {nod_err:?}"
    );
    assert_eq!(nod_err.code(), ExitCode::from(2));

    // Missing file → Config(Read) → exit 2.
    let absent_err = check_config(&tmp.path().join("absent"))
        .await
        .expect_err("missing file fails");
    assert_eq!(absent_err.code(), ExitCode::from(2));
}

// — `run` in-process ——————————————————————————————————————————————————

/// §4.3/§4.14 — `run` brings up the socket, answers the probe, and the
/// oneshot tears down in order: socket removed, lock released.
#[tokio::test]
async fn run_binds_serves_and_stops_clean_in_process() {
    let tmp = tempfile::tempdir().expect("tmp");
    let state = tmp.path().join("state");
    let config = write_config(
        &tmp.path().join("config"),
        "[daemon]\nherdr_socket = \"/nonexistent/herdr.sock\"\njev_base_url = \"http://127.0.0.1:9\"\njev_model = \"m\"\nreconcile_secs = 3600\n",
    );

    let settings = Settings {
        state_dir: state.clone(),
        config_dir: config,
        herdr_socket: None,
        reconcile_secs: None,
    };
    let (tx, rx) = oneshot::channel::<()>();
    let daemon = tokio::spawn(run(settings, None, Some(rx)));

    let paths = Paths::create(&state).expect("paths");
    let sock = paths.sock();
    for _ in 0..400 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(sock.exists(), "the listener bound");

    // The probe answers `{}` — the same read the lock's `Held` check makes.
    // Blocking std I/O, so it runs on a blocking thread — a synchronous
    // call would freeze the current_thread runtime's accept task.
    let answered = tokio::task::spawn_blocking({
        let probe_sock = sock.clone();
        move || lock::probe(&probe_sock)
    })
    .await
    .expect("probe joins");
    assert!(answered, "the daemon answers the probe");

    tx.send(()).expect("stop");
    let code = daemon.await.expect("join").expect("run exits ok");
    assert_eq!(code, ExitCode::SUCCESS, "clean stop");
    assert!(!sock.exists(), "teardown removed the socket");
    let (_lock, _removed) = lock::acquire(&paths).expect("lock released at teardown");
}
