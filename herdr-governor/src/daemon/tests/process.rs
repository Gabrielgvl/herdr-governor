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

/// An identifier byte (`[A-Za-z0-9_]`) — token boundaries for `tracing`
/// and the macro name after it.
fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The index just past a `"…"` literal opened at `i` — `\` escapes are
/// walked so an escaped quote never ends the literal early.
fn skip_string(bytes: &[u8], i: usize) -> usize {
    let mut j = i.saturating_add(1);
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j = j.saturating_add(1),
            b'"' => return j.saturating_add(1),
            _ => {}
        }
        j = j.saturating_add(1);
    }
    bytes.len()
}

/// `i` at an `r` that starts a raw string (`r"…"`, `r#"…"#`, …): the
/// index just past it, `None` when `r` is an ordinary identifier byte.
fn skip_raw_string(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i.saturating_add(1);
    while bytes.get(j) == Some(&b'#') {
        j = j.saturating_add(1);
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j.saturating_sub(i).saturating_sub(1);
    let mut k = j.saturating_add(1);
    while k < bytes.len() {
        if bytes[k] == b'"'
            && bytes
                .get(k.saturating_add(1)..=k.saturating_add(hashes))
                .is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
        {
            return Some(k.saturating_add(1).saturating_add(hashes));
        }
        k = k.saturating_add(1);
    }
    Some(bytes.len())
}

/// `i` at `'`: a char literal (`'x'`, `'\n'`, `'\u{…}'`) is walked past;
/// a lifetime (`'a `, `'static`) is not a literal — one byte suffices.
fn skip_char_or_lifetime(bytes: &[u8], i: usize) -> usize {
    if bytes.get(i.saturating_add(1)) == Some(&b'\\') {
        let mut j = i.saturating_add(2);
        while j < bytes.len() && bytes[j] != b'\'' {
            j = j.saturating_add(1);
        }
        return j.saturating_add(1);
    }
    if bytes.get(i.saturating_add(2)) == Some(&b'\'') {
        return i.saturating_add(3);
    }
    i.saturating_add(1)
}

/// The byte index just past the delimiter matching `bytes[open]` — a
/// tiny lexer so delimiters inside string/char literals and (nested)
/// comments never end a macro body early. `None` on unbalanced input.
fn matching_close(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth = depth.saturating_add(1),
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i.saturating_add(1));
                }
            }
            b'/' if bytes.get(i.saturating_add(1)) == Some(&b'/') => {
                i = bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |n| i.saturating_add(n));
                continue;
            }
            b'/' if bytes.get(i.saturating_add(1)) == Some(&b'*') => {
                // Rust block comments nest.
                let mut nested = 1_usize;
                let mut j = i.saturating_add(2);
                while j.saturating_add(1) < bytes.len() && nested > 0 {
                    // `/*` and `*/` can't start at the same byte — two
                    // independent probes, no else needed.
                    if bytes[j] == b'/' && bytes[j.saturating_add(1)] == b'*' {
                        nested = nested.saturating_add(1);
                        j = j.saturating_add(1);
                    }
                    if bytes[j] == b'*' && bytes[j.saturating_add(1)] == b'/' {
                        nested = nested.saturating_sub(1);
                        j = j.saturating_add(1);
                    }
                    j = j.saturating_add(1);
                }
                i = j;
                continue;
            }
            b'"' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'r' => {
                if let Some(next) = skip_raw_string(bytes, i) {
                    i = next;
                    continue;
                }
            }
            b'\'' => {
                i = skip_char_or_lifetime(bytes, i);
                continue;
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    None
}

/// The source's `tracing::<name>!` invocations as `(line, body)` pairs —
/// `body` spans the balanced delimiter after the `!`, so a field on a
/// continuation line scans with the macro it belongs to (F6/F8).
fn macro_invocations(source: &str) -> Vec<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut invocations = Vec::new();
    let mut cursor = 0_usize;
    while let Some(found) = source.get(cursor..).and_then(|rest| rest.find("tracing::")) {
        let at = cursor.saturating_add(found);
        cursor = at.saturating_add("tracing::".len());
        // `tracing` must be a whole token, not `mytracing::`'s tail.
        if at > 0 && word_byte(bytes[at.saturating_sub(1)]) {
            continue;
        }
        // Only `tracing::<name>!<delimiter>` is an invocation — a plain
        // `use tracing::info;` or `tracing::Level` is not.
        let mut i = cursor;
        while i < bytes.len() && word_byte(bytes[i]) {
            i = i.saturating_add(1);
        }
        if bytes.get(i) != Some(&b'!') {
            continue;
        }
        i = i.saturating_add(1);
        if !matches!(bytes.get(i), Some(b'(' | b'[' | b'{')) {
            continue;
        }
        let end = matching_close(bytes, i).unwrap_or(source.len());
        let line = source
            .get(..at)
            .unwrap_or_default()
            .matches('\n')
            .count()
            .saturating_add(1);
        invocations.push((line, source.get(at..end).unwrap_or_default()));
        cursor = end;
    }
    invocations
}

/// The forbidden fields found in `source` as `line: snippet` strings —
/// the tripwire's verdict on one file's text. Scans COMPLETE `tracing::`
/// macro invocations (the daemon's own style spreads fields across the
/// lines after the `tracing::` token), not lone lines.
fn forbidden_fields_in(source: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (line, body) in macro_invocations(source) {
        for word in FORBIDDEN_FIELDS {
            for at in word_at(body, word) {
                let after = body
                    .get(at.saturating_add(word.len())..)
                    .unwrap_or_default()
                    .trim_start();
                let before = body.get(..at).unwrap_or_default().trim_end();
                let is_field =
                    after.starts_with('=') || before.ends_with('?') || before.ends_with('%');
                if is_field {
                    offenders.push(format!("{line}: forbidden field `{word}` in `{body}`"));
                }
            }
        }
    }
    offenders
}

/// `log_helpers_take_only_ids_sizes_digests` — the lexical tripwire: scan
/// every `src/daemon/**` file; any `tracing::` call carrying one of the
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
        for hit in forbidden_fields_in(&source) {
            offenders.push(format!("{}:{hit}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

/// The negative control (F6/F8): a forbidden field on a CONTINUATION
/// line of a multi-line `tracing::` invocation must be caught — the
/// helpers' own style puts fields on the lines after the `tracing::`
/// token, so a per-line scan would wave the leak through.
#[test]
fn log_tripwire_scans_complete_macro_invocations() {
    // The control source splits its `tracing::` token — this file is
    // itself scanned by `log_helpers_take_only_ids_sizes_digests`, and a
    // literal invocation here would trip the real tripwire.
    let leak = concat!(
        "fn leak(run_id: &str, body: &str) {\n",
        "    tracing",
        "::info!(\n",
        "        run_id,\n",
        "        payload = body,\n",
        "        \"handoff written\"\n",
        "    );\n",
        "}\n",
    );
    let hits = forbidden_fields_in(leak);
    assert_eq!(hits.len(), 1, "the multi-line field is caught: {hits:?}");
    assert!(hits[0].contains("payload"), "{hits:?}");

    // A clean multi-line invocation reports nothing — this one carries
    // only allowed fields, so it is safe to write literally.
    let clean = r#"
fn fine(run_id: &str, n: usize) {
    tracing::info!(
        run_id,
        n,
        "handoff written"
    );
}
"#;
    assert!(
        forbidden_fields_in(clean).is_empty(),
        "fields that are ids/sizes pass"
    );
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
