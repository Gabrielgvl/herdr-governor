//! `probe` — reading a running daemon back from outside (P5.T1): the
//! bounded waits (`await_for`, `never`), the §4.3 lock-probe dialect,
//! the signal + stderr-drain legs every child test shares, the store
//! and `check-config` reads the assertions land on, and the socket
//! incarnation the seam tests derive.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use herdr_governor::adapters::herdr::ConnEpoch;
use herdr_governor::daemon::identity;
use herdr_governor::store::Store;

use super::{BIN, DEADLINE};

/// Poll `until` until it holds or the deadline passes — then panic.
/// Async so the test's runtime keeps driving the fakes and any
/// in-process daemon while the child makes progress.
pub async fn await_for(what: &str, mut until: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        if until() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Whether `what` stays false for `within` — the negative wait: a
/// thing that must NOT happen is asserted by exhausting the window.
pub async fn never(what: &str, within: Duration, mut happened: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(within).expect("deadline");
    while Instant::now() < deadline {
        assert!(!happened(), "{what} must not happen");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The §4.3 lock-probe dialect: connect, bare `ping`, read one line.
#[must_use]
pub fn probe(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _timeout = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(br#"{"jsonrpc":"2.0","id":"gov:probe","method":"ping"}"#)
        .and_then(|()| stream.write_all(b"\n"))
        .is_err()
    {
        return false;
    }
    let mut reply = String::new();
    let _read = BufReader::new(stream).read_line(&mut reply);
    reply.contains("\"result\"")
}

/// `kill <sig> <pid>` — the operator's signal, exactly as delivered.
/// `TestDaemon::signal` is the same leg through the harness.
pub fn signal(child: &Child, sig: &str) {
    let status = Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success(), "kill {sig} {}", child.id());
}

/// The child's buffered stderr, read on a thread so a chatty child
/// never deadlocks the pipe.
#[must_use]
pub fn stderr_of(child: &mut Child) -> std::thread::JoinHandle<String> {
    let mut pipe = child.stderr.take().expect("stderr piped");
    std::thread::spawn(move || {
        let mut text = String::new();
        let _read = pipe.read_to_string(&mut text);
        text
    })
}

/// The `relay_bindings` rows joined to their caller:
/// `(relay_instance_id, pane_at_bind, bound_at, native_session)`. Read
/// after the daemon is down — `Store::open` is never concurrent with a
/// running daemon in these tests.
#[must_use]
pub fn bindings(store_db: &Path) -> Vec<(String, String, String, String)> {
    let store = Store::open(store_db).expect("store opens");
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT b.relay_instance_id, b.pane_id_at_bind, b.bound_at, c.native_session
             FROM relay_bindings b JOIN callers c ON c.caller_id = b.caller_id",
        )
        .expect("bindings query");
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })
    .expect("bindings rows")
    .collect::<Result<Vec<_>, _>>()
    .expect("bindings collect")
}

/// `herdr-governor check-config --config-dir <dir>` → the catalog's
/// content digest off the `catalog ok: config=<d>` line.
#[must_use]
pub fn check_config_version(config: &Path) -> String {
    let out = Command::new(BIN)
        .arg("check-config")
        .arg("--config-dir")
        .arg(config)
        .output()
        .expect("check-config spawns");
    assert!(
        out.status.success(),
        "check-config: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout utf8");
    stdout
        .trim()
        .strip_prefix("catalog ok: config=")
        .and_then(|rest| rest.split_whitespace().next())
        .expect("config= digest")
        .to_owned()
}

/// The `HerdrIncarnation` the daemon's own mint derives for a snapshot
/// over `path` — `identity::incarnation` over the stat the connect
/// records (`<inode>:<mtime_secs>.<mtime_nsecs zero-padded>`). No other
/// mint exists — see `daemon/identity.rs`.
#[must_use]
pub fn socket_incarnation(path: &Path) -> String {
    let meta = std::fs::metadata(path).expect("socket stat");
    let epoch = ConnEpoch {
        seq: 0,
        socket_inode: meta.ino(),
        socket_mtime_secs: meta.mtime(),
        socket_mtime_nsecs: meta.mtime_nsec(),
    };
    identity::incarnation(&epoch).0
}
