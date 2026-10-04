//! `devin_log` — the F31 typed provider-limit probes (§4.17): the
//! session-bound process-log scan against the committed fixture (a
//! redacted copy of the reference's `devin-provider-limit.log`) plus the
//! staged trust/binding/`cycle_start` refusals, and the session-log 429
//! record's `isApiErrorMessage`/`apiErrorStatus`/`requestId` contract.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use governor_core::identity::Timestamp;
use herdr_governor::adapters::transcript::{LimitRecord, TranscriptRoots, limit_record};
use tempfile::{TempDir, tempdir};

use super::{no_roots, pointer, stage};

/// `tests/fixtures/contract/devin-provider-limit.log`.
fn fixture() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/contract/devin-provider-limit.log"),
    )
    .expect("the committed fixture reads")
}

/// `2026-10-02T01:30:00Z` — before the fixture's stall line
/// (`01:33:04.421266Z`), so the record counts.
const CYCLE: Timestamp = Timestamp(1_790_904_600_000);

/// The fixture's stall line — `2026-10-02T01:33:04.421266Z`.
const STALL_MS: i64 = 1_790_904_784_421;

/// Stage `bytes` as `dir`'s `devin_<stamp>_<pid>.log` process log.
fn stage_log(dir: &Path, bytes: &[u8]) -> PathBuf {
    stage(dir, "devin_20261002-013259_4242.log", bytes)
}

async fn probe(dir: &TempDir, session: &str) -> Option<LimitRecord> {
    limit_record(
        &pointer("devin", session, None),
        &no_roots(),
        Some(dir.path()),
        CYCLE,
    )
    .await
}

/// The session-bound log parses: the stall line's timestamp is the
/// record's `observed_at` and `Your limit will reset in 26 minutes` is
/// the stated `reset_at`.
#[tokio::test]
async fn devin_log_binds_session_and_parses_limit_line() {
    let dir = tempdir().expect("dir");
    stage_log(dir.path(), &fixture());
    let Some(record) = probe(&dir, "tidal-vase").await else {
        panic!("the bound log's stall line is the record")
    };
    assert_eq!(record.source, "devin_process_log");
    assert_eq!(record.observed_at, Timestamp(STALL_MS));
    assert_eq!(
        record.reset_at,
        Some(Timestamp(STALL_MS.saturating_add(1_560_000))),
        "26 minutes after the stall line"
    );
    assert_eq!(record.record_id(), "devin_process_log:1790904784421");
    assert_eq!(record.observed_at_rfc3339(), "2026-10-02T01:33:04.421Z");
    assert_eq!(
        record.reset_at_rfc3339().as_deref(),
        Some("2026-10-02T01:59:04.421Z")
    );
}

/// A world-writable log and a symlinked candidate are both refused —
/// untrusted native evidence is never a record.
#[tokio::test]
async fn devin_log_refuses_world_writable_and_symlink() {
    let dir = tempdir().expect("dir");
    let path = stage_log(dir.path(), &fixture());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod 0666");
    assert!(
        probe(&dir, "tidal-vase").await.is_none(),
        "a world-writable log is untrusted"
    );
    std::fs::remove_file(&path).expect("remove");

    std::os::unix::fs::symlink("/etc/hostname", &path).expect("symlink");
    assert!(
        probe(&dir, "tidal-vase").await.is_none(),
        "a symlinked candidate is untrusted"
    );
}

/// A log bound to a different session is foreign; a bound log whose only
/// stall line predates `cycle_start` proves nothing.
#[tokio::test]
async fn devin_log_ignores_other_sessions_and_old_lines() {
    let dir = tempdir().expect("dir");
    let foreign = String::from_utf8(fixture())
        .expect("fixture is utf8")
        .replace("tidal-vase", "other-session");
    stage_log(dir.path(), foreign.as_bytes());
    assert!(
        probe(&dir, "tidal-vase").await.is_none(),
        "a log naming another session is foreign"
    );

    // The bound session's stall line older than the cycle is no record.
    let old = String::from_utf8(fixture())
        .expect("fixture is utf8")
        .replace("01:33:0", "01:20:0");
    let dir2 = tempdir().expect("dir2");
    stage_log(dir2.path(), old.as_bytes());
    assert!(
        probe(&dir2, "tidal-vase").await.is_none(),
        "a stall line before cycle_start does not count"
    );
}

/// `.log.gz` archives are never decompressed or inspected — a gzipped
/// session log is no verdict (F36).
#[tokio::test]
async fn devin_log_ignores_gz_archives_and_returns_no_verdict() {
    let dir = tempdir().expect("dir");
    stage(dir.path(), "devin_20261002-013259_4242.log.gz", &fixture());
    assert!(
        probe(&dir, "tidal-vase").await.is_none(),
        "the archive is never a candidate"
    );
}

/// The session-log 429 record counts only as the fully-typed provider
/// error: `assistant` + `isApiErrorMessage` + `rate_limit` + status 429 +
/// a non-empty `requestId`, bound to the session's `sessionId`/`cwd` and
/// timestamped at or after `cycle_start`.
#[tokio::test]
async fn claude_limit_record_requires_429_and_request_id() {
    let dir = tempdir().expect("dir");
    let session = "11111111-2222-3333-4444-555555555555";
    let cwd = "/work";
    let roots = TranscriptRoots::new(Vec::new(), Vec::from([dir.path().join("projects")]));
    let leaf = format!("{session}.jsonl");
    let record = |extra: serde_json::Value| {
        let mut body = serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "error": "rate_limit",
            "apiErrorStatus": 429,
            "requestId": "req-1",
            "sessionId": session,
            "cwd": cwd,
            "timestamp": "2026-10-02T01:35:00Z",
        });
        body.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        format!("{body}\n")
    };

    // The full record — `retryAfterSeconds` states the reset.
    let path = stage(
        &dir.path().join("projects/-work"),
        &leaf,
        record(serde_json::json!({"retryAfterSeconds": 1800})).as_bytes(),
    );
    let pointer = pointer("claude", session, Some(cwd));
    let Some(found) = limit_record(&pointer, &roots, None, CYCLE).await else {
        panic!("the typed 429 record is the record")
    };
    assert_eq!(found.source, "claude_session_quota");
    assert_eq!(found.observed_at, Timestamp(1_790_904_900_000));
    assert_eq!(
        found.reset_at,
        Some(Timestamp(1_790_904_900_000_i64.saturating_add(1_800_000))),
        "the stated 1800 s delay"
    );

    for (name, broken) in [
        (
            "no requestId",
            record(serde_json::json!({"requestId": null})),
        ),
        (
            "empty requestId",
            record(serde_json::json!({"requestId": ""})),
        ),
        (
            "not a 429",
            record(serde_json::json!({"apiErrorStatus": 500})),
        ),
        (
            "not an api error",
            record(serde_json::json!({"isApiErrorMessage": false})),
        ),
        (
            "a foreign session",
            record(serde_json::json!({"sessionId": "99999999-8888-7777-6666-555555555555"})),
        ),
        (
            "a foreign cwd",
            record(serde_json::json!({"cwd": "/elsewhere"})),
        ),
    ] {
        std::fs::write(&path, broken).expect("rewrite");
        assert!(
            limit_record(&pointer, &roots, None, CYCLE).await.is_none(),
            "{name} is no record"
        );
    }
}
