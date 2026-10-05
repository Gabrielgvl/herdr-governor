//! `fixture` — the on-disk world a test daemon runs against (P5.T1):
//! `Catalog` renders a valid `catalog.toml` (the shortest useful
//! windows by default; `inert` when the tick must never fire),
//! `fixture` writes it plus the `0600` credential under a fresh
//! `DaemonDirs` tempdir, and `write_fixture` lays the same files at an
//! explicit config dir (the env-defaults test's controlled roots).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use herdr_governor::daemon::Settings;
use tempfile::TempDir;

/// The `catalog.toml` content for a test daemon: a valid `[policy]` +
/// `[catalog]` + `[daemon]` file where every window defaults to the
/// shortest useful value (`reconcile_secs` 1 — the tests' fast tick).
/// `herdr_socket` and `jev_base_url` name the fakes.
#[derive(Debug)]
pub struct Catalog {
    /// `[daemon].herdr_socket` — the fake Herdr socket.
    pub herdr_socket: PathBuf,
    /// `[daemon].jev_base_url` — the fake Jev base.
    pub jev_base_url: String,
    /// `[daemon].jev_model`.
    pub jev_model: String,
    /// `[daemon].reconcile_secs`.
    pub reconcile_secs: u64,
    /// `[policy].cooldown_secs` — the F27 reload knob: bumping it moves
    /// the adopted config digest.
    pub cooldown_secs: u64,
    /// `[policy].tiers` — the choice questions' label set.
    pub tiers: Vec<String>,
    /// The `[catalog]` member, verbatim TOML — `operating_points = []`
    /// when the test has no operating points.
    pub points_toml: String,
    /// Extra `[daemon]` keys, verbatim TOML lines
    /// (`shutdown_grace_secs = 1`, `retire_*`, `transcript_dir`,
    /// timeouts).
    pub daemon_extra: String,
    /// Extra `[policy]` keys, verbatim TOML lines (`judgment_window_secs
    /// = 3`, `repair_window_secs`, `idle_window_secs`) — the supervision
    /// suites' short windows.
    pub policy_extra: String,
}

impl Catalog {
    /// The minimal valid catalog: no operating points, one `fast` tier,
    /// a 1-second reconcile tick.
    #[must_use]
    pub fn new(herdr_socket: &Path, jev_base_url: &str) -> Self {
        Self {
            herdr_socket: herdr_socket.to_path_buf(),
            jev_base_url: jev_base_url.to_owned(),
            jev_model: "jev-fake".to_owned(),
            reconcile_secs: 1,
            cooldown_secs: 60,
            tiers: vec!["fast".to_owned()],
            points_toml: "operating_points = []".to_owned(),
            daemon_extra: String::new(),
            policy_extra: String::new(),
        }
    }

    /// `new` with an inert tick (`reconcile_secs = 3600`): snapshots
    /// happen only at request time — the bring-up/shutdown suites'
    /// cadence.
    #[must_use]
    pub fn inert(herdr_socket: &Path, jev_base_url: &str) -> Self {
        Self {
            reconcile_secs: 3600,
            ..Self::new(herdr_socket, jev_base_url)
        }
    }

    /// The `catalog.toml` body.
    #[must_use]
    pub fn toml(&self) -> String {
        let tiers = self
            .tiers
            .iter()
            .map(|tier| format!("\"{tier}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[policy]\ntiers = [{tiers}]\nprovider_limit_threshold = 0.6\n\
             cooldown_secs = {cooldown}\n{policy_extra}\n[catalog]\n{points}\n\n[daemon]\n\
             herdr_socket = \"{sock}\"\njev_base_url = \"{jev}\"\n\
             jev_model = \"{model}\"\nreconcile_secs = {reconcile}\n{extra}",
            cooldown = self.cooldown_secs,
            policy_extra = self.policy_extra,
            points = self.points_toml,
            sock = self.herdr_socket.display(),
            jev = self.jev_base_url,
            model = self.jev_model,
            reconcile = self.reconcile_secs,
            extra = self.daemon_extra,
        )
    }
}

/// The state + config dirs plus the `Settings` pointing at them; the
/// `TempDir` is owned here so the fixture cleans itself up — keep this
/// value alive as long as the daemon it built.
#[derive(Debug)]
pub struct DaemonDirs {
    root: TempDir,
    state: PathBuf,
    config: PathBuf,
}

impl DaemonDirs {
    /// The state/config tempdir root — also the natural throwaway
    /// `projectRoot` for a caller envelope.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// `Settings` for `TestDaemon::*` — the argv overrides stay `None`
    /// (the catalog carries every window).
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings {
            state_dir: self.state.clone(),
            config_dir: self.config.clone(),
            herdr_socket: None,
            reconcile_secs: None,
        }
    }

    /// `<state>` — the dir `daemon::run` fills.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// `<config>` — where the catalog + credentials live.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// `<state>/governor.sock` — the MCP socket once bound.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.state.join("governor.sock")
    }

    /// `<state>/governor.db` — the store the test seeds and reads back.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.state.join("governor.db")
    }
}

/// Write `catalog.toml` + the `0600` credential into `config_dir` —
/// the on-disk half of `fixture`, exported for tests that place the
/// config at a path they control (the HOME/XDG env matrix).
pub fn write_fixture(config_dir: &Path, catalog: &Catalog) {
    std::fs::create_dir_all(config_dir).expect("config dir");
    std::fs::write(config_dir.join("catalog.toml"), catalog.toml()).expect("catalog");
    let credentials = config_dir.join("credentials");
    std::fs::write(&credentials, "test-token\n").expect("credentials");
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600))
        .expect("credentials mode");
}

/// Write `root/{state,config}` under a fresh tempdir: `catalog.toml`
/// from `catalog` and the `0600` credential `daemon::run` requires.
#[must_use]
pub fn fixture(catalog: &Catalog) -> DaemonDirs {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let config = root.path().join("config");
    std::fs::create_dir_all(&state).expect("state dir");
    write_fixture(&config, catalog);
    DaemonDirs {
        root,
        state,
        config,
    }
}
