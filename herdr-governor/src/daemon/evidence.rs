//! `evidence` — §4.9's evidence bundle (F23, → F12): per supervised Run
//! the coordinator keeps one `EvidenceTail` — the accumulated
//! `BoundedTail` (the transcript adapter's one trim rule, → F40) and the
//! cursor it resumes from — so a pass that reads only the delta still
//! judges the last ≤ 32 KiB of records, and a restart that rebuilds from
//! `Cursor::START` lands on the same tail and the same digest.
//!
//! `gather` is the async I/O half, run off the coordinator: resolve the
//! session pointer and read forward from the cursor; when no transcript
//! can be read (`Unreadable`, `SourceUnreadable` — a document-format
//! child writes its document only at turn end, so it is absent during
//! the first turn — `Ambiguous`, a malformed or over-budget source)
//! fall back to the terminal, `agent.read(Recent, ≤ 200 lines)`
//! (ADR-0002), so the Run is still reviewed; `worktree_evidence` runs
//! only for a Run that pinned a base (→ F6). The coordinator absorbs the result, renders the bundle with
//! §13's redaction, and digests the canonical rendering.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use governor_core::identity::{Digest, EffectKey, PaneId, RunId, Timestamp};
use governor_core::lifecycle::Run;
use serde::Serialize;
use sha2::Digest as _;

use crate::adapters::git::{GitError, worktree_evidence};
use crate::adapters::herdr::{Client as HerdrClient, ReadOpts, ReadSource};
use crate::adapters::jev::{GitState, TranscriptLine};
use crate::adapters::transcript::{
    BoundedTail, Cursor, EventKind, LimitRecord, SessionPointer, TranscriptError, TranscriptRoots,
    limit_record, read_window, resolve,
};

/// ADR-0002's terminal fallback bound: the last 200 lines of the pane.
const TERMINAL_LINES: u32 = 200;

/// Reads per pass: each consumes up to the adapter's 16 MiB scan budget;
/// an oversized record or a rewrite costs one extra read each.
/// ponytail: a source growing faster than 8 × 16 MiB per review interval
/// stays behind until the next pass — raise the bound if D1 shows it.
const READS_PER_PASS: usize = 8;

/// The rendered, redacted, digested evidence a Jev ask carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bundle {
    /// The accumulated tail, one line per record.
    pub transcript: Vec<TranscriptLine>,
    /// The terminal fallback — present only when no transcript was read.
    pub terminal: Option<String>,
    /// `{head, dirty}` — present only for a Run with a base commit.
    pub git: Option<GitState>,
    /// sha-256 over the canonical rendering of the three fields above.
    pub digest: Digest,
    /// The Run's `evidence_generation` once its `evidence_digest` is
    /// this bundle's — an ask renders only from a stamped bundle.
    pub generation: Option<u64>,
}

/// One Run's evidence state in the coordinator (in memory — a restart
/// rebuilds it from `Cursor::START`).
#[derive(Debug, Default)]
pub(super) struct EvidenceTail {
    /// The accumulated ≤ 32 KiB tail.
    tail: BoundedTail,
    /// Where the next read resumes.
    cursor: Option<Cursor>,
    /// The last absorbed bundle.
    bundle: Option<Bundle>,
    /// When the last gather was absorbed — the review-interval clock.
    gathered_at: Option<Timestamp>,
    /// The typed provider-limit records observed for the Run (F31) —
    /// keyed `record_id` (`<source>:<observed_at_ms>`); once proven a
    /// record stays known for the Run's lifetime — it is the
    /// `limit:<record_id>` ask's own evidence.
    records: BTreeMap<String, LimitRecord>,
    /// A gather task is out — at most one per Run.
    in_flight: bool,
}

impl EvidenceTail {
    /// §4.7 step 4 — gather when none is out and the bundle is missing,
    /// does not match the Run's recorded evidence (a freeze bumped the
    /// generation: "immediately at freeze"), or the review interval
    /// elapsed.
    pub(super) fn needs_gather(&self, run: &Run, now: Timestamp, interval: Duration) -> bool {
        if self.in_flight {
            return false;
        }
        if self.bundle_for(run).is_none() {
            return true;
        }
        let window_ms = i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
        self.gathered_at
            .is_none_or(|at| now.0 >= at.0.saturating_add(window_ms))
    }

    /// The bundle an ask may render: stamped for the Run's current
    /// `evidence_generation` and equal to its recorded `evidence_digest`.
    pub(super) fn bundle_for(&self, run: &Run) -> Option<&Bundle> {
        self.bundle.as_ref().filter(|bundle| {
            bundle.generation == Some(run.evidence_generation)
                && run.evidence_digest == Some(bundle.digest)
        })
    }

    /// The gather request's mutable half — the tail and cursor travel to
    /// the task and come back in `Gathered`; marks the entry in flight.
    pub(super) fn checkout(&mut self) -> (BoundedTail, Cursor) {
        self.in_flight = true;
        (self.tail.clone(), self.cursor.unwrap_or(Cursor::START))
    }

    /// Take a finished gather back: the advanced tail and cursor, a fresh
    /// unstamped bundle, the typed limit record. Returns the digest.
    pub(super) fn absorb(&mut self, gathered: Gathered, now: Timestamp) -> Digest {
        let bundle = render(&gathered.tail, gathered.terminal.as_deref(), gathered.git);
        let digest = bundle.digest;
        self.tail = gathered.tail;
        self.cursor = Some(gathered.cursor);
        self.bundle = Some(bundle);
        if let Some(record) = gathered.limit {
            self.records.insert(record.record_id(), record);
        }
        self.gathered_at = Some(now);
        self.in_flight = false;
        digest
    }

    /// The newest record observed — a `blocked:` ask's `limitRecord`, or
    /// the stand-in when a restart dropped a `limit:` ask's keyed record.
    pub(super) fn latest_limit(&self) -> Option<&LimitRecord> {
        self.records
            .values()
            .max_by_key(|record| record.observed_at.0)
    }

    /// The record a `limit:<record_id>[:<n>]` ask names — retry suffixes
    /// resolve to the base record.
    pub(super) fn record_for(&self, key: &EffectKey) -> Option<&LimitRecord> {
        let suffix = super::runner::seam::suffix_of(key).strip_prefix("limit:")?;
        if let Some(record) = self.records.get(suffix) {
            return Some(record);
        }
        let (base, attempt) = suffix.rsplit_once(':')?;
        if attempt.is_empty() || !attempt.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        self.records.get(base)
    }

    /// Stamp the bundle with `run`'s generation when the Run recorded its
    /// digest (an `Event::Evidence` that applied, or an unchanged digest).
    pub(super) fn stamp(&mut self, run: &Run) {
        if let Some(bundle) = &mut self.bundle {
            bundle.generation =
                (run.evidence_digest == Some(bundle.digest)).then_some(run.evidence_generation);
        }
    }
}

/// Everything one gather needs, frozen at the tick.
#[derive(Debug)]
pub(super) struct GatherRequest {
    /// The Run.
    pub run: RunId,
    /// The session pointer — `None` when Herdr reported no session.
    pub pointer: Option<SessionPointer>,
    /// Where pointers resolve.
    pub roots: TranscriptRoots,
    /// The accumulated tail and its cursor (`EvidenceTail::checkout`).
    pub tail: (BoundedTail, Cursor),
    /// The child's current pane — the terminal fallback's target.
    pub pane: PaneId,
    /// The worktree — read only when `with_git`.
    pub cwd: String,
    /// The Run pinned a base commit (F6: otherwise git is omitted).
    pub with_git: bool,
    /// The Herdr client and op deadline for the fallback read.
    pub herdr: (HerdrClient, Duration),
    /// The first task prompt's `dispatched_at` — the limit record's
    /// `>=` bound (F31/F34; nudges and follow-ups never move it). `None`
    /// fails the probe closed.
    pub cycle_start: Option<Timestamp>,
    /// The process-log dir for the limit probe — `None` disables it.
    pub log_dir: Option<std::path::PathBuf>,
    /// Whether the owner's session was absent at the tick (F23's pause).
    pub owner_absent: bool,
}

/// A finished gather — posted back to the coordinator as `Msg::Evidence`.
#[derive(Debug)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) struct Gathered {
    /// The Run.
    pub run: RunId,
    /// The advanced tail.
    pub tail: BoundedTail,
    /// The advanced cursor.
    pub cursor: Cursor,
    /// The terminal fallback text, when no transcript could be read.
    pub terminal: Option<String>,
    /// The worktree evidence.
    pub git: Option<GitState>,
    /// The typed provider-limit record this pass proved (F31) — it does
    /// not enter the bundle or its digest; it rides beside them.
    pub limit: Option<LimitRecord>,
    /// Carried from the request.
    pub owner_absent: bool,
}

/// The async gather: the transcript leg, the terminal fallback when it
/// read nothing, the git leg for a base-pinned Run.
pub(super) async fn gather(request: GatherRequest) -> Gathered {
    let GatherRequest {
        run,
        pointer,
        roots,
        tail: (mut tail, mut cursor),
        pane,
        cwd,
        with_git,
        herdr: (herdr, op),
        cycle_start,
        log_dir,
        owner_absent,
    } = request;
    let read = match &pointer {
        Some(found) => read_tail(found, &roots, &mut tail, &mut cursor).await,
        None => Err(()),
    };
    let terminal = match read {
        Ok(()) => None,
        Err(()) => terminal(&herdr, &pane, op).await,
    };
    let git = if with_git {
        git(Path::new(&cwd)).await
    } else {
        None
    };
    // F31 — the typed limit record rides every pass: a Run whose
    // transcript reads nothing still carries its provider's record.
    let limit = match (&pointer, cycle_start) {
        (Some(found), Some(start)) => limit_record(found, &roots, log_dir.as_deref(), start).await,
        _ => None,
    };
    Gathered {
        run,
        tail,
        cursor,
        terminal,
        git,
        limit,
        owner_absent,
    }
}

/// Read forward from `cursor` into `tail` until a read consumes nothing:
/// an oversized record is skipped past (`resume`), a rewrite resets the
/// tail and rebuilds from `Cursor::START` once (the evidence legitimately
/// changes); every other failure means no transcript this pass.
async fn read_tail(
    pointer: &SessionPointer,
    roots: &TranscriptRoots,
    tail: &mut BoundedTail,
    cursor: &mut Cursor,
) -> Result<(), ()> {
    let source = resolve(pointer, roots).await.map_err(drop)?;
    let mut rebuilt = false;
    for _ in 0..READS_PER_PASS {
        match read_window(&source, *cursor).await {
            Ok(window) if window.cursor == *cursor => return Ok(()),
            Ok(window) => {
                tail.extend(window.events);
                *cursor = window.cursor;
            }
            Err(TranscriptError::RecordExceedsBudget { resume, .. }) => *cursor = resume,
            Err(TranscriptError::SourceRewritten { .. }) if !rebuilt => {
                rebuilt = true;
                *tail = BoundedTail::new();
                *cursor = Cursor::START;
            }
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

/// ADR-0002's fallback — the pane's recent text, `None` when Herdr
/// cannot answer (the pane may be gone).
async fn terminal(herdr: &HerdrClient, pane: &PaneId, op: Duration) -> Option<String> {
    let opts = ReadOpts {
        lines: Some(TERMINAL_LINES),
        ..ReadOpts::default()
    };
    herdr
        .agent_read(&pane.0, ReadSource::Recent, &opts, op)
        .await
        .ok()
        .map(|read| read.value.text)
}

/// `worktree_evidence`, re-read once on `HeadMoved` (§4.9); any other
/// failure omits git evidence for this pass.
async fn git(cwd: &Path) -> Option<GitState> {
    let evidence = match worktree_evidence(cwd).await {
        Err(GitError::HeadMoved { .. }) => worktree_evidence(cwd).await,
        other => other,
    };
    evidence.ok().map(|ev| GitState {
        head: ev.head,
        dirty: ev.dirty,
    })
}

/// The canonical rendering the digest covers — field order fixed.
#[derive(Serialize)]
struct Canonical<'a> {
    transcript: &'a [TranscriptLine],
    terminal: Option<&'a str>,
    git: Option<&'a GitState>,
}

/// Render the bundle: one redacted line per tail record, the redacted
/// terminal, git verbatim; sha-256 over the canonical JSON.
fn render(tail: &BoundedTail, screen: Option<&str>, git: Option<GitState>) -> Bundle {
    let transcript: Vec<TranscriptLine> = tail
        .events()
        .iter()
        .map(|event| TranscriptLine {
            timestamp: event.timestamp.clone(),
            role: event.role.clone(),
            kind: kind_name(event.kind).to_owned(),
            text: event.text.as_deref().map(redact),
        })
        .collect();
    let terminal = screen.map(redact);
    let canonical = Canonical {
        transcript: &transcript,
        terminal: terminal.as_deref(),
        git: git.as_ref(),
    };
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    Bundle {
        transcript,
        terminal,
        git,
        digest: Digest(sha2::Sha256::digest(&bytes).into()),
        generation: None,
    }
}

/// The adapter's evidence class as its wire spelling.
fn kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Session => "session",
        EventKind::Message => "message",
        EventKind::ToolCall => "tool_call",
        EventKind::ToolResult => "tool_result",
        EventKind::Error => "error",
        EventKind::Meta => "meta",
        EventKind::UserTurn => "user_turn",
        EventKind::Ambiguous => "ambiguous",
    }
}

/// §13 — a line shaped `^[A-Z][A-Z0-9_]*=` (an environment assignment)
/// becomes `<redacted>` before Jev and before the digest; every other
/// line passes verbatim.
pub(super) fn redact(text: &str) -> String {
    text.split('\n')
        .map(|line| if assignment(line) { "<redacted>" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `^[A-Z][A-Z0-9_]*=`.
fn assignment(line: &str) -> bool {
    let mut bytes = line.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_uppercase()) {
        return false;
    }
    for byte in bytes {
        if byte == b'=' {
            return true;
        }
        if !(byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_replaces_only_assignment_lines() {
        let text = "ran tests\nAPI_KEY=hunter2\nA1_B=x\nlower=ok\nAPI KEY=no\n=x";
        assert_eq!(
            redact(text),
            "ran tests\n<redacted>\n<redacted>\nlower=ok\nAPI KEY=no\n=x"
        );
    }

    #[test]
    fn render_digest_is_stable_and_terminal_sensitive() {
        let tail = BoundedTail::new();
        let a = render(&tail, None, None);
        let b = render(&tail, None, None);
        let c = render(&tail, Some("screen"), None);
        assert_eq!(a.digest, b.digest);
        assert_ne!(a.digest, c.digest);
        assert_eq!(c.terminal.as_deref(), Some("screen"));
    }
}
