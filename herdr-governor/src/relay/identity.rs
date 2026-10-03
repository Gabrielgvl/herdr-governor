//! `relay/identity` — the F1 derivation (ADR-0004, spec F1): `paneId`
//! from the inherited `HERDR_PANE_ID`, `projectRoot` as realpath of the
//! git toplevel or the invocation cwd, and a `relayInstanceId` minted per
//! process. The relay never fabricates: an absent pane id goes out as
//! `""` for the daemon to refuse `CALLER_IDENTITY_INVALID`, and a root
//! the daemon cannot validate is refused, never re-anchored (H#3).

use std::env;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// Fatal derivations only: without an instance id or a cwd the relay can
/// produce no legal envelope at all.
#[derive(Debug, Error)]
pub(super) enum IdentityError {
    /// `/dev/urandom` unreadable — no `relayInstanceId` exists to mint.
    #[error("cannot mint relayInstanceId: {0}")]
    Entropy(#[source] std::io::Error),
    /// `current_dir` failed — the invocation cwd is gone.
    #[error("cannot resolve the invocation cwd: {0}")]
    Cwd(#[source] std::io::Error),
}

/// The derived F1 caller envelope fields, verbatim wire strings.
#[derive(Debug)]
pub(super) struct Identity {
    /// `HERDR_PANE_ID`, `""` when absent — the daemon's refusal, never a
    /// fabricated pane.
    pub pane_id: String,
    /// realpath of `git rev-parse --show-toplevel`, else of the cwd.
    pub project_root: String,
    /// 128-bit random id, lowercase hex, minted at process start.
    pub relay_instance_id: String,
}

/// Derive once at process start: the id is minted here, held only in
/// memory and never persisted or configurable (ADR-0004).
pub(super) fn derive() -> Result<Identity, IdentityError> {
    let cwd = env::current_dir().map_err(IdentityError::Cwd)?;
    Ok(Identity {
        pane_id: env::var("HERDR_PANE_ID").unwrap_or_default(),
        project_root: project_root(&cwd),
        relay_instance_id: mint().map_err(IdentityError::Entropy)?,
    })
}

/// Sixteen bytes of `/dev/urandom` rendered lowercase hex — the ADR-0004
/// 128-bit wire form `validate_caller_envelope` pins.
fn mint() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ignored = write!(out, "{byte:02x}");
            out
        }))
}

/// `projectRoot` (spec F1): realpath of the git toplevel when the cwd
/// sits inside a worktree, else realpath of the cwd itself.
fn project_root(cwd: &Path) -> String {
    realpath(&git_toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf()))
}

/// `git -C <cwd> rev-parse --show-toplevel` as an argv array (no shell)
/// with `GIT_OPTIONAL_LOCKS=0`. `None` on every failure — spawn error,
/// non-zero exit, non-UTF-8 or empty output — so the cwd fallback
/// applies. Not inside a worktree is one such non-zero exit.
fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = text.trim_end_matches(['\r', '\n']);
    if root.is_empty() {
        return None;
    }
    Some(PathBuf::from(root))
}

/// `fs::canonicalize` — the realpath form the daemon's `projectRoot`
/// check pins. A path it cannot resolve goes out literal for the daemon
/// to refuse, and a root the wire cannot represent (a non-UTF-8 name)
/// goes out `""` — the refused caller, never a lossy substitute that
/// could canonicalize onto a different real directory (H#3).
fn realpath(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_str()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{mint, realpath};

    /// The mint is random and shaped exactly as the daemon validates.
    #[test]
    fn relay_instance_id_is_32_lowercase_hex_and_random() {
        let first = mint().expect("mint");
        let second = mint().expect("mint");
        for id in [&first, &second] {
            assert_eq!(id.len(), 32, "128-bit id is 32 hex chars: {id}");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "lowercase hex only: {id}",
            );
        }
        assert_ne!(first, second, "each mint is independent");
    }

    /// F1/H#3 — a root the wire cannot represent is refused, never
    /// substituted: a non-UTF-8 directory name goes out as `""` for the
    /// daemon to refuse `CALLER_IDENTITY_INVALID` — a lossy spelling
    /// could canonicalize onto a different real directory and attach it.
    #[test]
    fn realpath_refuses_a_non_utf8_root() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let raw = dir.path().join(OsStr::from_bytes(b"root-\xff"));
        std::fs::create_dir_all(&raw).expect("non-utf8 dir");
        assert_eq!(
            realpath(&raw),
            "",
            "an unrepresentable root derives as refused — never lossy"
        );
    }
}
