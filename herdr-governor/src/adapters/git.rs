//! `git` — worktree evidence (`base_commit`, dirty paths) through argv-only
//! `git` invocations (spec §10, the P4.G1 contract): async over
//! `tokio::process` (OQ-9), never a shell string — a hostile path or branch
//! name is one argv element and can never become a command — and typed
//! failure, never an empty string. The digest over the evidence is the
//! caller's job through governor-core; this adapter returns verbatim
//! strings.
//!
//! Exit-code contract (probed against git 2.43): `rev-parse --verify
//! --quiet HEAD` is `0` + sha, `1` silent on an unborn HEAD, `128` with a
//! `fatal:` prefix outside a repository; `status --porcelain=v1 -z` emits
//! `XY path\0`, and `XY new\0old\0` for renames/copies, filenames verbatim.

use std::io;
use std::path::Path;
use std::process::Output;

use thiserror::Error;
use tokio::process::Command;

/// The `git` program name — resolved on `PATH` by the OS, never a shell.
const GIT: &str = "git";

/// What the worktree looked like at one read: `head` is the full commit id
/// `HEAD` resolves to, `dirty` every path `git status` reports as changed
/// or untracked, verbatim and in git's order (a rename contributes its new
/// path then its old one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidence {
    /// Full hex commit id of `HEAD`.
    pub head: String,
    /// Changed and untracked paths, relative to the worktree root.
    pub dirty: Vec<String>,
}

/// Every way an evidence read can fail, typed.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum GitError {
    /// The `git` binary could not be spawned (missing or not executable).
    #[error("git binary unavailable: {0}")]
    Unavailable(#[source] io::Error),
    /// `root` is not inside a git repository (or does not exist).
    #[error("not a git repository")]
    NotARepo,
    /// The repository has no commit yet: `HEAD` is unborn.
    #[error("HEAD is unborn: no commit to pin")]
    UnbornHead,
    /// `git` exited non-zero for a reason this adapter does not classify.
    #[error("git failed with status {status:?}: {stderr}")]
    Failed {
        /// Exit code, `None` when killed by a signal.
        status: Option<i32>,
        /// Trimmed stderr, verbatim.
        stderr: String,
    },
    /// `git` succeeded but its output is not what the contract pins.
    #[error("git output malformed: {reason}")]
    Malformed {
        /// `head_not_hex` | `truncated_entry` | `non_utf8_path` |
        /// `missing_rename_source`.
        reason: &'static str,
    },
    /// `HEAD` resolved differently before and after the status read, so
    /// `dirty` is relative to an unknown base. The caller re-reads.
    #[error("HEAD moved during the read: {before} -> {after}")]
    HeadMoved {
        /// `HEAD` before `git status`.
        before: String,
        /// `HEAD` after `git status`.
        after: String,
    },
}

/// The commit `HEAD` resolves to in the worktree at `root`.
pub async fn base_commit(root: &Path) -> Result<String, GitError> {
    head(GIT, root).await
}

/// `HEAD` plus the dirty set of the worktree at `root`. `HEAD` is read on
/// both sides of the status read; a move between them is `HeadMoved`.
pub async fn worktree_evidence(root: &Path) -> Result<GitEvidence, GitError> {
    let before = head(GIT, root).await?;
    let dirty = dirty_paths(GIT, root).await?;
    let after = head(GIT, root).await?;
    evidence(before, dirty, after)
}

/// Composes one evidence read; pure so the mid-read guard is unit-tested.
fn evidence(before: String, dirty: Vec<String>, after: String) -> Result<GitEvidence, GitError> {
    if before == after {
        Ok(GitEvidence {
            head: before,
            dirty,
        })
    } else {
        Err(GitError::HeadMoved { before, after })
    }
}

async fn head(program: &str, root: &Path) -> Result<String, GitError> {
    let out = run(program, root, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
    match out.status.code() {
        Some(0) => parse_head(&out.stdout),
        // `--quiet`: unborn HEAD is exit 1 with nothing on stderr.
        Some(1) if out.stderr.is_empty() => Err(GitError::UnbornHead),
        _ => Err(classify_failure(&out)),
    }
}

async fn dirty_paths(program: &str, root: &Path) -> Result<Vec<String>, GitError> {
    let out = run(
        program,
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    if out.status.success() {
        parse_porcelain(&out.stdout)
    } else {
        Err(classify_failure(&out))
    }
}

/// One argv-array `git` invocation rooted at `root` via `-C` — `root` and
/// every argument travel as discrete argv elements, so no quoting layer
/// exists for them to escape. `GIT_OPTIONAL_LOCKS=0` keeps `status` from
/// writing the index (an evidence read never mutates the agent's repo).
async fn run(program: &str, root: &Path, args: &[&str]) -> Result<Output, GitError> {
    Command::new(program)
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(GitError::Unavailable)
}

fn classify_failure(out: &Output) -> GitError {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    // "fatal: not a git repository (or any of the parent directories)";
    // "fatal: cannot change to '<root>': No such file or directory".
    if stderr.starts_with("fatal: not a git repository")
        || stderr.starts_with("fatal: cannot change to")
    {
        GitError::NotARepo
    } else {
        GitError::Failed {
            status: out.status.code(),
            stderr,
        }
    }
}

fn parse_head(stdout: &[u8]) -> Result<String, GitError> {
    let text = std::str::from_utf8(stdout).map_err(|_utf8| GitError::Malformed {
        reason: "head_not_hex",
    })?;
    let sha = text.trim_end();
    let hex = sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit());
    if hex {
        Ok(sha.to_owned())
    } else {
        Err(GitError::Malformed {
            reason: "head_not_hex",
        })
    }
}

/// Parses `status --porcelain=v1 -z`: NUL-separated `XY<space>path`
/// entries; an `R`/`C` status is followed by one extra NUL-terminated entry,
/// the source path. Paths are verbatim bytes — spaces and newlines included
/// — and must be UTF-8 to become `String`s.
fn parse_porcelain(stdout: &[u8]) -> Result<Vec<String>, GitError> {
    let mut dirty = Vec::new();
    let mut entries = stdout.split(|&b| b == 0).peekable();
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            // Only the terminator after the last NUL may be empty.
            if entries.peek().is_some() {
                return Err(GitError::Malformed {
                    reason: "truncated_entry",
                });
            }
            break;
        }
        let (status, path) = match (entry.get(..3), entry.get(3..)) {
            (Some([x, _, b' ']), Some(path)) if !path.is_empty() => (*x, path),
            _ => {
                return Err(GitError::Malformed {
                    reason: "truncated_entry",
                });
            }
        };
        dirty.push(utf8_path(path)?);
        if status == b'R' || status == b'C' {
            let source = entries
                .next()
                .filter(|s| !s.is_empty())
                .ok_or(GitError::Malformed {
                    reason: "missing_rename_source",
                })?;
            dirty.push(utf8_path(source)?);
        }
    }
    Ok(dirty)
}

fn utf8_path(bytes: &[u8]) -> Result<String, GitError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_utf8| GitError::Malformed {
            reason: "non_utf8_path",
        })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{
        GitError, GitEvidence, base_commit, evidence, head, parse_head, parse_porcelain,
        worktree_evidence,
    };

    /// Throwaway repository under a tempdir, isolated from the user's git
    /// config. Never touches this worktree's own repository.
    fn repo() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        (dir, root)
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(root: &Path, msg: &str) -> String {
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "--allow-empty", "-m", msg]);
        git(root, &["rev-parse", "HEAD"])
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[tokio::test]
    async fn clean_repo_yields_head_and_empty_dirty_set() {
        let (_dir, root) = repo();
        let sha = commit(&root, "init");
        let ev = worktree_evidence(&root).await.unwrap();
        assert_eq!(
            ev,
            GitEvidence {
                head: sha.clone(),
                dirty: vec![]
            },
            "clean worktree"
        );
        assert_eq!(
            base_commit(&root).await.unwrap(),
            sha,
            "base_commit matches HEAD"
        );
    }

    #[tokio::test]
    async fn dirty_repo_lists_modified_untracked_and_renamed_paths_verbatim() {
        let (_dir, root) = repo();
        write(&root, "d i r/a b.txt", "x");
        write(&root, "n\nl.txt", "x");
        write(&root, "keep.txt", "x");
        let sha = commit(&root, "files");
        git(&root, &["mv", "d i r/a b.txt", "c d.txt"]);
        write(&root, "n\nl.txt", "y");
        write(&root, "new dir/u.txt", "z");
        let ev = worktree_evidence(&root).await.unwrap();
        assert_eq!(ev.head, sha, "head unchanged by dirt");
        assert_eq!(
            ev.dirty,
            vec!["c d.txt", "d i r/a b.txt", "n\nl.txt", "new dir/u.txt"],
            "rename new+old, newline path, untracked file inside dir"
        );
    }

    #[tokio::test]
    async fn unborn_head_is_typed() {
        let (_dir, root) = repo();
        assert!(
            matches!(base_commit(&root).await, Err(GitError::UnbornHead)),
            "unborn"
        );
        assert!(
            matches!(worktree_evidence(&root).await, Err(GitError::UnbornHead)),
            "unborn"
        );
    }

    #[tokio::test]
    async fn not_a_repo_and_missing_root_are_typed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            matches!(base_commit(dir.path()).await, Err(GitError::NotARepo)),
            "plain dir"
        );
        assert!(
            matches!(worktree_evidence(dir.path()).await, Err(GitError::NotARepo)),
            "dir"
        );
        let missing = dir.path().join("nope");
        assert!(
            matches!(base_commit(&missing).await, Err(GitError::NotARepo)),
            "missing"
        );
    }

    #[tokio::test]
    async fn missing_git_binary_is_unavailable() {
        let (_dir, root) = repo();
        let err = head("herdr-governor-no-such-git-binary", &root)
            .await
            .unwrap_err();
        assert!(
            matches!(err, GitError::Unavailable(_)),
            "spawn failure: {err}"
        );
    }

    #[tokio::test]
    async fn hostile_root_is_one_argv_element_not_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pwned");
        let hostile = dir.path().join(format!("x; touch {}", marker.display()));
        let err = base_commit(&hostile).await.unwrap_err();
        assert!(matches!(err, GitError::NotARepo), "{err}");
        assert!(!marker.exists(), "no shell ran the injected command");
    }

    #[tokio::test]
    async fn head_moved_mid_read_is_typed_not_panic() {
        let (_dir, root) = repo();
        let first = commit(&root, "one");
        let before = base_commit(&root).await.unwrap();
        let second = commit(&root, "two");
        let after = base_commit(&root).await.unwrap();
        assert_eq!(
            (before.as_str(), after.as_str()),
            (first.as_str(), second.as_str()),
            "moved"
        );
        let err = evidence(first.clone(), vec![], second.clone()).unwrap_err();
        assert!(
            matches!(&err, GitError::HeadMoved { before: b, after: a } if *b == first && *a == second),
            "{err}"
        );
        assert_eq!(
            evidence(first.clone(), vec!["a".into()], first.clone()).unwrap(),
            GitEvidence {
                head: first,
                dirty: vec!["a".into()]
            },
            "stable head composes"
        );
    }

    #[test]
    fn porcelain_parse_handles_renames_spaces_newlines_and_untracked() {
        let raw = b"R  c d.txt\0d i r/a b.txt\0 M n\nl.txt\0?? u.txt\0C  cp.txt\0src.txt\0";
        assert_eq!(
            parse_porcelain(raw).unwrap(),
            vec![
                "c d.txt",
                "d i r/a b.txt",
                "n\nl.txt",
                "u.txt",
                "cp.txt",
                "src.txt"
            ],
            "every entry, verbatim"
        );
        assert_eq!(
            parse_porcelain(b"").unwrap(),
            Vec::<String>::new(),
            "empty output"
        );
    }

    #[test]
    fn porcelain_parse_rejects_malformed_output() {
        let reason = |raw: &[u8]| match parse_porcelain(raw) {
            Err(GitError::Malformed { reason }) => reason,
            other => panic!("expected Malformed, got {other:?}"),
        };
        assert_eq!(
            reason(b"R  new.txt\0"),
            "missing_rename_source",
            "rename without source"
        );
        assert_eq!(reason(b"M\0"), "truncated_entry", "no status/path split");
        assert_eq!(reason(b" M \0"), "truncated_entry", "empty path");
        assert_eq!(
            reason(b" M a\0\0?? b\0"),
            "truncated_entry",
            "empty entry mid-stream"
        );
        assert_eq!(reason(b"?? \xff\0"), "non_utf8_path", "non-UTF-8 path");
    }

    #[test]
    fn head_parse_requires_full_hex_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_head(format!("{sha}\n").as_bytes()).unwrap(),
            sha,
            "trailing newline"
        );
        for bad in [
            "",
            "HEAD\n",
            "0123456",
            "0123456789abcdef0123456789abcdef0123456g",
        ] {
            assert!(
                matches!(
                    parse_head(bad.as_bytes()),
                    Err(GitError::Malformed {
                        reason: "head_not_hex"
                    })
                ),
                "{bad:?}"
            );
        }
    }
}
