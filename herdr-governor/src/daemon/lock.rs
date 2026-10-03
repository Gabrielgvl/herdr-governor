//! `lock` — the single-instance lock (H#4, §4.3 step 3): `flock(LOCK_EX |
//! LOCK_NB)` on `<state>/lock` proves this process owns the state dir.
//!
//! The ordering is the F32 fix: **lock first, then unlink**. A loser never
//! touches `<state>/governor.sock` — it only probes it so the exit message
//! can say whether the holding daemon still answers. The winner unlinks the
//! stale socket before the listener binds (the lock proves no live owner
//! can be serving that path).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};

use super::paths::{FILE_MODE, Paths};

/// The probe deadline — a live daemon answers instantly; a dead socket
/// fails `connect` immediately, so this bound only guards a wedged
/// listener that accepts without replying.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// The probe request — the daemon answers a `ping` JSON-RPC method with an
/// empty result; anything else gets `-32601`. (M2 replaces this with the
/// real transport; the method name is the probe vocabulary, not a tool.)
const PROBE_FRAME: &[u8] = br#"{"jsonrpc":"2.0","id":"gov:probe","method":"ping"}"#;

/// The held lock — `flock` releases when the fd closes, so dropping this
/// is the release step of §4.14 teardown. The field is never read: the
/// fd's whole job is staying open.
#[derive(Debug)]
pub(super) struct InstanceLock {
    _file: File,
}

/// Why `acquire` failed.
#[derive(Debug, thiserror::Error)]
pub(super) enum LockError {
    /// `<state>/lock` is held by another process. `answers` is whether the
    /// stale-socket probe got a reply — the message says "a daemon answers"
    /// vs "the lock is held but the socket is dead" (a half-dead holder
    /// still owns the state dir; we still refuse).
    #[error("state dir held by another daemon (socket answers: {answers})")]
    Held { answers: bool },
    /// An I/O failure on the lock file or the flock call.
    #[error("daemon lock: {0}")]
    Io(#[from] io::Error),
}

/// Blocking `connect` + one bounded request/response on `sock`. Returns
/// `true` only when a reply byte arrives before the deadline — the holder
/// is live *and* answering. `pub` so tests probe the same way.
pub(super) fn probe(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    drop(stream.set_read_timeout(Some(PROBE_TIMEOUT)));
    drop(stream.set_write_timeout(Some(PROBE_TIMEOUT)));
    if stream
        .write_all(PROBE_FRAME)
        .and_then(|()| stream.write_all(b"\n"))
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 1];
    stream.read(&mut buf).is_ok_and(|n| n == 1)
}

/// §4.3 step 3 — lock-then-unlink:
/// 1. `flock(LOCK_EX|LOCK_NB)` on `<state>/lock`. Held → `Err(Held)`; the
///    socket probe only decorates the message, it never changes the
///    verdict and it never unlinks.
/// 2. Lock acquired → this process owns the state dir: unlink a leftover
///    `<state>/governor.sock` (a crashed predecessor's path) and report
///    whether one was present so startup can log it.
///
/// Returns the held lock plus whether a stale socket was unlinked.
pub(super) fn acquire(paths: &Paths) -> Result<(InstanceLock, bool), LockError> {
    let lock_path = paths.lock();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    let mut permissions = fs::metadata(&lock_path)?.permissions();
    permissions.set_mode(FILE_MODE);
    fs::set_permissions(&lock_path, permissions)?;

    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => {
            // A live holder owns the state dir. Probe its socket to say
            // whether it still answers — then refuse either way.
            return Err(LockError::Held {
                answers: probe(&paths.sock()),
            });
        }
        Err(errno) => return Err(io::Error::from(errno).into()),
    }

    // Winner: a leftover socket belongs to a crashed predecessor (the lock
    // proves no live owner), so it is safe to unlink now.
    let sock = paths.sock();
    let removed = match fs::remove_file(&sock) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };
    Ok((InstanceLock { _file: file }, removed))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread;

    use super::{LockError, acquire};
    use crate::daemon::paths::Paths;

    /// A held `flock` makes a second `acquire` refuse with `Held` — the
    /// second daemon never reaches the socket path.
    #[test]
    fn lock_refuses_second_holder() {
        let tmp = tempfile::tempdir().expect("tmp");
        let paths = Paths::create(&tmp.path().join("state")).expect("paths");
        let (first, _removed) = acquire(&paths).expect("first acquire");
        match acquire(&paths) {
            Err(LockError::Held { answers }) => {
                assert!(!answers, "no live socket to answer");
            }
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        // After release a fresh acquire wins again.
        let (_second, _removed_again) = acquire(&paths).expect("reacquire after drop");
    }

    /// The lock holder wins; the loser never touches the socket; the
    /// *winner* unlinks a stale socket left by a crashed predecessor.
    #[test]
    fn lock_holder_wins_and_owner_unlinks_stale_socket() {
        let tmp = tempfile::tempdir().expect("tmp");
        let paths = Paths::create(&tmp.path().join("state")).expect("paths");
        let sock = paths.sock();

        // Stale socket: a bound-then-dropped listener leaves the file.
        {
            let _listener = UnixListener::bind(&sock).expect("bind stale");
        }
        assert!(sock.exists(), "stale socket file present");

        // Holder 1 acquires → its job unlinks the stale file.
        let (first, removed) = acquire(&paths).expect("holder 1");
        assert!(removed, "the winner unlinks the stale socket");
        assert!(!sock.exists(), "stale socket gone");

        // Simulate holder 1 dying mid-bind: drop the lock while a (stale)
        // socket file exists again.
        {
            let _listener = UnixListener::bind(&sock).expect("rebind stale");
        }
        drop(first);

        // A loser probes: it holds a live answering socket behind a thread
        // → Held{answers: true} and the socket must remain untouched.
        let (winner_lock, _) = acquire(&paths).expect("holder 2");
        let (stop, stop_rx) = mpsc::channel::<()>();
        let (bound, bound_rx) = mpsc::channel::<()>();
        let answer_sock = sock.clone();
        let answer =
            thread::spawn(move || {
                let listener = UnixListener::bind(&answer_sock).expect("bind answer");
                listener.set_nonblocking(true).expect("nonblocking");
                let _sent = bound.send(());
                while matches!(stop_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    if let Ok((mut conn, _)) = listener.accept() {
                        let mut buf = [0u8; 256];
                        drop(conn.read(&mut buf));
                        drop(conn.write_all(
                            b"{\"jsonrpc\":\"2.0\",\"id\":\"gov:probe\",\"result\":{}}\n",
                        ));
                    }
                    thread::yield_now();
                }
            });
        bound_rx.recv().expect("answer thread bound");

        match acquire(&paths) {
            Err(LockError::Held { answers }) => {
                assert!(answers, "the live probe answered");
            }
            other => panic!("expected Held, got {other:?}"),
        }
        assert!(sock.exists(), "the loser never unlinks the live socket");

        drop(stop);
        answer.join().expect("answer thread");
        drop(winner_lock);
    }

    /// `acquire` creates the lock file `0600` inside the `0700` state dir.
    #[test]
    fn lock_file_is_0600_in_state_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let paths = Paths::create(&tmp.path().join("state")).expect("paths");
        let (_lock, _removed) = acquire(&paths).expect("acquire");
        let mode = fs::metadata(paths.lock())
            .expect("lock meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "lock file mode");
        // The lock stays usable across re-open: a second Open of the same
        // path while held still refuses.
        match acquire(&paths) {
            Err(LockError::Held { .. }) => {}
            other => panic!("expected Held, got {other:?}"),
        }
    }
}
