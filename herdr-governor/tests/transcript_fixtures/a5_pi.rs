//! a5 pi legs — the LF-delimited session reader against the committed
//! samples (`a5-probe-outcomes.json` case ids, one test each).

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;

use tempfile::tempdir;

use super::{no_roots, pointer, privileged_runner, samples, stage};
use herdr_governor::adapters::transcript::{Cursor, TranscriptError, read_window, resolve};

#[tokio::test]
async fn a5_pi_partial_utf8() {
    // first read: the header emits, the partial-UTF-8 tail stays pending;
    // appending the rest emits it — the tail was retained, not rejected.
    let dir = tempdir().unwrap();
    let bytes = std::fs::read(samples().join("pi-header-plus-partial-utf8.jsonl")).unwrap();
    let path = stage(
        dir.path(),
        "2026-09-28T00-00-00-000Z_native-synthetic.jsonl",
        &bytes,
    );
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let first = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(first.events.len(), 1, "header only — tail is pending");
    assert_eq!(first.byte_count, 82, "header line bytes incl LF");
    assert_eq!(first.cursor.position, 82, "consumed through the header LF");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(&[0xa9, b'"', b'}', b']', b'}', b'}', b'\n'])
        .unwrap();
    drop(f);
    let next = read_window(&src, first.cursor).await.unwrap();
    assert_eq!(next.events.len(), 1, "the completed record emits");
    assert_eq!(next.byte_count, 90, "the completed record's bytes");
    assert_eq!(next.cursor.position, 172, "cursor past both records");
}

#[tokio::test]
async fn a5_pi_complete_json_without_newline() {
    // LF, not JSON validity, is the record boundary: a complete record
    // without its LF is pending — nothing emits, the cursor stays.
    let path = samples().join("pi-complete-record-no-newline.jsonl");
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert!(w.events.is_empty(), "complete JSON without LF is pending");
    assert_eq!(w.cursor, Cursor::START, "nothing was consumed");
    assert_eq!(w.byte_count, 0, "no bytes emitted");
}

#[tokio::test]
async fn a5_pi_malformed_complete_record() {
    // a malformed complete record fails the whole window at its offset —
    // accumulated events are not emitted ahead of the fault.
    let path = samples().join("pi-header-plus-malformed.jsonl");
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceMalformed { offset: 82, .. }),
        "malformed record at byte 82 fails the window: {err:?}"
    );
}

#[tokio::test]
async fn a5_pi_header_identity_not_validated() {
    // the reference reader emitted the foreign header; the governor
    // validates it — a header id that isn't the filename's is malformed.
    let dir = tempdir().unwrap();
    let bytes = std::fs::read(samples().join("pi-foreign-header.jsonl")).unwrap();
    let path = stage(
        dir.path(),
        "2026-09-28T00-00-00-000Z_native-synthetic.jsonl",
        &bytes,
    );
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    assert!(
        matches!(
            err,
            TranscriptError::SourceMalformed {
                reason: "session_id_mismatch",
                ..
            }
        ),
        "a header naming another session must fail: {err:?}"
    );
}

#[tokio::test]
async fn a5_pi_duplicate_id_files() {
    // the exact path selects a's file even though b advertises the same
    // header id; a bare id pointer is refused kind_not_path — no glob.
    let a = samples().join("pi-duplicate-a.jsonl");
    let src = resolve(&pointer("pi", a.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    assert_eq!(src.path(), a.as_path(), "exact path is the source");
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 2, "header plus one record");
    assert_eq!(
        w.events.get(1).and_then(|e| e.text.as_deref()),
        Some("a"),
        "a's record text, not the duplicate's"
    );
    let err = resolve(&pointer("pi", "native-synthetic", None), &no_roots())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            TranscriptError::SessionPointerInvalid {
                reason: "kind_not_path"
            }
        ),
        "an id-only pointer is refused: {err:?}"
    );
}

#[tokio::test]
async fn a5_pi_permission_denied() {
    if privileged_runner() {
        return; // DAC override reads a 000-mode file — mechanism n/a
    }
    let dir = tempdir().unwrap();
    let bytes = std::fs::read(samples().join("pi-duplicate-a.jsonl")).unwrap();
    let path = stage(dir.path(), "t_native-synthetic.jsonl", &bytes);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "EACCES" }),
        "permission denied is a typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_pi_vanish_before_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gone_native-synthetic.jsonl");
    let err = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "ENOENT" }),
        "a vanished path is typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_pi_vanish_after_open_before_read() {
    // the open fd pins the inode: removing the path between resolve and
    // read changes nothing — the bytes prove the opened snapshot.
    let dir = tempdir().unwrap();
    let bytes = std::fs::read(samples().join("pi-duplicate-a.jsonl")).unwrap();
    let path = stage(dir.path(), "v_native-synthetic.jsonl", &bytes);
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 2, "the pinned inode still reads");
}

#[tokio::test]
async fn a5_pi_oversized_scan() {
    // a record bigger than the window is skipped with a resume cursor —
    // the gap is exposed, not silent. A record spanning the whole 16 MiB
    // scan budget is a typed source_exceeds_budget instead.
    let dir = tempdir().unwrap();
    let big = format!(
        "{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}\n",
        "x".repeat(65_536)
    );
    let small = "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"cwd\":\"/x\"}\n";
    let body = format!("{big}{small}");
    let path = stage(dir.path(), "big_x.jsonl", body.as_bytes());
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    let TranscriptError::RecordExceedsBudget {
        offset,
        bytes,
        resume,
    } = err
    else {
        panic!("oversized record must be record_exceeds_budget: {err:?}");
    };
    assert_eq!(offset, 0, "the oversized record starts at the cursor");
    assert!(bytes > 32_768, "the record is bigger than the window");
    assert_eq!(
        resume.position,
        offset.saturating_add(bytes),
        "resume lands past the skipped record"
    );

    let huge = stage(dir.path(), "huge_x.jsonl", &vec![b'x'; 17_000_000]);
    let huge_src = resolve(&pointer("pi", huge.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let huge_err = read_window(&huge_src, Cursor::START).await.unwrap_err();
    assert!(
        matches!(huge_err, TranscriptError::SourceExceedsBudget { .. }),
        "a record spanning the scan budget is a typed failure: {huge_err:?}"
    );
}

#[tokio::test]
async fn a5_pi_after_skipped_record() {
    // resuming from the skip cursor reads the record after the gap.
    let dir = tempdir().unwrap();
    let big = format!(
        "{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}\n",
        "x".repeat(65_536)
    );
    let small = "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"cwd\":\"/x\"}\n";
    let bytes = format!("{big}{small}");
    let path = stage(dir.path(), "skip_x.jsonl", bytes.as_bytes());
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    let resume = err.resume_cursor().unwrap();
    let w = read_window(&src, resume).await.unwrap();
    assert_eq!(w.events.len(), 1, "the record after the gap emits");
    assert_eq!(
        w.cursor.position,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        "the cursor is at end of file"
    );
}
