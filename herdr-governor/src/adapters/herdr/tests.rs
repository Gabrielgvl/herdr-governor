//! Unit tests for the protocol-22 client: codec round-trips against the
//! committed fixtures, the frame bound, the error-code map, and the
//! socket-level behaviors a unit can prove without P4.H2's fake server
//! (typed deadline elapse, the silent-EOF teardown, one-request-per-
//! connection). Codec-layer tests live in `tests/codec.rs`, socket tests
//! in `tests/socket.rs`; the shared helpers stay here.
//!
//! I9 note: harness names never appear in any of these files, even in
//! test data — the fixture-carrying assertions look at structure, not
//! kinds.

mod codec;
mod socket;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::conn::{Client, ConnEpoch};

pub(super) fn epoch() -> ConnEpoch {
    ConnEpoch {
        seq: 0,
        socket_inode: 7,
        socket_mtime_secs: 1,
        socket_mtime_nsecs: 0,
    }
}

/// A bound socket in a tempdir plus the client dialing it. The returned
/// `TempDir` must outlive the test.
pub(super) fn socket() -> (tempfile::TempDir, UnixListener, Client) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("h.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    (dir, listener, Client::new(&path))
}

/// Accept one connection, read its single NDJSON request line, return the
/// stream and the request id (the server echoes it back).
pub(super) async fn accept_request(listener: &UnixListener) -> (BufReader<UnixStream>, String) {
    let (stream, _) = listener.accept().await.expect("accept");
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .await
        .expect("read request");
    let req: Value = serde_json::from_slice(&line).expect("request json");
    let id = req["id"].as_str().expect("request id").to_owned();
    (reader, id)
}

/// Write one NDJSON line on the server side.
pub(super) async fn write_line(stream: &mut BufReader<UnixStream>, line: Value) {
    let mut bytes = line.to_string().into_bytes();
    bytes.push(b'\n');
    stream.get_mut().write_all(&bytes).await.expect("write");
    stream.get_mut().flush().await.expect("flush");
}
