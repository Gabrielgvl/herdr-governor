//! a5 devin legs — the whole-document ATIF reader against the committed
//! samples (`a5-probe-outcomes.json` case ids, one test each).

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use tempfile::tempdir;

use super::{pointer, privileged_runner, samples, stage};
use herdr_governor::adapters::transcript::{
    Cursor, TranscriptError, TranscriptRoots, read_window, resolve,
};

/// Roots pointing at `dirs` as the document search space.
fn doc_roots(dirs: Vec<PathBuf>) -> TranscriptRoots {
    TranscriptRoots::new(dirs, Vec::new())
}

#[tokio::test]
async fn a5_devin_partial_document() {
    // a truncated document is retryable invalid_json; the completed
    // document then reads cleanly — the failure consumed nothing.
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    let truncated = std::fs::read(samples().join("devin-session-truncated.json")).unwrap();
    let path = stage(&root, "native-synthetic.json", &truncated);
    let roots = doc_roots(vec![root.clone()]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    assert!(
        matches!(
            err,
            TranscriptError::SourceMalformed {
                reason: "invalid_json",
                ..
            }
        ),
        "a partial document is retryable invalid_json: {err:?}"
    );

    let full = std::fs::read(samples().join("devin-session.json")).unwrap();
    std::fs::write(&path, &full).unwrap();
    // the rewritten file keeps the inode's path; resolve again so the
    // completed document is what's opened
    let src2 = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let w2 = read_window(&src2, Cursor::START).await.unwrap();
    assert_eq!(w2.events.len(), 1, "one step emits");
    assert_eq!(w2.byte_count, 44, "the step's serialized bytes");
    assert_eq!(w2.cursor.position, 1, "one step consumed");
}

#[tokio::test]
async fn a5_devin_identity_guards() {
    // content session_id must match the pointer id; an unsafe id is
    // refused before any path is touched.
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    let foreign = std::fs::read(samples().join("devin-foreign-id.json")).unwrap();
    stage(&root, "native-synthetic.json", &foreign);
    let roots = doc_roots(vec![root]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
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
        "a document naming another session must fail: {err:?}"
    );

    let err2 = resolve(&pointer("devin", "../escape", None), &roots)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err2,
            TranscriptError::SessionPointerInvalid {
                reason: "id_not_filename_safe"
            }
        ),
        "a traversal id is refused: {err2:?}"
    );
}

#[tokio::test]
async fn a5_devin_duplicate_id_files() {
    // the same id under two roots resolves by root order — a wins.
    let roots = doc_roots(vec![
        samples().join("devin-duplicate-a"),
        samples().join("devin-duplicate-b"),
    ]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 1, "one step from the chosen root");
    assert_eq!(
        w.events.first().and_then(|e| e.text.as_deref()),
        Some("a"),
        "root order selects a's document"
    );
}

#[tokio::test]
async fn a5_devin_xdg_resolution() {
    // an XDG-style root resolves `<root>/<id>.json`.
    let roots = doc_roots(vec![samples().join("devin-xdg/devin/cli/transcripts")]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 1, "the XDG-rooted document reads");
}

#[tokio::test]
async fn a5_devin_permission_denied() {
    if privileged_runner() {
        return; // DAC override reads a 000-mode file — mechanism n/a
    }
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    let doc = std::fs::read(samples().join("devin-session.json")).unwrap();
    let path = stage(&root, "native-synthetic.json", &doc);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = resolve(
        &pointer("devin", "native-synthetic", None),
        &doc_roots(vec![root]),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "EACCES" }),
        "permission denied is a typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_devin_vanish_before_open() {
    let dir = tempdir().unwrap();
    let roots = doc_roots(vec![dir.path().join("empty")]);
    let err = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceUnreadable { code: "ENOENT" }),
        "a vanished source is typed unreadable: {err:?}"
    );
}

#[tokio::test]
async fn a5_devin_vanish_after_open_before_read() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    let doc = std::fs::read(samples().join("devin-session.json")).unwrap();
    let path = stage(&root, "native-synthetic.json", &doc);
    let roots = doc_roots(vec![root]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 1, "the pinned inode still reads");
}

#[tokio::test]
async fn a5_devin_oversized_step() {
    // a step bigger than the window is skipped with a resume cursor; the
    // next read emits the following step — the gap is exposed.
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    let doc = format!(
        "{{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"native-synthetic\",\"steps\":[{{\"step_id\":1,\"source\":\"agent\",\"message\":\"{}\"}},{{\"step_id\":2,\"source\":\"agent\",\"message\":\"b\"}}]}}",
        "x".repeat(40_000)
    );
    stage(&root, "native-synthetic.json", doc.as_bytes());
    let roots = doc_roots(vec![root]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    let TranscriptError::RecordExceedsBudget { offset, resume, .. } = err else {
        panic!("oversized step must be record_exceeds_budget: {err:?}");
    };
    assert_eq!(offset, 1, "the first step is the oversized one");
    assert_eq!(resume.position, 1, "resume consumes the skipped step");

    let w = read_window(&src, resume).await.unwrap();
    assert_eq!(w.events.len(), 1, "the step after the gap emits");
    assert_eq!(w.cursor.position, 2, "both steps consumed");
    assert_eq!(
        w.events.first().and_then(|e| e.text.as_deref()),
        Some("b"),
        "the surviving step's text"
    );
}

#[tokio::test]
async fn a5_devin_source_ceiling() {
    // a document over the 8 MiB source ceiling is refused before reading.
    let dir = tempdir().unwrap();
    let root = dir.path().join("transcripts");
    stage(&root, "native-synthetic.json", &vec![b' '; 8_500_000]);
    let roots = doc_roots(vec![root]);
    let src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let err = read_window(&src, Cursor::START).await.unwrap_err();
    let TranscriptError::SourceExceedsBudget {
        bytes_at_least,
        budget,
    } = err
    else {
        panic!("an oversized source is source_exceeds_budget: {err:?}");
    };
    assert_eq!(budget, 8_388_608, "the pinned 8 MiB ceiling");
    assert!(bytes_at_least > 8_388_608, "the file really exceeds it");
}
