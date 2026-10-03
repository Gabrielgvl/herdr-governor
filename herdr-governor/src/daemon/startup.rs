//! `startup` — the §4.3 bring-up sequence (F28): `prepare` runs steps 1–5's
//! synchronous half (state dir, catalog + `[daemon]` + credential, the
//! lock-then-unlink instance lock, `Store::open`) and `bind` runs the
//! listener half of step 8 (`UnixListener::bind` + `chmod 0600`, after
//! marking and before the tasks spawn — H#5's ordering). Step 5's restart
//! marking and steps 6–7 are coordinator transitions (`coordinator.rs`).
//! The stale-socket unlink happens inside `lock::acquire`, never here.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use tokio::net::UnixListener;

use crate::adapters::config::{self, DaemonSettings, LoadedConfig};
use crate::adapters::jev::ApiKey;
use crate::store::Store;

use super::DaemonError;
use super::lock::{self, InstanceLock};
use super::log;
use super::paths::Paths;
use super::settings::{Resolved, Settings};

/// The catalog filename inside the config dir.
pub(super) const CATALOG_NAME: &str = "catalog.toml";
/// The credential filename inside the config dir (spec §19).
const CREDENTIALS_NAME: &str = "credentials";

/// Everything `daemon::run` keeps after `prepare`: the paths, the held
/// lock, the open store, the loaded catalog (with its `[daemon]` table),
/// the argv-resolved overrides, the credential and the catalog path (for
/// `SIGHUP` reload).
pub(super) struct Prepared {
    /// The state-dir layout.
    pub paths: Paths,
    /// The held instance lock — dropped last, at teardown.
    pub lock: InstanceLock,
    /// The open SQLite store.
    pub store: Store,
    /// The validated catalog + `[daemon]`.
    pub loaded: LoadedConfig,
    /// `loaded.daemon` hoisted — `prepare` already refused its absence.
    pub daemon: DaemonSettings,
    /// The argv overrides resolved against `daemon`.
    pub resolved: Resolved,
    /// The Jev credential (loaded at step 2; held for the Jev callers).
    pub api_key: ApiKey,
    /// `<config>/catalog.toml` — the reload source.
    pub catalog_path: PathBuf,
}

/// §4.3 steps 1–4: layout the state dir `0700`; load `catalog.toml` and
/// refuse `ConfigLoadError` (exit 2); refuse an absent `[daemon]` table
/// the same way; load the `0600` credential; take the instance lock —
/// lock first, then the winner unlinks a stale socket; open the store.
pub(super) async fn prepare(settings: &Settings) -> Result<Prepared, DaemonError> {
    log::step("state");
    let paths = Paths::create(&settings.state_dir)?;
    log::state_dir(paths.state());

    log::step("config");
    let catalog_path = settings.config_dir.join(CATALOG_NAME);
    let loaded = config::load(&catalog_path).await?;
    let daemon = loaded.daemon.clone().ok_or(DaemonError::NoDaemonTable)?;
    let resolved = super::settings::resolve(settings, &daemon);
    let api_key = config::load_credentials(&settings.config_dir.join(CREDENTIALS_NAME))
        .await
        .map_err(DaemonError::Credential)?;

    log::step("lock");
    let (lock, removed) = lock::acquire(&paths)?;
    log::stale_socket_removed(removed);

    log::step("store");
    let store = Store::open(&paths.db())?;

    Ok(Prepared {
        paths,
        lock,
        store,
        loaded,
        daemon,
        resolved,
        api_key,
        catalog_path,
    })
}

/// §4.3 step 8's listener half — `UnixListener::bind` on the now-free
/// socket path, `chmod 0600`. Runs strictly after the lock and the
/// restart marking so the socket never appears before the store's
/// restart state is consistent (H#5).
pub(super) fn bind(paths: &Paths) -> Result<UnixListener, DaemonError> {
    log::step("bind");
    let listener = UnixListener::bind(paths.sock())?;
    let mut permissions = std::fs::metadata(paths.sock())?.permissions();
    permissions.set_mode(super::paths::FILE_MODE);
    std::fs::set_permissions(paths.sock(), permissions)?;
    Ok(listener)
}
