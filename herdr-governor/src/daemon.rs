//! `daemon` — the Phase-5 daemon: one current-thread task owning every
//! lifecycle transition (§4.2), a `0600` Unix socket (ADR-0004's daemon
//! transport), the single-instance lock, and the `catalog.toml` +
//! `[daemon]` + credential foundation. `run` is the entry point: it
//! brings up steps 1–8 in order and serves until a signal or the
//! optional `shutdown` oneshot — the same function tests drive
//! in-process.
//!
//! Module layout: `api` (the M1 tool-value contract), `identity`/`status`
//! (F1's caller resolution + binding, F7's paged status), `settings`/`seam`
//! (argv + the fault seam), `paths`/`lock` (state dir + single instance),
//! `clock`/`ids` (one epoch read + all entropy), `log` (the safe tracing
//! surface), `startup`/`serve`/`shutdown` (bring-up, the reconcile tick,
//! teardown — the MCP listener is `mcp::serve`) and `coordinator` (the
//! store owner).

pub mod api;
pub mod identity;
pub mod ids;
pub mod status;

mod clock;
mod coordinator;
mod lock;
mod log;
mod paths;
mod seam;
mod serve;
mod settings;
mod shutdown;
mod startup;

#[cfg(test)]
mod tests;

use std::io;
use std::process::ExitCode;

use tokio::sync::{mpsc, oneshot};

pub use seam::{Boundary, SeamAction, SeamConfig, SeamError};
pub use settings::Settings;

/// `mcp::serve`'s connection tasks post `Msg::Tool` here — the mailbox
/// itself stays `mod coordinator`-private.
pub(crate) use coordinator::Msg;

use settings::{parse_check_config_args, parse_daemon_args};

use governor_core::config::ConfigVersion;

use crate::adapters::config::{self, ConfigLoadError};
use crate::adapters::jev::JevError;
use crate::store::{ApplyError, StoreError};

use coordinator::{Coordinator, CoordinatorArgs};
use lock::LockError;
use startup::Prepared;

/// The longest stderr line the daemon ever emits (§4.3 step 2's `≤ 500
/// chars` startup refusal): one line, no secret text, truncated hard.
const MAX_STDERR_LINE: usize = 500;

/// One sanitized stderr line: the first line of `message`, control
/// characters stripped, capped at 500 chars (§4.3 step 2; F27's startup
/// refusal). Never includes a secret — callers pass only typed errors.
#[must_use]
pub fn sanitize(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    let cleaned: String = first.chars().filter(|c| !c.is_control()).collect();
    cleaned.chars().take(MAX_STDERR_LINE).collect()
}

/// Why `run`/`check_config` refused — the exit code rides on the variant
/// (2: usage/config/refusal, 3: lock held, 1: everything else).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonError {
    /// Bad argv — an unknown flag, a missing value, a malformed `--seam`,
    /// or no way to derive a required path.
    #[error("{0}")]
    Usage(String),
    /// `catalog.toml` failed `config::load` (read, decode or validate).
    #[error("{0}")]
    Config(#[from] ConfigLoadError),
    /// `catalog.toml` loaded but carries no `[daemon]` table — the daemon
    /// has nothing to serve with.
    #[error("catalog.toml has no [daemon] table")]
    NoDaemonTable,
    /// The `credentials` file failed `load_credentials` (spec §19).
    #[error("credential unreadable: {0}")]
    Credential(#[from] JevError),
    /// A second daemon holds the state-dir lock — `answers` reports
    /// whether the probe found a live listener behind it.
    #[error("another daemon holds the state-dir lock (socket answers: {answers})")]
    Locked {
        /// Whether `<state>/governor.sock` answered the probe.
        answers: bool,
    },
    /// A store open/read failure at startup.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// A restart-marking apply failed (not a CAS drop — a hard error).
    #[error("apply: {0}")]
    Apply(#[from] ApplyError),
    /// Any other I/O failure (state dir, socket bind, teardown).
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The `--seam`/`GOV_DAEMON_SEAM` spec was malformed.
    #[error("seam: {0}")]
    Seam(#[from] SeamError),
}

impl DaemonError {
    /// A `Usage`-shaped error from a message.
    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    /// The process exit code for this error: 2 for usage/config/
    /// credential/`NoDaemonTable`/`Seam` refusals, 3 for lock-held, 1 for
    /// the rest (§4.3 step 2's contract).
    #[must_use]
    pub fn code(&self) -> ExitCode {
        use std::process::ExitCode as Code;
        match self {
            Self::Usage(_)
            | Self::Config(_)
            | Self::NoDaemonTable
            | Self::Credential(_)
            | Self::Seam(_) => Code::from(2),
            Self::Locked { .. } => Code::from(3),
            Self::Store(_) | Self::Apply(_) | Self::Io(_) => Code::from(1),
        }
    }
}

impl From<LockError> for DaemonError {
    fn from(err: LockError) -> Self {
        match err {
            LockError::Held { answers } => Self::Locked { answers },
            LockError::Io(io_err) => Self::Io(io_err),
        }
    }
}

/// What `check-config` prints on success — the catalog's content digest
/// and table sizes (a digest and counts, never file contents).
#[derive(Debug)]
pub struct CheckSummary {
    /// The sha256 content digest (OQ-7).
    pub version: ConfigVersion,
    /// `catalog.operating_points.len()`.
    pub operating_points: usize,
    /// `policy.tiers.len()`.
    pub tiers: usize,
}

impl CheckSummary {
    /// The one-line stdout verdict.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "catalog ok: config={} points={} tiers={} daemon=ok",
            self.version.0, self.operating_points, self.tiers
        )
    }
}

/// `herdr-governor check-config` — load + validate `catalog.toml`,
/// require the `[daemon]` table, and report. The credential is *not*
/// checked: a secret's existence is not a config question, and reading it
/// on a check path is a needless touch.
pub async fn check_config(config_dir: &std::path::Path) -> Result<CheckSummary, DaemonError> {
    let loaded = config::load(&config_dir.join(startup::CATALOG_NAME)).await?;
    if loaded.daemon.is_none() {
        return Err(DaemonError::NoDaemonTable);
    }
    Ok(CheckSummary {
        version: loaded.version,
        operating_points: loaded.config.catalog.operating_points.len(),
        tiers: loaded.config.policy.tiers.len(),
    })
}

/// The daemon's entry point (`herdr-governor daemon`, and the in-process
/// path tests spawn). Runs §4.3's ordered bring-up — state dir, catalog +
/// `[daemon]` + credential, lock-then-unlink, `Store::open`, restart
/// marking, then the bind and the tasks — and serves the coordinator
/// mailbox until `Signal::Shutdown`, the optional `shutdown` oneshot, or
/// every sender dropping. Returns the process exit code.
///
/// The optional `seam` is the in-process fault seam (§4.4, test-only);
/// `None` falls back to `GOV_DAEMON_SEAM` (argv's `--seam` already folded
/// into `settings`/`seam` by the caller).
pub async fn run(
    settings: Settings,
    seam: Option<SeamConfig>,
    shutdown: Option<oneshot::Receiver<()>>,
) -> Result<ExitCode, DaemonError> {
    let clock = clock::Clock::new();
    let now = clock.now();
    let armed_seam = match seam {
        Some(armed) => Some(armed),
        None => SeamConfig::from_env()?,
    };

    let Prepared {
        paths,
        lock,
        store,
        loaded,
        daemon,
        resolved,
        api_key: _api_key,
        catalog_path,
    } = startup::prepare(&settings).await?;

    // The subscriber installs only once `prepare` has succeeded: a startup
    // refusal must emit exactly one sanitized stderr line (§4.3 step 2,
    // F27) — installing earlier would put step lines in front of it. In
    // tests a prior `set_global_default` makes this a no-op.
    let _unused = log::init();
    if let Some(armed) = &armed_seam {
        log::seam_armed(
            &armed.suffix,
            boundary_name(armed.boundary),
            action_name(armed.action),
        );
    }

    let mut coordinator = Coordinator::new(
        store,
        CoordinatorArgs {
            loaded,
            daemon: daemon.clone(),
            catalog_path,
            clock,
            seam: armed_seam,
        },
    );
    coordinator.mark_restart(now)?;

    // §4.3 step 8 — the bind happens strictly after restart marking (H#5).
    let listener = startup::bind(&paths)?;
    log::bound(&paths.sock());

    let (tx, rx) = mpsc::channel::<Msg>(coordinator::MSG_CAPACITY);
    let accept = crate::mcp::serve::spawn(
        listener,
        tx.clone(),
        coordinator.shutdown_receiver(),
        crate::adapters::herdr::Client::new(resolved.herdr_socket.clone()),
        daemon.herdr_op_timeout,
    );
    let tick = serve::spawn_tick(&resolved, daemon.herdr_op_timeout, tx.clone());
    let mut tasks = shutdown::spawn_signals(&tx);
    tasks.extend([accept, tick]);
    drop(tx);

    let (_coordinator, _stop) = coordinator.serve(rx, shutdown).await;
    shutdown::teardown(&paths.sock(), tasks, lock).await?;
    Ok(ExitCode::SUCCESS)
}

/// `herdr-governor daemon …` — parse argv then `run`. On refusal stderr
/// gets one sanitized line and the process the error's exit code (§4.3
/// step 2's contract — the line never carries file contents).
pub async fn cli(args: &[String]) -> ExitCode {
    match parse_daemon_args(args) {
        Ok(settings::Invocation { settings, seam }) => match run(settings, seam, None).await {
            Ok(code) => code,
            Err(err) => refuse(&err),
        },
        Err(err) => refuse(&err),
    }
}

/// `herdr-governor check-config …` — stdout gets the `CheckSummary` line
/// on success; a refusal gets one sanitized stderr line + the code.
pub async fn check_cli(args: &[String]) -> ExitCode {
    let dir = match parse_check_config_args(args) {
        Ok(dir) => dir,
        Err(err) => return refuse(&err),
    };
    match check_config(&dir).await {
        Ok(summary) => {
            use std::io::Write as _;
            if writeln!(io::stdout().lock(), "{}", summary.line()).is_err() {
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(err) => refuse(&err),
    }
}

/// One sanitized stderr line plus the error's process exit code.
fn refuse(err: &DaemonError) -> ExitCode {
    use std::io::Write as _;
    drop(writeln!(
        io::stderr().lock(),
        "{}",
        sanitize(&err.to_string())
    ));
    err.code()
}

/// The seam boundary's spec spelling (for `log::seam_armed`).
fn boundary_name(boundary: Boundary) -> &'static str {
    match boundary {
        Boundary::PreDispatch => "pre_dispatch",
        Boundary::DispatchCommitted => "dispatch_committed",
        Boundary::WireReturned => "wire_returned",
        Boundary::ResultCommitted => "result_committed",
    }
}

/// The seam action's spec spelling.
fn action_name(action: SeamAction) -> &'static str {
    match action {
        SeamAction::Abort => "abort",
        SeamAction::Pause(_) => "pause",
    }
}
