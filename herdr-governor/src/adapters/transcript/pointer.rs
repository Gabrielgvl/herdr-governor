//! `pointer` — SessionPointer resolution: the catalog harness kind plus
//! the native session locator (a path or an id) become a `ResolvedSource`
//! holding the file open, so reads see the inode the pointer named. Path
//! and harness literals exist only under this module subtree (I9,
//! ADR-0002): everything outside passes kind and locator as data.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use tokio::fs::{self, File};
use tokio::sync::Mutex;

use super::error::{TranscriptError, unreadable};

/// Which transcript format a resolved source holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// LF-delimited session log with a header record at offset 0.
    PiJsonl,
    /// LF-delimited session log with a per-record session id; symlinks
    /// are refused at resolve time.
    ClaudeJsonl,
    /// One whole ATIF JSON document of steps.
    DevinAtif,
}

/// Which harness family a pointer names — parsed from the catalog id in
/// `SessionPointer::new`, the only place the names exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `pi` — the locator is the session file's path.
    Pi,
    /// `devin` — the locator is the session id under a data root.
    Devin,
    /// `claude` — the locator is the session id under project roots.
    Claude,
    /// Any harness without a transcript source (terminal evidence only).
    Other,
}

impl Kind {
    fn from_name(name: &str) -> Self {
        if name == "pi" {
            Self::Pi
        } else if name == "devin" {
            Self::Devin
        } else if name == "claude" {
            Self::Claude
        } else {
            Self::Other
        }
    }
}

/// A session-transcript pointer: the catalog harness kind, the native
/// session locator Herdr reported (a file path or a session id), and the
/// session cwd when the caller knows it.
#[derive(Debug, Clone)]
pub struct SessionPointer {
    kind: Kind,
    native_session: String,
    cwd: Option<String>,
}

impl SessionPointer {
    /// `kind` is the opaque catalog harness id — it is matched here, the
    /// only module allowed to name harnesses.
    #[must_use]
    pub fn new(kind: &str, native_session: &str, cwd: Option<&str>) -> Self {
        Self {
            kind: Kind::from_name(kind),
            native_session: native_session.to_owned(),
            cwd: cwd.map(str::to_owned),
        }
    }

    /// The limit-record probe's inputs (F31): the harness family, the
    /// native session locator, and the session cwd — `resolve` keeps the
    /// full pointer.
    pub(super) fn limit_probe(&self) -> (Kind, &str, Option<&str>) {
        (self.kind, self.native_session.as_str(), self.cwd.as_deref())
    }
}

/// The directories `resolve` searches. `from_env` derives them from the
/// harnesses' config/data dirs; tests and catalog data pass them
/// explicitly.
#[derive(Debug, Clone, Default)]
pub struct TranscriptRoots {
    /// `<id>.json` document roots, searched in order — first existing
    /// candidate wins deterministically.
    pub data_dirs: Vec<PathBuf>,
    /// `<slug>/<id>.jsonl` project roots.
    pub project_dirs: Vec<PathBuf>,
}

impl TranscriptRoots {
    /// Explicit roots — resolution order is the vector's.
    #[must_use]
    pub fn new(data_dirs: Vec<PathBuf>, project_dirs: Vec<PathBuf>) -> Self {
        Self {
            data_dirs,
            project_dirs,
        }
    }

    /// Env-derived roots: `$XDG_DATA_HOME` (else `~/.local/share`) holds
    /// the ATIF documents, `$CLAUDE_CONFIG_DIR` (else `~/.claude`) the
    /// project trees (A5 native locations).
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let conf = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".claude")));
        Self {
            data_dirs: xdg
                .map(|d| vec![d.join("devin/cli/transcripts")])
                .unwrap_or_default(),
            project_dirs: conf.map(|d| vec![d.join("projects")]).unwrap_or_default(),
        }
    }
}

/// A resolved source — the file stays open, so a vanished path still
/// reads the pinned inode (`a5_vanish_after_open_stale_inode`).
#[derive(Debug)]
pub struct ResolvedSource {
    file: Mutex<File>,
    path: PathBuf,
    ino: u64,
    format: Format,
    expected_id: Option<String>,
}

impl ResolvedSource {
    /// The path the pointer resolved to — a locator, not liveness proof.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The probe-vocabulary source name (`pi-jsonl`, `devin-session`,
    /// `claude-jsonl`) — for logs and evidence records.
    #[must_use]
    pub fn source_kind(&self) -> &'static str {
        match self.format {
            Format::PiJsonl => "pi-jsonl",
            Format::ClaudeJsonl => "claude-jsonl",
            Format::DevinAtif => "devin-session",
        }
    }

    pub(super) fn file(&self) -> &Mutex<File> {
        &self.file
    }

    pub(super) fn ino(&self) -> u64 {
        self.ino
    }

    pub(super) fn format(&self) -> Format {
        self.format
    }

    /// The identity the source content must prove — the session id for
    /// id-keyed formats, the filename-derived id for the path-keyed one.
    /// `None` means the check has nothing to compare (documented gap:
    /// a non-canonical filename yields no derivable id).
    pub(super) fn expected_id(&self) -> Option<&str> {
        self.expected_id.as_deref()
    }
}

/// Resolve a pointer to an open source. Candidates are chosen
/// deterministically (first root wins; no glob for id-keyed path
/// pointers) and the file is held open from this point on.
pub async fn resolve(
    pointer: &SessionPointer,
    roots: &TranscriptRoots,
) -> Result<ResolvedSource, TranscriptError> {
    match pointer.kind {
        Kind::Pi => resolve_path(&pointer.native_session).await,
        Kind::Devin => resolve_document(&pointer.native_session, roots).await,
        Kind::Claude => {
            resolve_slugged(&pointer.native_session, pointer.cwd.as_deref(), roots).await
        }
        Kind::Other => Err(TranscriptError::Unreadable {
            reason: "no_transcript_source",
        }),
    }
}

/// The session file path Herdr reported — opened verbatim (symlinks
/// followed). A bare id is refused: `kind_not_path` — the governor does
/// not glob for an id (`a5_pi_exact_path_no_glob`).
async fn resolve_path(native_session: &str) -> Result<ResolvedSource, TranscriptError> {
    if !native_session.contains('/') {
        return Err(TranscriptError::SessionPointerInvalid {
            reason: "kind_not_path",
        });
    }
    let path = PathBuf::from(native_session);
    let expected_id = path_id(&path);
    open_source(&path, Format::PiJsonl, expected_id.as_deref()).await
}

/// `<id>.json` under the data roots — first existing candidate wins
/// (`a5_devin_duplicate_root_exact`); an unsafe id is refused before any
/// path is touched.
async fn resolve_document(
    id: &str,
    roots: &TranscriptRoots,
) -> Result<ResolvedSource, TranscriptError> {
    if !filename_safe(id) {
        return Err(TranscriptError::SessionPointerInvalid {
            reason: "id_not_filename_safe",
        });
    }
    let name = format!("{id}.json");
    let mut first = None;
    for dir in &roots.data_dirs {
        let cand = dir.join(&name);
        if first.is_none() {
            first = Some(cand.clone());
        }
        if fs::symlink_metadata(&cand).await.is_ok() {
            return open_source(&cand, Format::DevinAtif, Some(id)).await;
        }
    }
    match first {
        Some(cand) => open_source(&cand, Format::DevinAtif, Some(id)).await,
        None => Err(TranscriptError::Unreadable { reason: "no_roots" }),
    }
}

/// `<slug>/<id>.jsonl` under the project roots; the slug derives from the
/// session cwd. A known cwd pins the candidate; without one every root is
/// walked and more than one match is `Ambiguous` — no general resolver
/// exists (`a5_claude_ambiguous_candidates`). A symlinked candidate is
/// refused outright — this reader's policy differs from the other two.
async fn resolve_slugged(
    id: &str,
    cwd: Option<&str>,
    roots: &TranscriptRoots,
) -> Result<ResolvedSource, TranscriptError> {
    if !filename_safe(id) {
        return Err(TranscriptError::SessionPointerInvalid {
            reason: "id_not_filename_safe",
        });
    }
    let name = format!("{id}.jsonl");
    let candidates = match cwd {
        Some(dir) => roots
            .project_dirs
            .iter()
            .map(|root| root.join(slug_of(dir)).join(&name))
            .collect(),
        None => glob_candidates(&roots.project_dirs, &name).await,
    };
    let mut existing = Vec::new();
    for cand in candidates {
        match fs::symlink_metadata(&cand).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(TranscriptError::SourceUnreadable {
                    code: "symlink_refused",
                });
            }
            Ok(_) => existing.push(cand),
            Err(_) => {}
        }
    }
    if let [cand] = existing.as_slice() {
        return open_source(cand, Format::ClaudeJsonl, Some(id)).await;
    }
    if existing.len() > 1 {
        return Err(TranscriptError::Ambiguous {
            candidates: existing.len(),
        });
    }
    // No candidate exists yet — open the pinned one anyway so the caller
    // sees the typed ENOENT; with no roots at all there is nothing to try.
    if roots.project_dirs.is_empty() {
        return Err(TranscriptError::Unreadable { reason: "no_roots" });
    }
    Err(TranscriptError::SourceUnreadable { code: "ENOENT" })
}

/// `<root>/*/<name>` — the candidate walk, in directory order. Missing or
/// unreadable roots contribute nothing.
async fn glob_candidates(roots: &[PathBuf], name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(mut entries) = fs::read_dir(root).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let cand = entry.path().join(name);
            if fs::symlink_metadata(&cand).await.is_ok() {
                found.push(cand);
            }
        }
    }
    found
}

/// Open `path` and pin its inode. The opened handle is the evidence;
/// open/read failures map to the typed `source_unreadable` codes.
async fn open_source(
    path: &Path,
    format: Format,
    expected_id: Option<&str>,
) -> Result<ResolvedSource, TranscriptError> {
    let file = File::open(path).await.map_err(|e| unreadable(&e))?;
    let ino = file.metadata().await.map_err(|e| unreadable(&e))?.ino();
    Ok(ResolvedSource {
        file: Mutex::new(file),
        path: path.to_path_buf(),
        ino,
        format,
        expected_id: expected_id.map(str::to_owned),
    })
}

/// The id a `<ts>_<id>.jsonl` session filename advertises — `None` when
/// the name doesn't conform (the header check then has nothing to
/// compare; the reference reader validated nothing at all).
fn path_id(path: &Path) -> Option<String> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let (_ts, id) = stem.split_once('_')?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_owned())
    }
}

/// The project slug a session cwd maps to: `/a/b` → `-a-b`
/// (`a5_native_default_locations`). ponytail: only `/`→`-` is
/// evidence-pinned; if the tool encodes other characters, resolution
/// misses and reports unreadable — extend here when a session hits it.
fn slug_of(cwd: &str) -> String {
    cwd.replace('/', "-")
}

/// A session id usable verbatim as a file name: nonempty, portable
/// characters only — traversal components (`../escape`) are refused as
/// `id_not_filename_safe`.
fn filename_safe(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}
