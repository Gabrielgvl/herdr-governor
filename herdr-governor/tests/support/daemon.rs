//! `daemon` — the `TestDaemon` half of the e2e harness (P5.T1): the
//! governor daemon started either in-process (`daemon::run` on the
//! test's own runtime, with an optional `SeamConfig` — the [r2]
//! signature) or as a real `herdr-governor daemon` child (the seam
//! arrives as `GOV_DAEMON_SEAM`, the crash-matrix env hook), plus the
//! `Catalog`/`fixture` builder that writes a valid short-window
//! `catalog.toml` + `0600` credential under a state tempdir. Every wait
//! is a bounded poll — never a `sleep` — and `shutdown` is the graceful
//! stop (the oneshot in-process, `SIGTERM` for a child).

use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread::JoinHandle as ThreadJoin;
use std::time::{Duration, Instant};

use herdr_governor::daemon::{self, Boundary, SeamAction, SeamConfig, Settings};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// The shipped binary — `spawn_child` runs the same argv the operator's
/// install runs.
const BIN: &str = env!("CARGO_BIN_EXE_herdr-governor");
/// Every wait's bound — the socket appears in milliseconds; this is the
/// broken-bring-up backstop.
const DEADLINE: Duration = Duration::from_secs(10);

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
    /// `[policy].tiers` — the choice questions' label set.
    pub tiers: Vec<String>,
    /// The `[catalog]` member, verbatim TOML — `operating_points = []`
    /// when the test has no operating points.
    pub points_toml: String,
    /// Extra `[daemon]` keys, verbatim TOML lines (`retire_*`,
    /// `transcript_dir`, timeouts).
    pub daemon_extra: String,
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
            tiers: vec!["fast".to_owned()],
            points_toml: "operating_points = []".to_owned(),
            daemon_extra: String::new(),
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
             cooldown_secs = 60\n\n[catalog]\n{points}\n\n[daemon]\n\
             herdr_socket = \"{sock}\"\njev_base_url = \"{jev}\"\n\
             jev_model = \"{model}\"\nreconcile_secs = {reconcile}\n{extra}",
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

    /// `<state>/governor.db` — the store tests seed through a second
    /// `Store::open` (the reconcile suite's pattern).
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.state.join("governor.db")
    }
}

/// Write `root/{state,config}`: `catalog.toml` from `catalog` and the
/// `0600` credential `daemon::run` requires.
#[must_use]
pub fn fixture(catalog: &Catalog) -> DaemonDirs {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let config = root.path().join("config");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(config.join("catalog.toml"), catalog.toml()).expect("catalog");
    let credentials = config.join("credentials");
    std::fs::write(&credentials, "test-token\n").expect("credentials");
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600))
        .expect("credentials mode");
    DaemonDirs {
        root,
        state,
        config,
    }
}

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
    /// the inherited value is scrubbed either way.
    pub async fn spawn_child(settings: &Settings, seam: Option<SeamConfig>) -> Self {
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
        if let Some(armed) = &seam {
            command.env("GOV_DAEMON_SEAM", seam_spec(armed));
        }
        let mut child = command.spawn().expect("daemon child spawns");
        let mut pipe = child.stderr.take().expect("stderr piped");
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            pipe.read_to_string(&mut text).ok();
            text
        });
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

    /// Send a signal to the child daemon (`"-TERM"`, `"-KILL"`, `"-ABRT"`
    /// spellings like the startup suite's `kill` calls). In-process
    /// daemons are tasks — a kill test spawns a child.
    pub fn signal(&self, sig: &str) {
        let Some(Kind::Child { child, .. }) = &self.kind else {
            panic!("signal() is for child daemons");
        };
        let status = Command::new("kill")
            .args([sig, &child.id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill {sig} {}", child.id());
    }

    /// Consume the child and wait for it to exit on its own — after a
    /// `signal`, or a seam's `Abort`. Returns the status plus the drained
    /// stderr (the `seam hit` marker's home). In-process daemons exit
    /// only through `shutdown`.
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
