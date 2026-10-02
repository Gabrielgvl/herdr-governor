//! Unit tests for the Jev client, all against the committed fixtures and
//! an in-process fake over `tokio::net::TcpListener` — never the live
//! network, never a real credential (keys are fake values in temp 0600
//! files). Pure wire tests live in `tests/wire.rs`; the fake-server leg
//! (request capture, every recorded error class, the size gate and the
//! credential seam) in `tests/client.rs`; shared helpers stay here.

mod client;
mod wire;

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::client::{ApiKey, Client};
use super::wire::{Kind, WireQuestion};

pub(super) const LAUNCH: &str =
    include_str!("../../../../tests/fixtures/contract/jev-launch-evaluation.json");
pub(super) const RAW: &str =
    include_str!("../../../../tests/fixtures/contract/jev-raw-response.json");
pub(super) const SUPERVISION: &str =
    include_str!("../../../../tests/fixtures/contract/jev-supervision-review.json");
pub(super) const EVIDENCE: &str =
    include_str!("../../../../tests/fixtures/contract/jev-wire-evidence.json");

pub(super) const FAKE_KEY: &str = "tsk_fake_0123456789abcdef";

pub(super) fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture json")
}

/// The fixture's `questions` as wire questions, in the fixture's key
/// order (`serde_json` sorts object keys, so order is given explicitly).
pub(super) fn fixture_questions(body: &Value, order: &[&str]) -> Vec<WireQuestion> {
    order
        .iter()
        .map(|name| {
            let q = &body["questions"][*name];
            let kind = match q["type"].as_str().expect("type") {
                "noul" => Kind::Noul { threshold: None },
                "choice" => Kind::Choice,
                other => panic!("unknown fixture type {other}"),
            };
            let mut criteria: Vec<(String, String)> = q["criteria"]
                .as_object()
                .expect("criteria")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().expect("criterion").to_owned()))
                .collect();
            // The fixture writes noul criteria `true` then `false`.
            criteria.sort_by_key(|(k, _)| k != "true");
            WireQuestion {
                name: (*name).to_owned(),
                kind,
                instructions: q["instructions"].as_str().expect("instructions").to_owned(),
                criteria,
            }
        })
        .collect()
}

/// A fake credential file with the given mode. The `TempDir` must outlive
/// the test.
pub(super) fn credential_file(contents: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("credentials");
    std::fs::write(&path, contents).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    (dir, path)
}

pub(super) async fn fake_key() -> (tempfile::TempDir, ApiKey) {
    let (dir, path) = credential_file(&format!("{FAKE_KEY}\n"), 0o600);
    let key = ApiKey::read_0600(&path).await.expect("fake key");
    (dir, key)
}

/// A bound loopback listener plus the client dialing it.
pub(super) async fn server() -> (TcpListener, Client) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    (listener, Client::new(&base).expect("client"))
}

/// What the fake does after reading one request.
#[derive(Debug, Clone)]
pub(super) enum Reply {
    /// Write a full HTTP/1.1 response and close.
    Respond {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// Never answer (the client's deadline decides).
    Silent,
    /// Close the socket without a status line (the peer-abort shape).
    Abort,
}

pub(super) fn respond(status: u16, body: &str) -> Reply {
    Reply::Respond {
        status,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
    }
}

/// The captured request: method, path, lower-cased headers and body.
#[derive(Debug)]
pub(super) struct Captured {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Captured {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Accept one connection, read exactly one HTTP/1.1 request, apply
/// `reply`, return the captured request.
pub(super) async fn serve_once(listener: &TcpListener, reply: Reply) -> Captured {
    let (mut stream, _) = listener.accept().await.expect("accept");
    let captured = read_request(&mut stream).await;
    match reply {
        Reply::Respond {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                write!(head, "{k}: {v}\r\n").expect("fmt");
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).await.expect("head");
            stream.write_all(&body).await.expect("body");
            stream.flush().await.expect("flush");
            stream.shutdown().await.expect("shutdown");
        }
        Reply::Silent => std::future::pending::<()>().await,
        Reply::Abort => drop(stream),
    }
    captured
}

async fn read_request(stream: &mut TcpStream) -> Captured {
    let mut buf = Vec::new();
    let split = loop {
        let mut byte = [0_u8; 1];
        let n = stream.read(&mut byte).await.expect("read");
        assert!(n == 1, "request ended before headers");
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break buf.len().saturating_sub(4);
        }
    };
    let head = String::from_utf8(buf[..split].to_vec()).expect("ascii head");
    let mut lines = head.split("\r\n");
    let request_line = lines.next().expect("request line");
    let mut parts = request_line.split(' ');
    let method = parts.next().expect("method").to_owned();
    let path = parts.next().expect("path").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map_or(0, |(_, v)| v.parse().expect("content-length"));
    let mut body = vec![0_u8; len];
    stream.read_exact(&mut body).await.expect("body");
    Captured {
        method,
        path,
        headers,
        body,
    }
}
