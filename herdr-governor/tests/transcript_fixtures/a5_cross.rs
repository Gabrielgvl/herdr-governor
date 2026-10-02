//! a5 cross-reader legs — cursor rewrites, symlink policy differences,
//! and the selection/fallback contract (`a5-probe-outcomes.json` ids).

use std::os::unix::fs::symlink;

use tempfile::tempdir;

use super::{no_roots, pointer, samples, stage};
use herdr_governor::adapters::transcript::{
    Cursor, TranscriptError, TranscriptRoots, read_window, resolve,
};

#[tokio::test]
async fn a5_cursor_rewrites() {
    // bytes under the cursor changed → source_rewritten; for the document
    // source the reason is anchor_mismatch.
    let dir = tempdir().unwrap();
    let a = std::fs::read(samples().join("pi-duplicate-a.jsonl")).unwrap();
    let b = std::fs::read(samples().join("pi-duplicate-b.jsonl")).unwrap();
    assert_eq!(a.len(), b.len(), "the samples differ only in content");
    let path = stage(dir.path(), "t_native-synthetic.jsonl", &a);
    let src = resolve(&pointer("pi", path.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    std::fs::write(&path, &b).unwrap(); // same inode, same length, new bytes
    let err = read_window(&src, w.cursor).await.unwrap_err();
    assert!(
        matches!(err, TranscriptError::SourceRewritten { .. }),
        "a rewritten cursor region is detected: {err:?}"
    );

    let root = dir.path().join("transcripts");
    let doc_a = "{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"native-synthetic\",\"steps\":[{\"step_id\":1,\"source\":\"agent\",\"message\":\"a\"}]}";
    stage(&root, "native-synthetic.json", doc_a.as_bytes());
    let roots = TranscriptRoots::new(vec![root], Vec::new());
    let doc_src = resolve(&pointer("devin", "native-synthetic", None), &roots)
        .await
        .unwrap();
    let doc_w = read_window(&doc_src, Cursor::START).await.unwrap();
    let doc_b = "{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"native-synthetic\",\"steps\":[{\"step_id\":1,\"source\":\"agent\",\"message\":\"b\"}]}";
    std::fs::write(doc_src.path(), doc_b.as_bytes()).unwrap();
    let doc_err = read_window(&doc_src, doc_w.cursor).await.unwrap_err();
    assert!(
        matches!(
            doc_err,
            TranscriptError::SourceRewritten {
                reason: "anchor_mismatch"
            }
        ),
        "a rewritten document is anchor_mismatch: {doc_err:?}"
    );
}

#[tokio::test]
async fn a5_symlink_behavior() {
    // symlink policy differs per reader: two follow, the slugged reader
    // refuses outright.
    let dir = tempdir().unwrap();
    let target = stage(
        dir.path(),
        "real_native-synthetic.jsonl",
        &std::fs::read(samples().join("pi-duplicate-a.jsonl")).unwrap(),
    );
    let link = dir.path().join("link_native-synthetic.jsonl");
    symlink(&target, &link).unwrap();

    let src = resolve(&pointer("pi", link.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert_eq!(w.events.len(), 2, "the path reader follows a symlink");

    let root = dir.path().join("transcripts");
    std::fs::create_dir_all(&root).unwrap();
    symlink(
        samples().join("devin-foreign-id.json"),
        root.join("native-synthetic.json"),
    )
    .unwrap();
    let doc_roots = TranscriptRoots::new(vec![root], Vec::new());
    let doc_src = resolve(&pointer("devin", "native-synthetic", None), &doc_roots)
        .await
        .unwrap();
    let doc_err = read_window(&doc_src, Cursor::START).await.unwrap_err();
    assert!(
        matches!(
            doc_err,
            TranscriptError::SourceMalformed {
                reason: "session_id_mismatch",
                ..
            }
        ),
        "followed, then the content id rejects: {doc_err:?}"
    );

    let proot = dir.path().join("projects");
    let real = stage(
        dir.path(),
        "elsewhere/u.jsonl",
        &std::fs::read(samples().join("claude-quota.jsonl")).unwrap(),
    );
    std::fs::create_dir_all(proot.join("-synthetic-project")).unwrap();
    symlink(&real, proot.join("-synthetic-project/u.jsonl")).unwrap();
    let proj_roots = TranscriptRoots::new(Vec::new(), vec![proot]);
    let proj_err = resolve(&pointer("claude", "u", None), &proj_roots)
        .await
        .unwrap_err();
    assert!(
        matches!(
            proj_err,
            TranscriptError::SourceUnreadable {
                code: "symlink_refused"
            }
        ),
        "the slugged reader refuses a symlink: {proj_err:?}"
    );
}

#[tokio::test]
async fn a5_source_selection_and_fallback() {
    // structured-source failure is typed, never silently terminal: the
    // adapter reports unreadable and the caller decides on fallback —
    // `structured_failure_terminal_calls` was 0 in the reference too.
    let dir = tempdir().unwrap();
    let missing = dir.path().join("nope_x.jsonl");
    let path_err = resolve(&pointer("pi", missing.to_str().unwrap(), None), &no_roots())
        .await
        .unwrap_err();
    assert!(
        matches!(
            path_err,
            TranscriptError::SourceUnreadable { code: "ENOENT" }
        ),
        "path pointer, absent file: {path_err:?}"
    );

    let doc_roots = TranscriptRoots::new(vec![dir.path().join("empty")], Vec::new());
    let doc_err = resolve(&pointer("devin", "native-synthetic", None), &doc_roots)
        .await
        .unwrap_err();
    assert!(
        matches!(
            doc_err,
            TranscriptError::SourceUnreadable { code: "ENOENT" }
        ),
        "document pointer, absent file: {doc_err:?}"
    );

    let proj_roots = TranscriptRoots::new(Vec::new(), vec![dir.path().join("projects")]);
    let proj_err = resolve(&pointer("claude", "u", None), &proj_roots)
        .await
        .unwrap_err();
    assert!(
        matches!(
            proj_err,
            TranscriptError::SourceUnreadable { code: "ENOENT" }
        ),
        "slugged pointer, no candidates: {proj_err:?}"
    );

    for kind in ["agy", "unknown"] {
        let kind_err = resolve(&pointer(kind, "anything", None), &no_roots())
            .await
            .unwrap_err();
        assert!(
            matches!(
                kind_err,
                TranscriptError::Unreadable {
                    reason: "no_transcript_source"
                }
            ),
            "an unqualified kind is terminal-fallback unreadable: {kind_err:?}"
        );
    }
}
