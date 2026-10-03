//! `paths` — the state/config directory layout (§4.15): the state dir is
//! created `0700` and carries `governor.db`, `governor.sock`, `lock` plus
//! the `handoffs/`, `frozen/`, `followups/` subtrees the later lanes write
//! into. Subdirs are created eagerly so the layout and its modes are pinned
//! in one place at startup, not by whichever writer runs first.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// Unix `0700` — owner rwx only.
const DIR_MODE: u32 = 0o700;
/// Unix `0600` — owner rw only (the lock file).
pub(super) const FILE_MODE: u32 = 0o600;

/// The state dir layout. Every accessor is derived — nothing caches a
/// caller-supplied filename.
#[derive(Debug, Clone)]
pub(super) struct Paths {
    state: PathBuf,
}

/// Create `dir` (and the `0700` parent) if missing and pin its mode —
/// a pre-existing dir keeps its name but takes the `0700` the spec pins.
fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(DIR_MODE);
    fs::set_permissions(path, permissions)
}

/// The default state dir (OQ-F/G decided): `$XDG_STATE_HOME/herdr-governor`
/// or `~/.local/state/herdr-governor`. `None` when neither env names a home.
pub(super) fn default_state_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(xdg) if !xdg.is_empty() => Some(PathBuf::from(xdg).join("herdr-governor")),
        _ => std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("herdr-governor")
        }),
    }
}

/// The default config dir (§4.15): `$XDG_CONFIG_HOME/herdr-governor` or
/// `~/.config/herdr-governor`.
pub(super) fn default_config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() => Some(PathBuf::from(xdg).join("herdr-governor")),
        _ => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".config").join("herdr-governor")),
    }
}

impl Paths {
    /// Resolve `state` into the layout and materialize it: the state dir
    /// and the three artifact subtrees, all `0700` (§4.15). The subtree
    /// names come from the accessors below — the layout is declared once.
    pub(super) fn create(state: &Path) -> io::Result<Self> {
        ensure_dir(state)?;
        let paths = Self {
            state: state.to_path_buf(),
        };
        for dir in [paths.handoffs(), paths.frozen(), paths.followups()] {
            ensure_dir(&dir)?;
        }
        Ok(paths)
    }

    /// The state dir itself.
    pub(super) fn state(&self) -> &Path {
        &self.state
    }

    /// `<state>/governor.db` — the SQLite lifecycle store.
    pub(super) fn db(&self) -> PathBuf {
        self.state.join("governor.db")
    }

    /// `<state>/governor.sock` — the MCP listener's Unix socket.
    pub(super) fn sock(&self) -> PathBuf {
        self.state.join("governor.sock")
    }

    /// `<state>/lock` — the single-instance `flock` file.
    pub(super) fn lock(&self) -> PathBuf {
        self.state.join("lock")
    }

    /// `<state>/handoffs/` — accepted handoff markdown (`<runId>/handoff.md`).
    pub(super) fn handoffs(&self) -> PathBuf {
        self.state.join("handoffs")
    }

    /// `<state>/frozen/` — frozen handoff payloads (`<runId>/<wg>-<digest16>.md`).
    pub(super) fn frozen(&self) -> PathBuf {
        self.state.join("frozen")
    }

    /// `<state>/followups/` — follow-up message bodies (`<runId>/<seq>.md`).
    pub(super) fn followups(&self) -> PathBuf {
        self.state.join("followups")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::{FILE_MODE, Paths, default_config_dir, default_state_dir};

    fn mode(path: &std::path::Path) -> u32 {
        fs::metadata(path).expect("mode").permissions().mode() & 0o777
    }

    /// `create` materializes the layout `0700` — state dir and the three
    /// artifact subtrees — and pins a lax pre-existing dir down to `0700`.
    #[test]
    fn paths_create_layouts_0700_and_pins_existing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let state = tmp.path().join("state");
        fs::create_dir_all(&state).expect("mkdir");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).expect("chmod");
        let paths = Paths::create(&state).expect("create");
        for dir in [
            paths.state(),
            &paths.handoffs(),
            &paths.frozen(),
            &paths.followups(),
        ] {
            assert_eq!(mode(dir), 0o700, "0700 dir {}", dir.display());
        }
        assert_eq!(paths.db(), state.join("governor.db"));
        assert_eq!(paths.sock(), state.join("governor.sock"));
        assert_eq!(paths.lock(), state.join("lock"));
        // The file modes the layout pins.
        assert_eq!(FILE_MODE, 0o600);
    }

    /// Defaults resolve under XDG or the `~/.local/state` + `~/.config`
    /// fallbacks — the functions read env, so assert the shape that holds
    /// for every value of HOME/XDG.
    #[test]
    fn paths_defaults_end_in_herdr_governor() {
        for dir in [default_state_dir(), default_config_dir()]
            .into_iter()
            .flatten()
        {
            assert_eq!(dir.file_name().unwrap(), "herdr-governor");
        }
    }
}
