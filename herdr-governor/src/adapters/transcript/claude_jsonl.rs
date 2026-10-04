//! `claude_jsonl` — the LF-delimited session log under the project-slug
//! roots: one JSON record per line, and every record carrying a
//! `sessionId` must match the pointer's id — a foreign record means the
//! file isn't the session it was resolved as (`session_id_mismatch`;
//! `A5-CORRUPT-NOT-ABSENCE` — corruption defeats a no-progress proof).

use std::io::SeekFrom;

use governor_core::identity::Timestamp;
use serde_json::Value;
use tokio::fs::File;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use super::devin_log::{self, LimitRecord};
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

// — typed provider-limit record (§4.17/F31) ————————————————————————————

/// The tail bound for one limit scan of a session log — the reference's
/// 512 KiB (`claude-quota.ts`); a whole record is a few KiB.
const TAIL_BYTES: u64 = 512 * 1024;

/// Record fields carrying an absolute reset instant — the provider names
/// observed across harnesses; a string is a timestamp, a number an epoch
/// in s or ms.
const RESET_INSTANT_FIELDS: [&str; 6] = [
    "retryNotBefore",
    "resetAt",
    "resetsAt",
    "retryAt",
    "reset_at",
    "retry_at",
];

/// Record fields carrying a delay in seconds.
const RESET_AFTER_SECONDS_FIELDS: [&str; 4] = [
    "retryAfterSeconds",
    "retry_after_seconds",
    "retryAfter",
    "retry_after",
];

/// Record fields carrying a delay in milliseconds.
const RESET_AFTER_MS_FIELDS: [&str; 2] = ["retryAfterMs", "retry_after_ms"];

/// `<1e12` is epoch seconds, `>=1e12` epoch ms — the same cutover the
/// reference applies; ms today are ~1.7e12.
fn epochish_ms(value: i64) -> i64 {
    if value.abs() < 1_000_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

/// A record field as an epoch-ms instant: JSON number (s|ms) or string
/// (strict timestamp).
fn instant_field_ms(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().map(epochish_ms),
        Value::String(text) if !text.is_empty() => devin_log::rfc3339_ms(text),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) | Value::String(_) => {
            None
        }
    }
}

/// ASCII-insensitive `text.find(needle)` — byte-exact offsets.
fn find_ci(text: &str, needle: &str) -> Option<usize> {
    text.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `limit reached|<epoch>` — the capture reads 10–13 digits; a longer run
/// keeps its first 13 (the greedy `\d{10,13}`).
fn limit_reached_ms(text: &str) -> Option<i64> {
    let mut from = 0_usize;
    while let Some(pos) =
        find_ci(text.get(from..)?, "limit reached|").map(|hit| from.saturating_add(hit))
    {
        let after = text.get(pos.saturating_add(14)..).unwrap_or("");
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits >= 10 {
            let take = digits.min(13);
            if let Some(ms) = after
                .get(..take)
                .and_then(|s| s.parse::<i64>().ok())
                .map(epochish_ms)
            {
                return Some(ms);
            }
        }
        from = pos.saturating_add(1);
    }
    None
}

/// The record's `message` text — bare string, or `content` blocks (each a
/// string or `{type,text}`).
fn message_texts(record: &Value) -> Vec<&str> {
    match record.get("message") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Object(message)) => message
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| match block {
                        Value::String(s) => Some(s.as_str()),
                        Value::Object(b) => b.get("text").and_then(Value::as_str),
                        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some(_) | None => Vec::new(),
    }
}

/// The record's stated reset instant (`resetSignalMs`): typed instant
/// fields (epoch s|ms or timestamp string, must be after the record), then
/// typed delays, then `limit reached|<epoch>` in the message text.
fn reset_signal_ms(record: &Value, record_ms: i64) -> Option<i64> {
    for field in RESET_INSTANT_FIELDS {
        if let Some(ms) = instant_field_ms(record.get(field))
            && ms > record_ms
        {
            return Some(ms);
        }
    }
    for field in RESET_AFTER_SECONDS_FIELDS {
        if let Some(delay) = record.get(field).and_then(Value::as_i64)
            && delay > 0
        {
            return Some(record_ms.saturating_add(delay.saturating_mul(1_000)));
        }
    }
    for field in RESET_AFTER_MS_FIELDS {
        if let Some(delay) = record.get(field).and_then(Value::as_i64)
            && delay > 0
        {
            return Some(record_ms.saturating_add(delay));
        }
    }
    message_texts(record)
        .iter()
        .find_map(|text| limit_reached_ms(text).filter(|ms| *ms > record_ms))
}

/// The session id shape the provider writes — the reference's
/// `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}` (ci).
fn session_id_ok(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// The session log's tail scanned for the typed 429 record: the newest
/// `assistant`/`isApiErrorMessage`/`error == "rate_limit"`/
/// `apiErrorStatus == 429` record with a non-empty `requestId` bound to
/// this session (`sessionId` and `cwd` both equal) at or after
/// `cycle_start` — anything else is no record. Without the session cwd
/// the reference proves nothing, so the probe answers `None`.
pub(super) async fn limit_record(
    source: &ResolvedSource,
    session: &str,
    cwd: Option<&str>,
    cycle_start: Timestamp,
) -> Option<LimitRecord> {
    let want_cwd = cwd?;
    if !session_id_ok(session) {
        return None;
    }
    let (start, buf) = {
        let mut guard = source.file().lock().await;
        let file = &mut *guard;
        let size = file.metadata().await.ok()?.len();
        let start = size.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(start)).await.ok()?;
        let mut buf = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut buf).await.ok()?;
        drop(guard);
        (start, buf)
    };
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    // a partial last line (in-flight write) is dropped — the tail is read
    // whole-lines only
    lines.pop();
    let mut record = None;
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !value.is_object()
            || value.get("sessionId").and_then(Value::as_str) != Some(session)
            || value.get("cwd").and_then(Value::as_str) != Some(want_cwd)
        {
            continue;
        }
        let Some(record_ms) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(devin_log::rfc3339_ms)
        else {
            continue;
        };
        if record_ms < cycle_start.0 {
            continue;
        }
        let limited = value.get("type").and_then(Value::as_str) == Some("assistant")
            && value.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
            && value.get("error").and_then(Value::as_str) == Some("rate_limit")
            && value.get("apiErrorStatus").and_then(Value::as_i64) == Some(429)
            && value
                .get("requestId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty());
        if limited {
            record = Some(LimitRecord {
                source: "claude_session_quota",
                observed_at: Timestamp(record_ms),
                reset_at: reset_signal_ms(&value, record_ms).map(Timestamp),
            });
        }
    }
    record
}
