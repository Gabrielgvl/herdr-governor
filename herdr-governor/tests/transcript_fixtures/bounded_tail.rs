//! `BoundedTail` (§4.9 [r3] → F40) — the one trim rule both readers and
//! the daemon's evidence tail share: an accumulated tail fed window by
//! window equals the tail one read from `Cursor::START` emits, for the
//! line format and the document format alike, and the trim matches a
//! hand-computed suffix.

use tempfile::tempdir;

use super::{no_roots, pointer, stage};
use herdr_governor::adapters::transcript::{
    BoundedTail, Cursor, EventKind, TranscriptEvent, TranscriptRoots, read_window, resolve,
};

/// `window_is_deterministic_32k_tail`'s record — 162 bytes with its LF.
const LINE: &str = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}]}}\n";

fn event(text: &str, source_bytes: u64) -> TranscriptEvent {
    TranscriptEvent {
        timestamp: None,
        role: None,
        kind: EventKind::Message,
        text: Some(text.to_owned()),
        source_bytes,
    }
}

fn texts(tail: &BoundedTail) -> Vec<String> {
    tail.events()
        .iter()
        .filter_map(|e| e.text.clone())
        .collect()
}

#[test]
fn bounded_tail_trim_matches_a_hand_computed_suffix() {
    // 10 KiB records: three fit (30 KiB), a fourth evicts the oldest; a
    // record alone over the window still stays (the newest is kept).
    let mut tail = BoundedTail::new();
    for name in ["a", "b", "c", "d"] {
        tail.push(event(name, 10 * 1024));
    }
    assert_eq!(texts(&tail), ["b", "c", "d"], "the oldest is evicted first");
    assert_eq!(tail.bytes(), 30 * 1024, "bytes track the retained records");
    tail.push(event("huge", 40 * 1024));
    assert_eq!(texts(&tail), ["huge"], "an over-window record stands alone");
}

#[tokio::test]
async fn bounded_tail_trims_by_source_bytes_like_both_readers() {
    line_format_accumulates_to_the_rebuilt_tail().await;
    document_format_accumulates_to_the_rebuilt_tail().await;
}

/// 300 records read in two windows (100, then 200 appended) and
/// accumulated equal one read of all 300 from `START`.
async fn line_format_accumulates_to_the_rebuilt_tail() {
    let dir = tempdir().unwrap();
    let path = stage(dir.path(), "acc_x.jsonl", LINE.repeat(100).as_bytes());
    let locator = path.to_str().unwrap().to_owned();
    let src = resolve(&pointer("pi", &locator, None), &no_roots())
        .await
        .unwrap();
    let first = read_window(&src, Cursor::START).await.unwrap();
    let mut tail = BoundedTail::new();
    tail.extend(first.events);
    std::fs::write(&path, LINE.repeat(300)).unwrap();
    let second = read_window(&src, first.cursor).await.unwrap();
    tail.extend(second.events);

    let rebuilt = resolve(&pointer("pi", &locator, None), &no_roots())
        .await
        .unwrap();
    let whole = read_window(&rebuilt, Cursor::START).await.unwrap();
    assert_eq!(
        second.cursor, whole.cursor,
        "both paths consumed everything"
    );
    assert_eq!(tail.bytes(), whole.byte_count, "same source bytes retained");
    assert_eq!(
        tail.into_events(),
        whole.events,
        "the accumulated tail is the rebuilt tail"
    );
}

/// An ATIF document of `count` steps, 200-byte messages each.
fn document(count: usize) -> String {
    let steps: Vec<String> = (1..=count)
        .map(|i| {
            format!(
                "{{\"step_id\":{i},\"source\":\"agent\",\"message\":\"{}\"}}",
                "m".repeat(200)
            )
        })
        .collect();
    format!(
        "{{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"doc-1\",\"steps\":[{}]}}",
        steps.join(",")
    )
}

/// A 300-step document read as 120 steps, then whole — accumulated
/// equals one read from `START`.
async fn document_format_accumulates_to_the_rebuilt_tail() {
    let dir = tempdir().unwrap();
    let roots = TranscriptRoots::new(vec![dir.path().to_path_buf()], Vec::new());
    std::fs::write(dir.path().join("doc-1.json"), document(120)).unwrap();
    let early = resolve(&pointer("devin", "doc-1", None), &roots)
        .await
        .unwrap();
    let first = read_window(&early, Cursor::START).await.unwrap();
    let mut tail = BoundedTail::new();
    tail.extend(first.events);
    drop(early);
    std::fs::write(dir.path().join("doc-1.json"), document(300)).unwrap();
    let late = resolve(&pointer("devin", "doc-1", None), &roots)
        .await
        .unwrap();
    let second = read_window(&late, first.cursor).await.unwrap();
    tail.extend(second.events);
    let whole = read_window(&late, Cursor::START).await.unwrap();
    assert_eq!(
        second.cursor, whole.cursor,
        "both paths consumed every step"
    );
    assert!(
        whole.byte_count <= 32 * 1024,
        "the document tail is bounded"
    );
    assert_eq!(tail.bytes(), whole.byte_count, "same source bytes retained");
    assert_eq!(
        tail.into_events(),
        whole.events,
        "the accumulated document tail is the rebuilt tail"
    );
}
