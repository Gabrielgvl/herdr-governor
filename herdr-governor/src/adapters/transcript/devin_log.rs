//! `devin_log` — §4.17's typed provider-limit record (F31): the harness's
//! own per-process log is the evidence source for the provider-limit line,
//! and the same scan for the id-keyed session formats reads the session
//! file's tail. Both paths emit one `LimitRecord` — `source`, the record's
//! own `observed_at`, and the stated `reset_at` — or `None` (fail closed:
//! unreadable, untrusted or ambiguous evidence never becomes a record).
//!
//! The process-log format is external and unversioned; the shapes below
//! were observed on the deployed CLI (2026-10) and anything else fails
//! closed. Hand-rolled prefix/suffix matching — no regex crate.

use std::io::SeekFrom;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use governor_core::identity::Timestamp;
use tokio::fs;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

mod time;

use super::claude_jsonl;
use super::pointer::{self, Kind, SessionPointer, TranscriptRoots};

pub(super) use time::{ms_rfc3339, rfc3339_ms};

/// The typed provider-limit record an evidence pass can prove (§4.17):
/// which native source produced it, the record's own timestamp and the
/// provider's stated reset instant when present. `record_id` = `<source>:
/// <observed_at_ms>` — the once-per-record dedup key `limit:<record_id>`
/// (F34) is minted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitRecord {
    /// The wire `source` (`*_session_quota` / `*_process_log`).
    pub source: &'static str,
    /// The record's own timestamp — ms from its `Z` line/record.
    pub observed_at: Timestamp,
    /// The provider's stated reset instant when the record carries one.
    pub reset_at: Option<Timestamp>,
}

impl LimitRecord {
    /// `<source>:<observed_at_ms>` — the record's dedup identity, the
    /// `limit:<record_id>` ask family's suffix (§4.17/F34).
    #[must_use]
    pub fn record_id(&self) -> String {
        format!("{}:{}", self.source, self.observed_at.0)
    }

    /// `observed_at` rendered `YYYY-MM-DDTHH:MM:SS.mmmZ` for the wire.
    #[must_use]
    pub fn observed_at_rfc3339(&self) -> String {
        ms_rfc3339(self.observed_at.0)
    }

    /// `reset_at` rendered the same when present.
    #[must_use]
    pub fn reset_at_rfc3339(&self) -> Option<String> {
        self.reset_at.map(|at| ms_rfc3339(at.0))
    }
}

/// The tail bound for one process log / session file (§4.17).
const TAIL_BYTES: u64 = 256 * 1024;

/// Newest-first candidate logs read per check; one process per session
/// keeps the bound small.
const MAX_LOGS: usize = 32;

/// The `<id>` the process log's session lines name (`[a-z0-9][a-z0-9-]{0,63}`).
fn session_id_ok(id: &str) -> bool {
    let mut bytes = id.bytes();
    let head_ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    head_ok
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `devin_<YYYYMMDD-HHMMSS>_<pid>.log` — `.log.gz` archives and every other
/// name are never candidates (F36: a rotation of a live log is unproven, so
/// the reader fails closed rather than decompress).
fn log_name_ok(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix("devin_")
        .and_then(|rest| rest.strip_suffix(".log"))
    else {
        return false;
    };
    let Some((date, rest)) = body.split_once('-') else {
        return false;
    };
    let Some((time, pid)) = rest.split_once('_') else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    digits(date, 8) && digits(time, 6) && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
}

/// The log's session-binding line: `Created new session: <id>` or
/// `for session <id>` names `([a-z0-9-]+)\b` — the first needle's leftmost
/// match wins and the capture backtracks the greedy run to the largest
/// word-boundary prefix, exactly as the reference regex does.
fn session_line(line: &str) -> Option<&str> {
    let mut rest = line;
    while !rest.is_empty() {
        let created = rest.find("Created new session: ").map(|p| (p, 21));
        let for_session = rest.find("for session ").map(|p| (p, 12));
        let (pos, len) = [created, for_session]
            .into_iter()
            .flatten()
            .min_by_key(|(p, _)| *p)?;
        let after = rest.get(pos.saturating_add(len)..)?;
        let bytes = after.as_bytes();
        let run = after
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            .count();
        // `\b` after the greedy run: the largest `k` where exactly one side
        // is a word char (in-run chars are word iff not `-`).
        for k in (1..=run).rev() {
            let prev_word = bytes.get(k.saturating_sub(1)).is_some_and(|b| *b != b'-');
            let next_word = bytes
                .get(k)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            if prev_word != next_word {
                return after.get(..k);
            }
        }
        rest = rest.get(pos.saturating_add(1)..)?;
    }
    None
}

/// The stall line: `<ts> ERROR affogato::agent::control_loop: attempts=<n>
/// error=Inference(ServerError(message=Reached free model rate limit.<…>))
/// Exhausted inference retries; stopping turn`. The `WARN attempt=N …
/// Transient inference error` retries before it are not a stall. Returns
/// the timestamp token and the parenthesized tail.
fn limit_line(line: &str) -> Option<(&str, &str)> {
    let head = line
        .trim_end()
        .strip_suffix(")) Exhausted inference retries; stopping turn")?;
    let (ts, fields) = head.split_once(' ')?;
    if ts.is_empty() {
        return None;
    }
    let attempts = fields.strip_prefix("ERROR affogato::agent::control_loop: attempts=")?;
    let digits = attempts.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let message = attempts
        .get(digits..)?
        .strip_prefix(" error=Inference(ServerError(message=Reached free model rate limit.")?;
    Some((ts, message))
}

/// `Your limit will reset in (\d+) (second|minute|hour)s?\b` → the delay
/// in ms — later occurrences are scanned when an earlier one fails to
/// parse, as the reference regex's global scan does.
fn reset_delay_ms(text: &str) -> Option<i64> {
    let mut rest = text;
    while let Some(pos) = rest.find("Your limit will reset in ") {
        rest = rest.get(pos.saturating_add(25)..)?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let count = rest.get(..digits).and_then(|n| n.parse::<i64>().ok());
        let word = rest.get(digits..).and_then(|s| s.strip_prefix(' '));
        if let (Some(n), Some(unit_text)) = (count, word) {
            for (unit, unit_ms) in [("second", 1_000), ("minute", 60_000), ("hour", 3_600_000)] {
                let Some(tail) = unit_text.strip_prefix(unit) else {
                    continue;
                };
                let after_s = tail.strip_prefix('s').unwrap_or(tail);
                let boundary = after_s
                    .as_bytes()
                    .first()
                    .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'));
                if boundary {
                    return n.checked_mul(unit_ms);
                }
            }
        }
    }
    None
}

/// One scanned log's verdict: `Foreign` keeps looking (another process's
/// log); `Ours` — the tail bound to this session — ends the scan with the
/// last stall line's record or none.
enum Verdict {
    /// The tail names a different session — keep scanning other logs.
    Foreign,
    /// The tail bound to this session — `Some` carries the newest stall
    /// line's record at or after `cycle_start`, `None` when none matches.
    Ours(Option<LimitRecord>),
}

/// Scan one log's tail lines: a session line naming another session makes
/// the log foreign; the newest stall line `>= cycle_start` is the record.
fn scan_lines(lines: &[String], session: &str, cycle_start: Timestamp) -> Verdict {
    let mut bound = false;
    let mut limit = None;
    for line in lines {
        if let Some(named) = session_line(line) {
            if named != session {
                return Verdict::Foreign;
            }
            bound = true;
            continue;
        }
        let Some((ts_text, message)) = limit_line(line) else {
            continue;
        };
        let Some(at) = rfc3339_ms(ts_text) else {
            continue;
        };
        if at < cycle_start.0 {
            continue;
        }
        limit = Some(LimitRecord {
            source: "devin_process_log",
            observed_at: Timestamp(at),
            reset_at: reset_delay_ms(message).map(|delay| Timestamp(at.saturating_add(delay))),
        });
    }
    if bound {
        Verdict::Ours(limit)
    } else {
        Verdict::Foreign
    }
}

/// A log the daemon's user owns outright: regular file, not a symlink,
/// owned by the euid, never world-writable.
fn trusted(meta: &std::fs::Metadata) -> bool {
    meta.is_file()
        && !meta.file_type().is_symlink()
        && meta.uid() == uid()
        && meta.mode() & 0o002 == 0
}

/// The daemon's real uid — the reference's `process.getuid()`.
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// `mtime` in epoch ms for the `cycle_start` candidate gate.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000)
        .saturating_add(meta.mtime_nsec().saturating_div(1_000_000))
}

/// Read the last `TAIL_BYTES` of `path` as whole lines — the open runs on
/// the blocking pool with `O_NONBLOCK` so a FIFO named like a log can
/// never pin an executor thread, `O_NOFOLLOW` and the `fstat` verdict on
/// the returned descriptor (re-checked `(dev, ino)` against the listing)
/// refuse a swap between the two reads; a partial first or last line is
/// dropped.
async fn tail_lines(path: &Path, dev: u64, ino: u64) -> Option<Vec<String>> {
    let owned = path.to_path_buf();
    let fd = tokio::task::spawn_blocking(move || {
        rustix::fs::open(
            owned,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
    })
    .await
    .ok()?
    .ok()?;
    let mut file = fs::File::from_std(std::fs::File::from(fd));
    let meta = file.metadata().await.ok()?;
    if !trusted(&meta) || meta.dev() != dev || meta.ino() != ino {
        return None;
    }
    let start = meta.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).await.ok()?;
    let mut buf = Vec::new();
    (&mut file)
        .take(TAIL_BYTES)
        .read_to_end(&mut buf)
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    // the tail ends at a whole line — a partial last line (a write in
    // flight) is dropped
    lines.pop();
    Some(lines)
}

/// Scan `dir` for this session's process log: `devin_<stamp>_<pid>.log`
/// candidates `mtime >= cycle_start`, newest first, at most `MAX_LOGS`;
/// a candidate's tail must bind to the session and the newest stall line
/// at or after `cycle_start` is the record. `None` is the fail-closed
/// verdict — an unreadable dir, only foreign or untrusted logs, or no
/// stall line are all "no record".
async fn scan_devin_log_dir(
    dir: &Path,
    session: &str,
    cycle_start: Timestamp,
) -> Option<LimitRecord> {
    if !session_id_ok(session) {
        return None;
    }
    let mut entries = fs::read_dir(dir).await.ok()?;
    let mut candidates = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !log_name_ok(name) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !trusted(&meta) || mtime_ms(&meta) < cycle_start.0 {
            continue;
        }
        candidates.push((mtime_ms(&meta), entry.path(), meta.dev(), meta.ino()));
    }
    candidates.sort_by_key(|candidate| core::cmp::Reverse(candidate.0));
    for (_, path, dev, ino) in candidates.into_iter().take(MAX_LOGS) {
        let Some(lines) = tail_lines(&path, dev, ino).await else {
            continue;
        };
        match scan_lines(&lines, session, cycle_start) {
            Verdict::Ours(record) => return record,
            Verdict::Foreign => {}
        }
    }
    None
}

/// The default process-log dir — the harness data dir the `[daemon]`
/// `devin_log_dir` key overrides (§4.17's `$XDG_DATA_HOME/devin/cli/logs`).
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")))
        .map(|data| data.join("devin/cli/logs"))
}

/// The typed provider-limit record a Run's native evidence carries —
/// `None` when the session kind has no such source or the evidence proves
/// nothing this pass. Devin reads its process-log dir (never the ATIF
/// transcript — a step quoting the phrase is task output, not a provider
/// record, P7); the id-keyed session log's tail is scanned for the typed
/// 429 record.
pub async fn limit_record(
    pointer: &SessionPointer,
    roots: &TranscriptRoots,
    log_dir: Option<&Path>,
    cycle_start: Timestamp,
) -> Option<LimitRecord> {
    let (kind, session, cwd) = pointer.limit_probe();
    match kind {
        Kind::Devin => scan_devin_log_dir(log_dir?, session, cycle_start).await,
        Kind::Claude => {
            let source = pointer::resolve(pointer, roots).await.ok()?;
            claude_jsonl::limit_record(&source, session, cwd, cycle_start).await
        }
        Kind::Pi | Kind::Other => None,
    }
}

// —————————————————————————————————————————————————————————————————————
// unit tests — the parser halves the e2e doesn't reach
// —————————————————————————————————————————————————————————————————————
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_line_matches_the_stall_only() {
        let line = "2026-10-02T01:33:04.421266Z ERROR affogato::agent::control_loop: attempts=3 error=Inference(ServerError(message=Reached free model rate limit. Upgrade to Max for higher limits, or switch to a different model. Your limit will reset in 26 minutes. (trace ID: 0000000000000000))) Exhausted inference retries; stopping turn";
        let (ts, message) = limit_line(line).expect("the stall line");
        assert_eq!(ts, "2026-10-02T01:33:04.421266Z");
        assert_eq!(reset_delay_ms(message), Some(26 * 60_000), "26 minutes");
        // the WARN retry shape (`attempt=`, different suffix) never matches
        let warn = "2026-10-02T01:33:03.685383Z  WARN affogato::agent::control_loop: attempt=1 max=3 error=Inference(ServerError(message=Reached free model rate limit. x)) Transient inference error; retrying on next iteration";
        assert!(limit_line(warn).is_none());
        // a different inference error is not a provider limit
        let other = "2026-10-02T01:33:04.421266Z ERROR affogato::agent::control_loop: attempts=3 error=Inference(ServerError(message=Model exploded.))) Exhausted inference retries; stopping turn";
        assert!(limit_line(other).is_none());
    }

    #[test]
    fn session_line_binds_only_listed_forms() {
        assert_eq!(
            session_line("2026-10-02T01:33:02Z  INFO x: Created new session: tidal-vase"),
            Some("tidal-vase")
        );
        assert_eq!(
            session_line(
                "… session_db: Saved 4 message nodes (starting from 0) for session tidal-vase"
            ),
            Some("tidal-vase")
        );
        assert_eq!(session_line("no session here"), None);
        // uppercase in the id position is not a session id
        assert_eq!(session_line("for session Tidal-Vase"), None);
        // a word char after the run keeps it unbound (`tidal_vase` is `\w`)
        assert_eq!(session_line("for session tidal_vase"), None);
    }

    #[test]
    fn log_name_ok_accepts_plain_logs_only() {
        assert!(log_name_ok("devin_20261001-223258_3826277.log"));
        for bad in [
            "devin_20261001-223258_3826277.log.gz",
            "devin_2026101-223258_1.log",
            "devin_20261001-223258_x.log",
            "devin_20261001_223258_1.log",
            "other_20261001-223258_1.log",
            "devin_.log",
            "notes.log",
        ] {
            assert!(!log_name_ok(bad), "{bad}");
        }
    }
}
