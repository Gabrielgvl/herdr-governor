//! `coordinator::reload` — the F27 `SIGHUP` arm's implementation (split
//! from `coordinator.rs` under the 500-line cap).

use crate::adapters::config::{self, ConfigLoadError};
use crate::daemon::log;

use super::Coordinator;

impl Coordinator {
    /// `SIGHUP` — `config::reload` against the last-good; on `Adopted` the
    /// `[daemon]` table must still be present (a daemonless reload would
    /// silently strand the daemon, so it retains instead). F7's
    /// `config.valid`/`lastError`/`lastGoodAt` track the same outcomes.
    pub(super) async fn reload(&mut self) {
        match config::reload(&self.loaded, &self.catalog_path).await {
            config::ReloadOutcome::Adopted(loaded) => {
                if let Some(daemon) = loaded.daemon.clone() {
                    let settings_changed = self.daemon != daemon;
                    log::config_adopted(&loaded.version.0);
                    self.daemon = daemon;
                    self.loaded = *loaded;
                    self.config_adopted_at = self.clock.now();
                    self.config_last_error = None;
                    if settings_changed {
                        // The spawned tasks captured their intervals at
                        // startup; a `[daemon]` change takes effect at
                        // the next restart (documented limitation) — the
                        // log says so rather than the false "retained".
                        log::config_daemon_deferred();
                    }
                } else {
                    // A daemonless catalog is a refused INVALID attempt —
                    // last-good stays live; F7 reports `invalid` (F5).
                    self.config_last_error = Some("invalid");
                    log::config_retained("missing-daemon");
                }
            }
            config::ReloadOutcome::Retained { error, .. } => {
                let class = match error {
                    ConfigLoadError::Read(_) => "read",
                    ConfigLoadError::Decode(_) => "decode",
                    ConfigLoadError::Invalid(_) => "invalid",
                };
                self.config_last_error = Some(class);
                log::config_retained(class);
            }
        }
    }
}
