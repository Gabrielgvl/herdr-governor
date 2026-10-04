//! `daemon` — the `TestDaemon` half of the e2e harness (P5.T1): the
//! governor daemon started either in-process (`daemon::run` on the
//! test's own runtime, with an optional `SeamConfig` — the [r2]
//! signature) or as a real `herdr-governor daemon` child — awaited for
//! its bound socket (`spawn_child`), or returned live for tests
//! asserting the bring-up refusal itself (`spawn_raw`). `fixture`
//! builds the `Catalog` + `DaemonDirs` world under a state tempdir;
//! `probe` is the read-back half: bounded waits, the lock probe, the
//! signal/stderr legs, the store and `check-config` reads. Every wait
//! is a bounded poll — never a `sleep` — and `shutdown` is the graceful
//! stop (the oneshot in-process, `SIGTERM` for a child).

pub mod fixture;
pub mod probe;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread::JoinHandle as ThreadJoin;
use std::time::{Duration, Instant};

use herdr_governor::daemon::{self, Boundary, SeamAction, SeamConfig, Settings};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use fixture::{Catalog, DaemonDirs, fixture, write_fixture};
pub use probe::{
    await_for, bindings, check_config_version, never, probe, signal, socket_incarnation, stderr_of,
};

/// The shipped binary — `spawn_*` runs the same argv the operator's
/// install runs; the env-matrix spawn in `defaults` uses it too.
pub const BIN: &str = env!("CARGO_BIN_EXE_herdr-governor");

/// Every wait's bound — the socket appears in milliseconds; this is the
/// broken-bring-up backstop.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// The seam pause `pause_at_dispatch` arms — long enough for a test to
/// interpose, far inside any `shutdown_grace_secs = 1` fixture.
const SEAM_PAUSE_MS: u64 = 400;

/// Which bring-up produced the daemon.
#[derive(Debug)]
enum Kind {
    /// `daemon::run` on the test's own runtime.
    InProcess {
        /// The graceful-stop signal `daemon::run` awaits.
        stop: oneshot::Sender<()>,
        /// The serving task — joins with `Ok(ExitCode::SUCCESS)` after
        /// `stop`.
        join: JoinHandle<Result<ExitCode, daemon::DaemonError>>,
    },
    /// `herdr-governor daemon` as a real process.
    Child {
        /// The child handle.
        child: Child,
        /// The stderr drain — the `seam hit` marker's home.
        stderr: ThreadJoin<String>,
    },
}

/// A started daemon — owns the in-process task or the child process.
/// `Drop` cleans up a daemon `shutdown` never stopped (task abort /
/// SIGKILL), so a panicking test leaves no serving task or process.
#[derive(Debug)]
pub struct TestDaemon {
    kind: Option<Kind>,
    state_dir: PathBuf,
    config_dir: PathBuf,
}

impl TestDaemon {
    /// `daemon::run` in-process with `seam` ([r2]: the seam rides the
    /// call, never the env); returns once the governor socket is bound.
    /// Panics if `daemon::run` exits first — the `DaemonError` is the
    /// report.
    pub async fn start_in_process(settings: &Settings, seam: Option<SeamConfig>) -> Self {
        let (stop, rx) = oneshot::channel::<()>();
        let join = tokio::spawn(daemon::run(settings.clone(), seam, Some(rx)));
        let sock = socket_path(&settings.state_dir);
        let deadline = deadline();
        while Instant::now() < deadline {
            if sock.exists() {
                return Self::new(
                    Kind::InProcess { stop, join },
                    &settings.state_dir,
                    &settings.config_dir,
                );
            }
            if join.is_finished() {
                let outcome = join.await;
                panic!("the daemon exited during bring-up: {outcome:?}");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        join.abort();
        panic!(
            "the governor socket never bound at {} within {DEADLINE:?}",
            sock.display()
        );
    }

    /// `herdr-governor daemon` as a real child process — the binary the
    /// operator's argv runs, stderr piped for the seam markers. `seam`
    /// (when armed) rides as `GOV_DAEMON_SEAM` in the child's env only;
    /// the inherited value is scrubbed either way. Returns once the
    /// governor socket is bound — a child that exits first is a
    /// bring-up failure reported on its own stderr.
    pub async fn spawn_child(settings: &Settings, seam: Option<SeamConfig>) -> Self {
        let mut child = command(settings, seam)
            .spawn()
            .expect("daemon child spawns");
        let stderr = stderr_of(&mut child);
        let sock = socket_path(&settings.state_dir);
        let deadline = deadline();
        while Instant::now() < deadline {
            if sock.exists() {
                return Self::new(
                    Kind::Child { child, stderr },
                    &settings.state_dir,
                    &settings.config_dir,
                );
            }
            if let Some(status) = child.try_wait().expect("child try_wait") {
                let text = stderr.join().expect("stderr drains");
                panic!("the daemon child exited during bring-up: {status} {text}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        child.kill().ok();
        child.wait().ok();
        panic!(
            "the daemon child's socket never bound at {} within {DEADLINE:?}",
            sock.display()
        );
    }

    /// `herdr-governor daemon` spawned and returned live — no bind
    /// wait: the bring-up-refusal tests (`exit 2` on a bad catalog,
    /// `exit 3` on a held lock) assert the child's own exit. `wait`
    /// reaps it, `signal` drives it, `Drop` kills a still-running one.
    /// The argv/env are `spawn_child`'s, seam included.
    #[must_use]
    pub fn spawn_raw(settings: &Settings, seam: Option<SeamConfig>) -> Self {
        let mut child = command(settings, seam)
            .spawn()
            .expect("daemon child spawns");
        let stderr = stderr_of(&mut child);
        Self::new(
            Kind::Child { child, stderr },
            &settings.state_dir,
            &settings.config_dir,
        )
    }

    /// `<state>/governor.sock` — the MCP socket both clients dial.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        socket_path(&self.state_dir)
    }

    /// `<state>` — the daemon's state dir.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// `<config>` — the catalog dir (the SIGHUP reload target).
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// `<state>/governor.db` — the seeded store path.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.state_dir.join("governor.db")
    }

    /// Whether the daemon is still running — `try_wait` on the child,
    /// `is_finished` on the in-process task. The lock-held refusal
    /// tests assert the holder survives a second daemon's exit.
    #[must_use]
    pub fn alive(&mut self) -> bool {
        match &mut self.kind {
            Some(Kind::Child { child, .. }) => child.try_wait().expect("child try_wait").is_none(),
            Some(Kind::InProcess { join, .. }) => !join.is_finished(),
            None => false,
        }
    }

    /// Send a signal to the child daemon (`"-TERM"`, `"-KILL"`, `"-ABRT"`
    /// spellings like the startup suite's `kill` calls). In-process
    /// daemons are tasks — a kill test spawns a child.
    pub fn signal(&self, sig: &str) {
        let Some(Kind::Child { child, .. }) = &self.kind else {
            panic!("signal() is for child daemons");
        };
        signal(child, sig);
    }

    /// Consume the child and wait for it to exit on its own — after a
    /// `signal`, a seam's `Abort`, or the bring-up refusal `spawn_raw`
    /// leaves running. Returns the status plus the drained stderr (the
    /// `seam hit` marker's home). In-process daemons exit only through
    /// `shutdown`.
    pub async fn wait(mut self) -> (ExitStatus, String) {
        let Some(Kind::Child { mut child, stderr }) = self.kind.take() else {
            panic!("wait() is for child daemons — use shutdown() in-process");
        };
        let status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .expect("wait task joins")
            .expect("child waits");
        let text = stderr.join().expect("stderr drains");
        (status, text)
    }

    /// The graceful stop: the oneshot in-process, `SIGTERM` for a child.
    /// Asserts the clean exit — a dirty one is a test failure, not a
    /// state the harness papers over.
    pub async fn shutdown(mut self) {
        match self.kind.take() {
            Some(Kind::InProcess { stop, join }) => {
                stop.send(()).ok();
                let code = join
                    .await
                    .expect("daemon task joins")
                    .expect("daemon::run returns Ok");
                assert_eq!(code, ExitCode::SUCCESS, "the daemon exits cleanly");
            }
            Some(Kind::Child { child, stderr }) => {
                let (status, text) = self.wait_child(child, stderr).await;
                assert!(status.success(), "the daemon child exits cleanly: {text}");
            }
            None => panic!("shutdown() on an already-consumed daemon"),
        }
    }

    /// SIGTERM + wait + stderr — the child's graceful-stop leg.
    async fn wait_child(
        &self,
        mut child: Child,
        stderr: ThreadJoin<String>,
    ) -> (ExitStatus, String) {
        let pid = child.id().to_string();
        let kill_status = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("kill runs");
        assert!(kill_status.success(), "SIGTERM {pid}");
        let status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .expect("wait task joins")
            .expect("child waits");
        let text = stderr.join().expect("stderr drains");
        (status, text)
    }

    fn new(kind: Kind, state_dir: &Path, config_dir: &Path) -> Self {
        Self {
            kind: Some(kind),
            state_dir: state_dir.to_path_buf(),
            config_dir: config_dir.to_path_buf(),
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        match self.kind.take() {
            Some(Kind::InProcess { join, .. }) => join.abort(),
            Some(Kind::Child { mut child, .. }) => {
                child.kill().ok();
                child.wait().ok();
            }
            None => {}
        }
    }
}

/// The `daemon` argv/env both child bring-ups share: dirs from
/// `settings`, the optional `--herdr-socket`/`--reconcile-secs`
/// overrides, the `GOV_DAEMON_SEAM` arm (or a scrubbed env), stdout
/// null and stderr piped for the seam markers.
fn command(settings: &Settings, seam: Option<SeamConfig>) -> Command {
    let mut command = Command::new(BIN);
    command
        .arg("daemon")
        .arg("--state-dir")
        .arg(&settings.state_dir)
        .arg("--config-dir")
        .arg(&settings.config_dir)
        .env_remove("GOV_DAEMON_SEAM")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(herdr_socket) = &settings.herdr_socket {
        command.arg("--herdr-socket").arg(herdr_socket);
    }
    if let Some(secs) = settings.reconcile_secs {
        command.arg("--reconcile-secs").arg(secs.to_string());
    }
    if let Some(armed) = seam {
        command.env("GOV_DAEMON_SEAM", seam_spec(&armed));
    }
    command
}

/// A `dispatch_committed` pause seam on `suffix` — the deterministic
/// window between the durable commit and the wire op (§4.4).
#[must_use]
pub fn pause_at_dispatch(suffix: &str) -> SeamConfig {
    SeamConfig {
        suffix: suffix.to_owned(),
        boundary: Boundary::DispatchCommitted,
        action: SeamAction::Pause(Duration::from_millis(SEAM_PAUSE_MS)),
    }
}

/// `<state>/governor.sock`.
fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("governor.sock")
}

/// `now + DEADLINE` (checked — `Instant` arithmetic is the
/// `arithmetic_side_effects` trigger otherwise).
fn deadline() -> Instant {
    Instant::now()
        .checked_add(DEADLINE)
        .expect("a 10s deadline always fits")
}

/// `SeamConfig` back to its `<suffix>@<boundary>:<action>` env spelling —
/// the grammar `GOV_DAEMON_SEAM` parses.
fn seam_spec(seam: &SeamConfig) -> String {
    let boundary = match seam.boundary {
        Boundary::PreDispatch => "pre_dispatch",
        Boundary::DispatchCommitted => "dispatch_committed",
        Boundary::WireReturned => "wire_returned",
        Boundary::ResultCommitted => "result_committed",
    };
    let action = match seam.action {
        SeamAction::Abort => "abort".to_owned(),
        SeamAction::Pause(d) => format!("pause:{}", d.as_millis()),
    };
    format!("{}@{boundary}:{action}", seam.suffix)
}
