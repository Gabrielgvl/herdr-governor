//! `pi_jsonl` — the LF-delimited session log: one JSON record per line,
//! LF is the boundary (a complete record without its LF stays pending),
//! and the first line is the session header — its `id` must prove the
//! filename-derived id (`A5-PI-HEADER`: the governor validates the
//! identity the reference reader skipped, `a5_header_identity_not_validated`).

use serde_json::Value;
use tokio::fs::File;

use super::error::TranscriptError;
use super::pointer::ResolvedSource;
use super::window::{Cursor, EventKind, TranscriptEvent, Window, scan_jsonl, text_field};

/// The LF scan with this format's rules: header identity at offset 0, no
/// per-record id field.
pub(super) async fn read(
    file: &mut File,
    source: &ResolvedSource,
    cursor: Cursor,
) -> Result<Window, TranscriptError> {
    scan_jsonl(file, cursor, source.expected_id(), true, None, normalize).await
}

/// A record → its supervision event: `session` records mark the header,
/// `message` records carry role and content blocks (text and tool
/// markers), a `custom_message` a hook-free user turn, `compaction`
/// unplaceable, everything else `Meta` (`trace-tail.ts:151-163`).
fn normalize(record: &Value) -> TranscriptEvent {
    let timestamp = text_field(record, "timestamp");
    let meta = |at| TranscriptEvent {
        timestamp: at,
        role: None,
        kind: EventKind::Meta,
        text: None,
        source_bytes: 0,
    };
    match record.get("type").and_then(Value::as_str) {
        Some("session") => TranscriptEvent {
            timestamp,
            role: None,
            kind: EventKind::Session,
            text: None,
            source_bytes: 0,
        },
        Some("message") => message_event(record, timestamp),
        Some("custom_message") => {
            let from_hook = record.get("fromHook").and_then(Value::as_bool) == Some(true)
                || record
                    .get("message")
                    .and_then(|m| m.get("fromHook"))
                    .and_then(Value::as_bool)
                    == Some(true);
            TranscriptEvent {
                kind: if from_hook {
                    EventKind::Meta
                } else {
                    EventKind::UserTurn
                },
                ..meta(timestamp)
            }
        }
        Some("compaction") => TranscriptEvent {
            kind: EventKind::Ambiguous,
            ..meta(timestamp)
        },
        Some(_) | None => meta(timestamp),
    }
}

/// A `message` record → role + concatenated text blocks + the tool
/// markers its content carries.
fn message_event(record: &Value, timestamp: Option<String>) -> TranscriptEvent {
    let msg = record.get("message");
    let role = msg
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut text = String::new();
    let mut has_tool_call = false;
    if let Some(blocks) = msg.and_then(|m| m.get("content")).and_then(Value::as_array) {
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
                Some("toolCall") => has_tool_call = true,
                Some(_) | None => {}
            }
        }
    }
    TranscriptEvent {
        timestamp: timestamp.or_else(|| msg.and_then(|m| text_field(m, "timestamp"))),
        role: role.clone(),
        kind: if role.as_deref() == Some("user") {
            EventKind::UserTurn
        } else if role.as_deref() == Some("toolResult") {
            EventKind::ToolResult
        } else if has_tool_call {
            EventKind::ToolCall
        } else {
            EventKind::Message
        },
        text: if text.is_empty() { None } else { Some(text) },
        source_bytes: 0,
    }
}
