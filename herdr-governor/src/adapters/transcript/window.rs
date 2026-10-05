//! `window` — the deterministic tail window over a resolved source (N5):
//! the scan walks forward from `cursor`, reads at most `SCAN_BUDGET_BYTES`
//! (16 MiB), consumes only complete records, and emits the ≤ 32 KiB tail
//! of the consumed region. A partial write stays pending — LF, not JSON
//! validity, is the record boundary. A cursor that no longer matches the
//! consumed region is a rewrite, not new data.

use std::collections::VecDeque;
use std::io::SeekFrom;

use serde_json::Value;
use tokio::fs::File;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use super::error::{TranscriptError, unreadable};
use super::pointer::{Format, ResolvedSource};
use super::{claude_jsonl, devin_atif, pi_jsonl};

/// N5 — the transcript evidence window: ≤ 32 KiB of source bytes, taken
/// from the tail deterministically. The file-offset (`u64`) copy of the
/// bound `governor_core::routing::TRANSCRIPT_WINDOW_MAX_BYTES` pins; the
/// assertion below fails the build if the two ever drift (`as` conversions
/// are denied, so the copy is a checked literal rather than a cast).
pub(super) const WINDOW_MAX_BYTES: u64 = 32 * 1024;
const _: () = assert!(
    governor_core::routing::TRANSCRIPT_WINDOW_MAX_BYTES == 32 * 1024,
    "WINDOW_MAX_BYTES must equal governor-core's TRANSCRIPT_WINDOW_MAX_BYTES"
);

/// N5 — at most 16 MiB is read from the source per `read_window` call.
pub(super) const SCAN_BUDGET_BYTES: u64 = 16 * 1024 * 1024;

/// Bytes sampled before the cursor for rewrite detection.
/// ponytail: the anchor is the consumed region's tail sample — a rewrite
/// strictly inside the region that preserves length and the last
/// `ANCHOR_BYTES` escapes detection; upgrade path is hashing the whole
/// consumed prefix (≤ the scan budget) if adversarial rewrites matter.
const ANCHOR_BYTES: u64 = 64;

/// Where a read stopped — opaque to callers, stable across calls.
///
/// `position` is a byte offset for the line formats and a consumed-record
/// count for the document format. `anchor` is a content sample of the
/// consumed region; when the next read finds it changed, that is a
/// `SourceRewritten`, not new data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Byte offset (line sources) or consumed-record count (document).
    pub position: u64,
    /// FNV-1a over the anchor sample; `0` marks the unconsumed start.
    pub anchor: u64,
}

impl Cursor {
    /// The unconsumed starting cursor.
    pub const START: Self = Self {
        position: 0,
        anchor: 0,
    };
}

/// What a normalized record is for supervision — evidence classes, not
/// the source's own record-type vocabulary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A session-open/header record.
    Session,
    /// Conversation content (a user/assistant/system record).
    Message,
    /// A tool invocation.
    ToolCall,
    /// A tool's result.
    ToolResult,
    /// An error record (e.g. an API/quota failure record).
    Error,
    /// Any other record the format carries.
    Meta,
    /// A prompt the user submitted — the per-kind rules of
    /// `trace-tail.ts:137-170` (`§4.9`; the F30 trace proof scans for it).
    UserTurn,
    /// A record the trace scan cannot place (a compaction) — C7 refuses
    /// `trace_ambiguous` on it in the post-anchor delta.
    Ambiguous,
}

/// One normalized record — supervision fields only (ADR-0002):
/// timestamps, roles, tool markers, progress text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEvent {
    /// The record's own timestamp, verbatim.
    pub timestamp: Option<String>,
    /// The record's role, verbatim.
    pub role: Option<String>,
    /// Which evidence class the record carries.
    pub kind: EventKind,
    /// Progress text — message/step text, or the error name.
    pub text: Option<String>,
    /// Source bytes the record spans — the reader sets it at the push
    /// site, where the serialized length is already known (normalizers
    /// leave `0`).
    pub source_bytes: u64,
}

/// The shared ≤ `WINDOW_MAX_BYTES` tail accumulator (§4.9 `[r3]` → F40):
/// one rule — append, then drop whole records from the front while the
/// tail exceeds the window — used by both readers and the daemon's
/// `EvidenceTail`, so an accumulated tail and one rebuilt from
/// `Cursor::START` are byte-identical.
#[derive(Debug, Clone, Default)]
pub struct BoundedTail {
    events: VecDeque<TranscriptEvent>,
    bytes: u64,
}

impl BoundedTail {
    /// An empty tail.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `event` (its `source_bytes` set) and trim from the front
    /// until the tail fits. The newest record always stays — an event
    /// alone exceeding the window is the caller's
    /// `record_exceeds_budget` path, never pushed here.
    pub fn push(&mut self, event: TranscriptEvent) {
        self.bytes = self.bytes.saturating_add(event.source_bytes);
        self.events.push_back(event);
        while self.bytes > WINDOW_MAX_BYTES && self.events.len() > 1 {
            if let Some(dropped) = self.events.pop_front() {
                self.bytes = self.bytes.saturating_sub(dropped.source_bytes);
            }
        }
    }

    /// `push` over an iterator — the same trim after every append.
    pub fn extend(&mut self, events: impl IntoIterator<Item = TranscriptEvent>) {
        for event in events {
            self.push(event);
        }
    }

    /// The retained events, oldest → newest.
    #[must_use]
    pub fn events(&self) -> &VecDeque<TranscriptEvent> {
        &self.events
    }

    /// Source bytes the retained events span.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The retained events as a `Window`'s list, oldest → newest.
    #[must_use]
    pub fn into_events(self) -> Vec<TranscriptEvent> {
        self.events.into()
    }
}

/// One read: the tail events, their combined source bytes, and the
/// cursor past everything consumed (emitted or window-trimmed).
#[derive(Debug)]
pub struct Window {
    /// The deterministic tail — whole records only.
    pub events: Vec<TranscriptEvent>,
    /// Source bytes the emitted events span.
    pub byte_count: u64,
    /// Position after the last consumed record.
    pub cursor: Cursor,
}

/// Read the next window of normalized events from `source`.
///
/// `Cursor::START` reads from the beginning; a returned cursor resumes.
/// A vanished path is not an error — the opened inode is the evidence
/// (`a5_vanish_after_open_stale_inode`); a *replaced* path is.
pub async fn read_window(
    source: &ResolvedSource,
    cursor: Cursor,
) -> Result<Window, TranscriptError> {
    if let Ok(meta) = tokio::fs::metadata(source.path()).await
        && std::os::unix::fs::MetadataExt::ino(&meta) != source.ino()
    {
        return Err(TranscriptError::SourceRewritten {
            reason: "path_replaced",
        });
    }
    let mut file = source.file().lock().await;
    match source.format() {
        Format::PiJsonl => pi_jsonl::read(&mut file, source, cursor).await,
        Format::ClaudeJsonl => claude_jsonl::read(&mut file, source, cursor).await,
        Format::DevinAtif => devin_atif::read(&mut file, source, cursor).await,
    }
}

/// The shared LF-delimited scan both line formats use. `session_header`
/// validates a first-line session record's id; `record_id_key` names the
/// per-record identity field to check; `normalize` maps a record to its
/// supervision event.
pub(super) async fn scan_jsonl(
    file: &mut File,
    cursor: Cursor,
    expected_id: Option<&str>,
    session_header: bool,
    record_id_key: Option<&'static str>,
    normalize: fn(&Value) -> TranscriptEvent,
) -> Result<Window, TranscriptError> {
    let len = file.metadata().await.map_err(|e| unreadable(&e))?.len();
    if cursor.position > len {
        return Err(TranscriptError::SourceRewritten {
            reason: "shorter_than_cursor",
        });
    }
    if cursor.position > 0 {
        verify_anchor(file, cursor).await?;
    }
    let avail = len.saturating_sub(cursor.position);
    file.seek(SeekFrom::Start(cursor.position))
        .await
        .map_err(|e| unreadable(&e))?;
    let mut buf = Vec::new();
    let mut chunk = vec![0_u8; 65_536];
    let mut remaining = avail.min(SCAN_BUDGET_BYTES);
    while remaining > 0 {
        let want = usize::try_from(remaining.min(65_536)).unwrap_or(65_536);
        let Some(dst) = chunk.get_mut(..want) else {
            break;
        };
        let n = file.read(dst).await.map_err(|e| unreadable(&e))?;
        if n == 0 {
            break;
        }
        if let Some(got) = chunk.get(..n) {
            buf.extend_from_slice(got);
        }
        remaining = remaining.saturating_sub(u64::try_from(n).unwrap_or(0));
    }
    let scanned = scan_lines(
        &buf,
        cursor.position,
        expected_id,
        session_header,
        record_id_key,
        normalize,
    )?;
    if scanned.consumed == 0 && avail > SCAN_BUDGET_BYTES {
        // The pending record alone spans the whole scan budget — progress
        // is impossible within it, so the failure is typed, not silent.
        return Err(TranscriptError::SourceExceedsBudget {
            bytes_at_least: SCAN_BUDGET_BYTES.saturating_add(1),
            budget: SCAN_BUDGET_BYTES,
        });
    }
    let position = cursor.position.saturating_add(scanned.consumed);
    // The unconsumed start is `Cursor::START` exactly — an empty anchor
    // sample hashes to the FNV basis, which would make "nothing consumed"
    // differ from the cursor the caller started with.
    let anchor = if position == 0 {
        0
    } else {
        anchor_at(file, position).await?
    };
    Ok(Window {
        events: scanned.events,
        byte_count: scanned.byte_count,
        cursor: Cursor { position, anchor },
    })
}

/// What `scan_lines` produced.
struct Scanned {
    /// The surviving tail events.
    events: Vec<TranscriptEvent>,
    /// Bytes consumed relative to the base (emitted or window-trimmed).
    consumed: u64,
    /// Source bytes the emitted events span.
    byte_count: u64,
}

/// Pure pass over `buf` holding the source bytes from absolute offset
/// `base`: consume every LF-terminated record, keep the window tail.
fn scan_lines(
    buf: &[u8],
    base: u64,
    expected_id: Option<&str>,
    session_header: bool,
    record_id_key: Option<&str>,
    normalize: fn(&Value) -> TranscriptEvent,
) -> Result<Scanned, TranscriptError> {
    let mut tail = BoundedTail::new();
    let mut off = 0_usize;
    while let Some(rest) = buf.get(off..) {
        if rest.is_empty() {
            break;
        }
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            break; // unterminated tail stays pending
        };
        let line_len = nl.saturating_add(1);
        let bytes = u64::try_from(line_len).unwrap_or(u64::MAX);
        let offset = base.saturating_add(u64::try_from(off).unwrap_or(u64::MAX));
        if bytes > WINDOW_MAX_BYTES {
            if tail.events().is_empty() {
                return Err(TranscriptError::RecordExceedsBudget {
                    offset,
                    bytes,
                    resume: Cursor {
                        position: offset.saturating_add(bytes),
                        anchor: fnv1a(tail_sample(rest, nl.saturating_add(1))),
                    },
                });
            }
            break;
        }
        let Some(line) = rest.get(..=nl) else { break };
        let record: Value =
            serde_json::from_slice(line).map_err(|_json| TranscriptError::SourceMalformed {
                offset,
                reason: "invalid_json",
            })?;
        check_identity(&record, offset, expected_id, session_header, record_id_key)?;
        let mut event = normalize(&record);
        event.source_bytes = bytes;
        tail.push(event);
        off = off.saturating_add(line_len);
    }
    Ok(Scanned {
        byte_count: tail.bytes(),
        events: tail.into_events(),
        consumed: u64::try_from(off).unwrap_or(u64::MAX),
    })
}

/// Identity rules on a consumed record: at absolute offset 0 a session
/// header must prove the expected id; a record carrying the id key must
/// match it. Both failures are `session_id_mismatch`.
fn check_identity(
    record: &Value,
    offset: u64,
    expected_id: Option<&str>,
    session_header: bool,
    record_id_key: Option<&str>,
) -> Result<(), TranscriptError> {
    let Some(want) = expected_id else {
        return Ok(());
    };
    let mismatched = TranscriptError::SourceMalformed {
        offset,
        reason: "session_id_mismatch",
    };
    if offset == 0
        && session_header
        && record.get("type").and_then(Value::as_str) == Some("session")
        && record.get("id").and_then(Value::as_str) != Some(want)
    {
        return Err(mismatched);
    }
    if let Some(key) = record_id_key
        && let Some(got) = record.get(key).and_then(Value::as_str)
        && got != want
    {
        return Err(mismatched);
    }
    Ok(())
}

/// FNV-1a over the last `ANCHOR_BYTES` before `pos` — the rewrite anchor.
async fn anchor_at(file: &mut File, pos: u64) -> Result<u64, TranscriptError> {
    Ok(fnv1a(&anchor_bytes(file, pos).await?))
}

/// Read the `≤ ANCHOR_BYTES` sample ending at `pos` from the pinned file.
async fn anchor_bytes(file: &mut File, pos: u64) -> Result<Vec<u8>, TranscriptError> {
    let lo = pos.saturating_sub(ANCHOR_BYTES);
    let want = usize::try_from(pos.saturating_sub(lo)).unwrap_or(0);
    let mut buf = vec![0_u8; want];
    file.seek(SeekFrom::Start(lo))
        .await
        .map_err(|e| unreadable(&e))?;
    file.read_exact(&mut buf).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            TranscriptError::SourceRewritten {
                reason: "vanished_mid_read",
            }
        } else {
            unreadable(&e)
        }
    })?;
    Ok(buf)
}

/// The byte before the cursor must be the record terminator and the tail
/// sample must match the issued anchor — anything else is a rewrite.
async fn verify_anchor(file: &mut File, cursor: Cursor) -> Result<(), TranscriptError> {
    let sample = anchor_bytes(file, cursor.position).await?;
    if sample.last() != Some(&b'\n') {
        return Err(TranscriptError::SourceRewritten {
            reason: "boundary_lost",
        });
    }
    if fnv1a(&sample) != cursor.anchor {
        return Err(TranscriptError::SourceRewritten {
            reason: "anchor_mismatch",
        });
    }
    Ok(())
}

/// The last `≤ ANCHOR_BYTES` of `bytes` — the same sample `anchor_at`
/// reads, for cursors minted without a re-read.
pub(super) fn tail_sample(bytes: &[u8], end: usize) -> &[u8] {
    let lo = end.saturating_sub(usize::try_from(ANCHOR_BYTES).unwrap_or(64));
    bytes
        .get(..end)
        .map_or(&[], |head| head.get(lo..).unwrap_or(&[]))
}

/// FNV-1a — a content-sample hash for rewrite detection, nothing more.
pub(super) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A string field as an owned `Option`, verbatim.
pub(super) fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
