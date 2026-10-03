//! `raw/daemon` — the `[daemon]` table's DTO (§4.15, OQ-E), split
//! out of `raw.rs` for the 500-line hygiene bound. Same rules as the
//! sibling tables: `deny_unknown_fields`, bare-second durations, and
//! `default` fns named by the serde attributes, resolved in this module.

use core::time::Duration;
use std::path::PathBuf;

use serde::Deserialize;

use crate::adapters::config::DaemonSettings;

/// `[daemon]` — the daemon's own settings (§4.15, OQ-E). The same
/// required-vs-defaulted rule as `[policy]` applies: `herdr_socket`,
/// `jev_base_url` and `jev_model` name no spec default → required; every
/// timeout/interval decodes to the spec's written default; the three
/// `Option`s are absent-means-derive (`TranscriptRoots::from_env` for the
/// transcript roots, the harness data dir for `devin_log_dir`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDaemon {
    /// The Herdr socket the daemon drives — required.
    herdr_socket: PathBuf,
    /// The Jev API base URL — required.
    jev_base_url: String,
    /// The Jev model id — required.
    jev_model: String,
    /// Jev call deadline, seconds (default 20).
    #[serde(default = "default_jev_timeout_secs")]
    jev_timeout_secs: u64,
    /// One Herdr op deadline, seconds (default 10).
    #[serde(default = "default_herdr_op_timeout_secs")]
    herdr_op_timeout_secs: u64,
    /// `agent_start` deadline, milliseconds (default 30000).
    #[serde(default = "default_agent_start_timeout_ms")]
    agent_start_timeout_ms: u64,
    /// Reconcile tick interval, seconds (default 30).
    #[serde(default = "default_reconcile_secs")]
    reconcile_secs: u64,
    /// Deadline-review sweep interval, seconds (default 300).
    #[serde(default = "default_review_interval_secs")]
    review_interval_secs: u64,
    /// `herdr_launch` wait bound, seconds (default 60).
    #[serde(default = "default_launch_wait_secs")]
    launch_wait_secs: u64,
    /// §4.14 in-flight receipt bound, seconds (default 10).
    #[serde(default = "default_shutdown_grace_secs")]
    shutdown_grace_secs: u64,
    /// F30 — retirement is owner-default-on; `false` journals
    /// `would_retire` observations and closes nothing (default true).
    #[serde(default = "default_retire_enabled")]
    retire_enabled: bool,
    /// F30 — continuous idle/done + unchanged screen before a close,
    /// seconds (default 900).
    #[serde(default = "default_retire_grace_secs")]
    retire_grace_secs: u64,
    /// F31 — absent ⇒ `TranscriptRoots::from_env`'s data roots.
    transcript_data_dirs: Option<Vec<PathBuf>>,
    /// F31 — absent ⇒ `TranscriptRoots::from_env`'s project roots.
    transcript_project_dirs: Option<Vec<PathBuf>>,
    /// F31 — absent ⇒ the harness log dir under the data root.
    devin_log_dir: Option<PathBuf>,
}

impl RawDaemon {
    /// Map the DTO onto `DaemonSettings`: `*_secs`/`*_ms` become
    /// `Duration`s; the `Option` overrides pass through.
    pub(super) fn into_settings(self) -> DaemonSettings {
        DaemonSettings {
            herdr_socket: self.herdr_socket,
            jev_base_url: self.jev_base_url,
            jev_model: self.jev_model,
            jev_timeout: Duration::from_secs(self.jev_timeout_secs),
            herdr_op_timeout: Duration::from_secs(self.herdr_op_timeout_secs),
            agent_start_timeout: Duration::from_millis(self.agent_start_timeout_ms),
            reconcile: Duration::from_secs(self.reconcile_secs),
            review_interval: Duration::from_secs(self.review_interval_secs),
            launch_wait: Duration::from_secs(self.launch_wait_secs),
            shutdown_grace: Duration::from_secs(self.shutdown_grace_secs),
            retire_enabled: self.retire_enabled,
            retire_grace: Duration::from_secs(self.retire_grace_secs),
            transcript_data_dirs: self.transcript_data_dirs,
            transcript_project_dirs: self.transcript_project_dirs,
            devin_log_dir: self.devin_log_dir,
        }
    }
}

fn default_jev_timeout_secs() -> u64 {
    20
}

fn default_herdr_op_timeout_secs() -> u64 {
    10
}

fn default_agent_start_timeout_ms() -> u64 {
    30_000
}

fn default_reconcile_secs() -> u64 {
    30
}

fn default_review_interval_secs() -> u64 {
    300
}

fn default_launch_wait_secs() -> u64 {
    60
}

fn default_shutdown_grace_secs() -> u64 {
    10
}

fn default_retire_enabled() -> bool {
    true
}

fn default_retire_grace_secs() -> u64 {
    900
}
