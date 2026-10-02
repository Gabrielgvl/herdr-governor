//! a5 claude legs — the slugged JSONL reader against the committed
//! samples (`a5-probe-outcomes.json` case ids, one test each). The
//! samples' session id is the zero uuid.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use tempfile::tempdir;

use super::{pointer, privileged_runner, samples, stage};
use herdr_governor::adapters::transcript::{
    Cursor, EventKind, TranscriptError, TranscriptRoots, read_window, resolve,
};

const UUID: &str = "00000000-0000-4000-8000-000000000001";

/// Roots with `dir` as the only project tree.
fn project_roots(dir: PathBuf) -> TranscriptRoots {
    TranscriptRoots::new(Vec::new(), vec![dir])
}

/// Stage `fixture` under `projects/<slug>/<uuid>.jsonl` in `dir`.
fn stage_session(dir: &std::path::Path, slug: &str, fixture: &str) -> PathBuf {
    let bytes = std::fs::read(samples().join(fixture)).unwrap();
    stage(dir, &format!("projects/{slug}/{UUID}.jsonl"), &bytes)
}

#[tokio::test]
async fn a5_claude_ambiguous_id_files() {
    // two slugs carrying the same session file are ambiguous — no general
    // resolver; the cwd-pinned slug resolves to exactly one.
    let roots = project_roots(samples().join("claude-projects"));
    let err = resolve(&pointer("claude", UUID, None), &roots)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::Ambiguous { candidates: 2 }),
        "two slug candidates are ambiguous: {err:?}"
    );

    let src = resolve(&pointer("claude", UUID, Some("/synthetic/project")), &roots)
        .await
        .unwrap();
    assert!(
        src.path()
            .ends_with(format!("-synthetic-project/{UUID}.jsonl")),
        "the pinned slug resolves: {}",
        src.path().display()
    );
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 1, "the pinned file reads");
    assert_eq!(
        w.events.first().map(|e| e.kind),
        Some(EventKind::Error),
        "the quota record is an error event"
    );
}

#[tokio::test]
async fn a5_claude_partial_and_corrupt_records() {
    // unterminated tail pending; a complete record plus a partial tool
    // record emits only the complete one; a malformed complete record
    // fails the window — corruption defeats a no-progress proof
    // (A5-CORRUPT-NOT-ABSENCE).
    let dir = tempdir().unwrap();
    let roots = project_roots(dir.path().join("projects"));

    stage_session(
        dir.path(),
        "-synthetic-project",
        "claude-quota-partial.jsonl",
    );
    let src1 = resolve(&pointer("claude", UUID, None), &roots)
        .await
        .unwrap();
    let w1 = read_window(&src1, Cursor::START).await.unwrap();
    assert!(w1.events.is_empty(), "the unterminated record is pending");
    assert_eq!(w1.cursor, Cursor::START, "nothing consumed");
    drop(src1);

    let dir2 = tempdir().unwrap();
    let roots2 = project_roots(dir2.path().join("projects"));
    stage_session(
        dir2.path(),
        "-synthetic-project",
        "claude-quota-plus-partial-tool.jsonl",
    );
    let src2 = resolve(&pointer("claude", UUID, None), &roots2)
        .await
        .unwrap();
    let w2 = read_window(&src2, Cursor::START).await.unwrap();
    assert_eq!(
        w2.events.len(),
        1,
        "the quota record emits; the tail is pending"
    );
    drop(src2);

    let dir3 = tempdir().unwrap();
    let roots3 = project_roots(dir3.path().join("projects"));
    stage_session(
        dir3.path(),
        "-synthetic-project",
        "claude-malformed-then-quota.jsonl",
    );
    let src3 = resolve(&pointer("claude", UUID, None), &roots3)
        .await
        .unwrap();
    let err = read_window(&src3, Cursor::START).await.unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceMalformed { offset: 0, .. }),
        "the malformed first record fails the window: {err:?}"
    );
}

#[tokio::test]
async fn a5_claude_permission_denied() {
    if privileged_runner() {
        return; // DAC override reads a 000-mode file — mechanism n/a
    }
    let dir = tempdir().unwrap();
    let roots = project_roots(dir.path().join("projects"));
    let path = stage_session(dir.path(), "-synthetic-project", "claude-quota.jsonl");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = resolve(&pointer("claude", UUID, None), &roots)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "EACCES" }),
        "permission denied is a typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_claude_vanish_before_open() {
    let dir = tempdir().unwrap();
    let roots = project_roots(dir.path().join("projects"));
    let err = resolve(&pointer("claude", UUID, None), &roots)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "ENOENT" }),
        "no candidate is typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_claude_vanish_after_open_before_read() {
    // the pinned inode still reads after the path vanishes.
    let dir = tempdir().unwrap();
    let roots = project_roots(dir.path().join("projects"));
    let path = stage_session(dir.path(), "-synthetic-project", "claude-quota.jsonl");
    let src = resolve(&pointer("claude", UUID, None), &roots)
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 1, "the pinned inode still reads");
}
