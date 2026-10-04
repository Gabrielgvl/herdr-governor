//! `claude_jsonl` — the LF-delimited session log under the project-slug
//! roots: one JSON record per line, and every record carrying a
//! `sessionId` must match the pointer's id — a foreign record means the
//! file isn't the session it was resolved as (`session_id_mismatch`;
//! `A5-CORRUPT-NOT-ABSENCE` — corruption defeats a no-progress proof).

use serde_json::Value;
use tokio::fs::File;

use super::error::TranscriptError;
use super::pointer::ResolvedSource;
use super::window::{Cursor, EventKind, TranscriptEvent, Window, scan_jsonl, text_field};

/// The LF scan with this format's rules: no header record, per-record
/// `sessionId` identity.
pub(super) async fn read(
    file: &mut File,
    source: &ResolvedSource,
    cursor: Cursor,
) -> Result<Window, TranscriptError> {
    scan_jsonl(
        file,
        cursor,
        source.expected_id(),
        false,
        Some("sessionId"),
        normalize,
    )
    .await
}

/// A record → its supervision event: `user`/`assistant` records carry
/// role and content (`tool_use`/`tool_result` blocks are the tool
/// markers); an `isApiErrorMessage` record is an `Error` carrying its
/// error name (e.g. a rate-limit/quota record); a prompt the user
/// submitted is `UserTurn`; the rest are `Meta`.
fn normalize(record: &Value) -> TranscriptEvent {
    let timestamp = text_field(record, "timestamp");
    let ty = record.get("type").and_then(Value::as_str);
    if record.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
        return TranscriptEvent {
            timestamp,
            role: ty.map(str::to_owned),
            kind: EventKind::Error,
            text: text_field(record, "error"),
            source_bytes: 0,
        };
    }
    match ty {
        Some("user" | "assistant") => {
            let content = record.get("message").and_then(|m| m.get("content"));
            let (text, kind) = content_summary(content);
            TranscriptEvent {
                timestamp,
                role: ty.map(str::to_owned),
                kind: if ty == Some("user") && is_user_turn(record) {
                    EventKind::UserTurn
                } else {
                    kind
                },
                text,
                source_bytes: 0,
            }
        }
        Some(_) | None => TranscriptEvent {
            timestamp,
            role: None,
            kind: EventKind::Meta,
            text: None,
            source_bytes: 0,
        },
    }
}

/// `trace-tail.ts:137-149` — a `type:user` record is the user's prompt
/// only when `message.role` is `user`, it is not tool output (no
/// `toolUseResult`, no `tool_result` block) or runtime context
/// (`isMeta`/`isCompactSummary`/`isSidechain`), and it carries text (a
/// non-empty string or a `text` block).
fn is_user_turn(record: &Value) -> bool {
    let Some(message) = record.get("message") else {
        return false;
    };
    if message.get("role").and_then(Value::as_str) != Some("user")
        || record.get("toolUseResult").is_some()
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return false;
    }
    match message.get("content") {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(blocks)) => {
            let mut has_text = false;
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_result") => return false,
                    Some("text") => has_text = true,
                    Some(_) | None => {}
                }
            }
            has_text
        }
        Some(_) | None => false,
    }
}

/// `message.content` may be a bare string or a block array — collect the
/// `text` blocks and the tool markers.
fn content_summary(content: Option<&Value>) -> (Option<String>, EventKind) {
    let mut text = String::new();
    let mut kind = EventKind::Message;
    match content {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    Some("tool_use") => kind = EventKind::ToolCall,
                    Some("tool_result") => {
                        if kind == EventKind::Message {
                            kind = EventKind::ToolResult;
                        }
                    }
                    Some(_) | None => {}
                }
            }
        }
        Some(_) | None => {}
    }
    (if text.is_empty() { None } else { Some(text) }, kind)
}
