//! Window semantics — the two plan-named tests that pin the tail window
//! beyond the probe mirror.

use tempfile::tempdir;

use super::super::{Cursor, TranscriptRoots, read_window, resolve};
use super::{pointer, stage};

#[tokio::test]
async fn unterminated_tail_is_pending_not_malformed() {
    // a tail that never terminated — mid-JSON or mid-UTF-8 — is pending:
    // nothing emits, nothing fails, the cursor keeps the consumed prefix.
    let dir = tempdir().unwrap();
    let header = b"{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"cwd\":\"/x\"}\n";
    let tails: [Vec<u8>; 2] = [
        b"{\"type\":\"message\",\"message\":{\"role\":\"a".to_vec(),
        vec![0xc3],
    ];
    for (i, tail) in tails.into_iter().enumerate() {
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&tail);
        let path = stage(dir.path(), &format!("pend{i}_s.jsonl"), &bytes);
        let src = resolve(
            &pointer("pi", path.to_str().unwrap(), None),
            &TranscriptRoots::default(),
        )
        .await
        .unwrap();
        let w = read_window(&src, Cursor::START).await.unwrap();
        assert_eq!(w.events.len(), 1, "only the complete record emits");
        assert_eq!(
            w.cursor.position,
            u64::try_from(header.len()).unwrap_or(u64::MAX),
            "the cursor stops at the unterminated tail"
        );
    }
}

#[tokio::test]
async fn window_is_deterministic_32k_tail() {
    // more new records than the window holds: the emitted events are the
    // consumed region's tail — whole records only — while the cursor
    // still advances past everything consumed.
    let dir = tempdir().unwrap();
    let line = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}]}}\n";
    let mut bytes = Vec::new();
    for _ in 0..500 {
        bytes.extend_from_slice(line.as_bytes());
    }
    let path = stage(dir.path(), "full_x.jsonl", &bytes);
    let src = resolve(
        &pointer("pi", path.to_str().unwrap(), None),
        &TranscriptRoots::default(),
    )
    .await
    .unwrap();
    let w = read_window(&src, Cursor::START).await.unwrap();
    assert!(w.byte_count <= 32_768, "the window is bounded at 32 KiB");
    assert!(!w.events.is_empty(), "the tail is non-empty");
    let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
    assert_eq!(
        w.byte_count.checked_rem(line_len),
        Some(0),
        "the tail is whole records only — no torn leading record"
    );
    let emitted = u64::try_from(w.events.len()).unwrap_or(u64::MAX);
    assert_eq!(
        w.byte_count,
        emitted.saturating_mul(line_len),
        "every emitted event is one full record"
    );
    assert_eq!(
        w.cursor.position,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        "the cursor consumes past the trimmed head"
    );

    // determinism: an identical read produces an identical window
    let src2 = resolve(
        &pointer("pi", path.to_str().unwrap(), None),
        &TranscriptRoots::default(),
    )
    .await
    .unwrap();
    let w2 = read_window(&src2, Cursor::START).await.unwrap();
    assert_eq!(w2.cursor, w.cursor, "same input → same cursor");
    assert_eq!(w2.byte_count, w.byte_count, "same input → same bytes");
    assert_eq!(w2.events, w.events, "same input → same events");

    // and a follow-up read sees nothing new
    let w3 = read_window(&src, w.cursor).await.unwrap();
    assert!(w3.events.is_empty(), "no new records → empty window");
    assert_eq!(w3.cursor, w.cursor, "the cursor stands");
}
