//! `devin_atif` — the whole-document ATIF transcript: one JSON document
//! of `steps`, a `session_id` that must match the pointer, an 8 MiB
//! source ceiling, and a cursor in consumed-step units with a content
//! anchor — a rewritten document is `anchor_mismatch`, not new steps
//! (`a5_cursor_rewrite_detected`). A partial document is `invalid_json`
//! and retryable (`a5_devin_partial_document_retryable`).

use std::collections::VecDeque;
use std::io::SeekFrom;

use serde_json::Value;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _};

use super::error::{TranscriptError, unreadable};
use super::pointer::ResolvedSource;
use super::window::{
    Cursor, EventKind, TranscriptEvent, WINDOW_MAX_BYTES, Window, fnv1a, text_field,
};

/// A5 — the per-source ceiling (`DEVIN_SOURCE_MAX_BYTES`); the document
/// is parsed whole, so reading past it is refused before any read.
const SOURCE_MAX_BYTES: u64 = 8 * 1024 * 1024;

fn malformed_json() -> TranscriptError {
    TranscriptError::SourceMalformed {
        offset: 0,
        reason: "invalid_json",
    }
}

/// Read the window of steps after `cursor`. The document is reparsed in
/// full (≤ 8 MiB); the cursor anchor re-proves the consumed region.
pub(super) async fn read(
    file: &mut File,
    source: &ResolvedSource,
    cursor: Cursor,
) -> Result<Window, TranscriptError> {
    let len = file.metadata().await.map_err(|e| unreadable(&e))?.len();
    if len > SOURCE_MAX_BYTES {
        return Err(TranscriptError::SourceExceedsBudget {
            bytes_at_least: len,
            budget: SOURCE_MAX_BYTES,
        });
    }
    file.seek(SeekFrom::Start(0))
        .await
        .map_err(|e| unreadable(&e))?;
    let buf = read_bounded(file).await?;
    let doc: Value = serde_json::from_slice(&buf).map_err(|_json| malformed_json())?;
    let session_id = doc.get("session_id").and_then(Value::as_str);
    if session_id != source.expected_id() {
        return Err(TranscriptError::SourceMalformed {
            offset: 0,
            reason: "session_id_mismatch",
        });
    }
    let Some(steps) = doc.get("steps").and_then(Value::as_array) else {
        return Err(malformed_json());
    };
    let pos = usize::try_from(cursor.position).unwrap_or(usize::MAX);
    if pos > steps.len() || (pos > 0 && !anchor_holds(steps, pos, session_id, cursor)?) {
        return Err(TranscriptError::SourceRewritten {
            reason: "anchor_mismatch",
        });
    }
    emit_tail(steps, pos, session_id, cursor)
}

/// Read the whole source, but never more than the ceiling: the metadata
/// check above is advisory (the file may grow or be rewritten between it
/// and the read), so the read itself is what enforces the budget.
pub(super) async fn read_bounded(
    reader: impl AsyncRead + Unpin,
) -> Result<Vec<u8>, TranscriptError> {
    let mut buf = Vec::new();
    reader
        .take(SOURCE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut buf)
        .await
        .map_err(|e| unreadable(&e))?;
    let read = u64::try_from(buf.len()).unwrap_or(u64::MAX);
    if read > SOURCE_MAX_BYTES {
        return Err(TranscriptError::SourceExceedsBudget {
            bytes_at_least: read,
            budget: SOURCE_MAX_BYTES,
        });
    }
    Ok(buf)
}

/// The content anchor: FNV-1a over the session id and the last consumed
/// step's canonical serialization — the rewrite the probe recorded as
/// `anchor_mismatch`.
fn anchor(session_id: Option<&str>, step: &Value) -> Result<u64, TranscriptError> {
    let ser = serde_json::to_vec(step).map_err(|_json| malformed_json())?;
    let mut bytes = session_id.unwrap_or("").as_bytes().to_vec();
    bytes.extend_from_slice(&ser);
    Ok(fnv1a(&bytes))
}

/// Re-prove the consumed region: `steps[pos - 1]` must still hash to the
/// issued anchor.
fn anchor_holds(
    steps: &[Value],
    pos: usize,
    session_id: Option<&str>,
    cursor: Cursor,
) -> Result<bool, TranscriptError> {
    match steps.get(pos.saturating_sub(1)) {
        Some(prev) => Ok(anchor(session_id, prev)? == cursor.anchor),
        None => Ok(false),
    }
}

/// Consume steps from `pos`, keep the ≤ window tail, and skip — with a
/// resume cursor — a single oversized step (`record_exceeds_budget`).
fn emit_tail(
    steps: &[Value],
    pos: usize,
    session_id: Option<&str>,
    cursor: Cursor,
) -> Result<Window, TranscriptError> {
    let mut emitted: VecDeque<(TranscriptEvent, u64)> = VecDeque::new();
    let mut emitted_bytes = 0_u64;
    let mut consumed = pos;
    for (i, step) in steps.iter().enumerate().skip(pos) {
        let ser = serde_json::to_vec(step).map_err(|_json| malformed_json())?;
        let bytes = u64::try_from(ser.len()).unwrap_or(u64::MAX);
        if bytes > WINDOW_MAX_BYTES {
            if consumed == pos {
                return Err(TranscriptError::RecordExceedsBudget {
                    offset: u64::try_from(i.saturating_add(1)).unwrap_or(u64::MAX),
                    bytes,
                    resume: Cursor {
                        position: u64::try_from(i.saturating_add(1)).unwrap_or(u64::MAX),
                        anchor: anchor(session_id, step)?,
                    },
                });
            }
            break;
        }
        emitted.push_back((normalize_step(step), bytes));
        emitted_bytes = emitted_bytes.saturating_add(bytes);
        while emitted_bytes > WINDOW_MAX_BYTES && emitted.len() > 1 {
            if let Some((_, dropped)) = emitted.pop_front() {
                emitted_bytes = emitted_bytes.saturating_sub(dropped);
            }
        }
        consumed = i.saturating_add(1);
    }
    let anchor = if consumed == pos {
        cursor.anchor
    } else {
        match steps.get(consumed.saturating_sub(1)) {
            Some(last) => anchor(session_id, last)?,
            None => cursor.anchor,
        }
    };
    Ok(Window {
        events: emitted.into_iter().map(|(event, _)| event).collect(),
        byte_count: emitted_bytes,
        cursor: Cursor {
            position: u64::try_from(consumed).unwrap_or(u64::MAX),
            anchor,
        },
    })
}

/// A step → its supervision event: `source` is the role, `message` the
/// progress text, a non-empty `tool_calls` the tool marker.
fn normalize_step(step: &Value) -> TranscriptEvent {
    let has_tool_calls = step
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|c| !c.is_empty());
    TranscriptEvent {
        timestamp: text_field(step, "timestamp"),
        role: text_field(step, "source"),
        kind: if has_tool_calls {
            EventKind::ToolCall
        } else {
            EventKind::Message
        },
        text: text_field(step, "message"),
    }
}
