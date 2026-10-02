//! The transport: the `Client` handle, one `tokio::net::UnixStream` per
//! request (the A2 evidence — one unary request per connection, closed
//! ~100 ms after the reply; a pipelined second frame is dropped), the
//! connect-time `ConnEpoch` mint, the bounded line read, and the armed
//! `Subscription` stream machinery.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::codec::{
    Frame, HerdrError, LineAccumulator, decode_frame, decode_subevent, encode_request,
    error_to_typed, malformed,
};
use super::types::{Observed, SubEvent};

/// A per-connection discontinuity marker (OQ-8): the client's monotonic
/// connect sequence plus the socket file's inode and mtime observed at dial
/// time. Protocol 22 carries no server-incarnation proof (A4
/// confirmed-negative), so this never claims to be one — the daemon mints
/// the `HerdrIncarnation` it stores and detects discontinuity by comparing
/// epochs. A socket file swapped between the stat and the connect is the
/// known ceiling and surfaces as a stale epoch the F28 re-proof covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnEpoch {
    /// Monotonic sequence of connections this client has made.
    pub seq: u64,
    /// Inode of the socket file observed at dial time.
    pub socket_inode: u64,
    /// Socket-file mtime, seconds part.
    pub socket_mtime_secs: i64,
    /// Socket-file mtime, nanoseconds part.
    pub socket_mtime_nsecs: i64,
}

/// The Herdr client handle: a socket path plus the shared connect counter
/// that keeps `ConnEpoch::seq` process-monotonic across clones.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    conns: Arc<AtomicU64>,
}

impl Client {
    /// A client for the session socket at `socket` — e.g. the
    /// ADR-0004/A2-isolated session socket, never the live one.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            conns: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Connect, send one request frame, read one reply frame — all inside
    /// the caller's deadline. `DeadlineExceeded` drops the half-open
    /// connection; the request may still have landed (F8: `unconfirmed`,
    /// never `absent`).
    pub(crate) async fn request(
        &self,
        method: &str,
        params: &impl Serialize,
        deadline: Duration,
    ) -> Result<Observed<Value>, HerdrError> {
        let call = async {
            let mut conn = self.connect().await?;
            // ponytail: the epoch's connect sequence doubles as the request
            // id — one request per connection makes them 1:1, and the reply
            // id then names its own epoch. Upgrade path if request ids ever
            // need minting independently of connects: a second counter.
            let id = format!("gov:{}", conn.epoch().seq);
            let frame = encode_request(&id, method, params)?;
            conn.send_request(&frame).await?;
            conn.read_reply(&id).await
        };
        match tokio::time::timeout(deadline, call).await {
            Ok(reply) => reply,
            Err(_) => Err(HerdrError::DeadlineExceeded),
        }
    }

    /// Dial the socket and mint the connection's epoch — the one transport
    /// op every caller shares.
    pub(crate) async fn connect(&self) -> Result<Conn, HerdrError> {
        connect(&self.socket, &self.conns).await
    }
}

/// One open connection: a buffered stream, the line accumulator that
/// outlives single reads (a `fill_buf` can pull several frames, or half
/// of one, at once), and the epoch it was dialed under. Consumed by
/// `read_frame`/`send_request`; dropped at op end.
pub(crate) struct Conn {
    stream: BufReader<UnixStream>,
    acc: LineAccumulator,
    epoch: ConnEpoch,
}

impl Conn {
    /// The epoch this connection was dialed under.
    pub(crate) fn epoch(&self) -> ConnEpoch {
        self.epoch
    }

    /// Write one request frame and flush.
    pub(crate) async fn send_request(&mut self, frame: &[u8]) -> Result<(), HerdrError> {
        self.stream.write_all(frame).await.map_err(HerdrError::Io)?;
        self.stream.flush().await.map_err(HerdrError::Io)
    }

    /// Read one bounded line (the reply frame, or the next event on a
    /// subscription). Partial frames reassemble across reads (the recorded
    /// `partial_frame_reassembled` behavior); a clean EOF is `StreamClosed`.
    pub(crate) async fn read_frame(&mut self) -> Result<Vec<u8>, HerdrError> {
        loop {
            if let Some(line) = self.acc.take_line()? {
                return Ok(line);
            }
            let chunk = self.stream.fill_buf().await.map_err(HerdrError::Io)?;
            if chunk.is_empty() {
                return Err(HerdrError::StreamClosed);
            }
            let n = chunk.len();
            self.acc.push(chunk)?;
            self.stream.consume(n);
        }
    }

    /// The request's reply: one frame, decoded and typed. An event frame
    /// here is malformed — unary connections carry no events.
    pub(crate) async fn read_reply(
        &mut self,
        request_id: &str,
    ) -> Result<Observed<Value>, HerdrError> {
        match decode_frame(&self.read_frame().await?)? {
            Frame::Result { id, result } => {
                if id != request_id {
                    return Err(malformed(format!(
                        "reply id {id:?} does not echo request {request_id:?}"
                    )));
                }
                Ok(Observed {
                    epoch: self.epoch,
                    value: result,
                })
            }
            Frame::Error { id, code, message } => {
                Err(error_to_typed(request_id, &id, &code, message))
            }
            Frame::Event { .. } => Err(malformed("event frame on a unary connection".to_owned())),
        }
    }
}

/// Dial the socket and mint its epoch: stat the socket file (inode+mtime),
/// then connect. Stat first so a missing file fails as `Connect`, and so
/// the epoch describes the thing we dialed in the common case.
pub(crate) async fn connect(path: &Path, seq: &AtomicU64) -> Result<Conn, HerdrError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(HerdrError::Connect)?;
    let stream = UnixStream::connect(path)
        .await
        .map_err(HerdrError::Connect)?;
    let epoch = ConnEpoch {
        seq: seq.fetch_add(1, Ordering::Relaxed),
        socket_inode: meta.ino(),
        socket_mtime_secs: meta.mtime(),
        socket_mtime_nsecs: meta.mtime_nsec(),
    };
    Ok(Conn {
        stream: BufReader::new(stream),
        acc: LineAccumulator::new(),
        epoch,
    })
}

/// The armed stream's read loop: frames until EOF or a malformed line,
/// typed events forwarded, and the terminal error (`StreamClosed` on the
/// recorded silent teardown) delivered as the last channel item before
/// exit. A mid-stream error frame maps through the same typed error map —
/// evidence says armed streams never carry one, but mapping it honestly
/// beats dropping it.
pub(crate) async fn drive(
    mut conn: Conn,
    request_id: String,
    tx: mpsc::Sender<Result<SubEvent, HerdrError>>,
) {
    loop {
        let event = match conn.read_frame().await {
            Err(e) => Err(e),
            Ok(line) => match decode_frame(&line) {
                Err(e) => Err(e),
                Ok(Frame::Event { event, data }) => decode_subevent(&event, data),
                Ok(Frame::Result { .. }) => Err(malformed(
                    "result frame on an armed subscription stream".to_owned(),
                )),
                Ok(Frame::Error { id, code, message }) => {
                    Err(error_to_typed(&request_id, &id, &code, message))
                }
            },
        };
        let terminal = event.is_err();
        if tx.send(event).await.is_err() || terminal {
            return;
        }
    }
}

/// The handle an armed `events.subscribe` surfaces as: a stream of typed
/// events over `mpsc`, the conn task holding the socket open, and the
/// epoch of the connection it is armed on.
#[derive(Debug)]
pub struct Subscription {
    rx: mpsc::Receiver<Result<SubEvent, HerdrError>>,
    task: JoinHandle<()>,
    epoch: ConnEpoch,
    client: Client,
    specs: Vec<super::types::SubscriptionSpec>,
    deadline: Duration,
}

impl Subscription {
    /// The armed handle, wiring the reader task to its channel — built by
    /// `Client::subscribe` after the `subscription_started` ack.
    pub(crate) fn armed(
        rx: mpsc::Receiver<Result<SubEvent, HerdrError>>,
        task: JoinHandle<()>,
        epoch: ConnEpoch,
        client: Client,
        specs: Vec<super::types::SubscriptionSpec>,
        deadline: Duration,
    ) -> Self {
        Self {
            rx,
            task,
            epoch,
            client,
            specs,
            deadline,
        }
    }

    /// The next typed event, or `None` once the stream task has ended — a
    /// terminal error (e.g. `StreamClosed` on the recorded silent EOF)
    /// arrives as the last item first.
    pub async fn next(&mut self) -> Option<Result<SubEvent, HerdrError>> {
        self.rx.recv().await
    }

    /// The epoch of the connection this stream is armed on — discontinuity
    /// detection input for the daemon, not an incarnation proof.
    #[must_use]
    pub fn epoch(&self) -> ConnEpoch {
        self.epoch
    }

    /// Re-arm on a fresh connection with the same specs (the A2 ruling:
    /// every teardown re-arms; the caller catches up via
    /// `pane.read`/`session.snapshot` — the wire has no replay). Meaningful
    /// once this stream has ended; `Connect` failure on the re-arm is the
    /// "server gone" signal.
    ///
    /// # Errors
    /// Same surface as the `subscribe` op.
    pub async fn rearm(&self) -> Result<Subscription, HerdrError> {
        self.client
            .subscribe(self.specs.clone(), self.deadline)
            .await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}
