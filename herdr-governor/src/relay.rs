//! `relay` — the per-session stdio transport (ADR-0004, spec F1/N4/N7,
//! p5-plan §4.11). A caller's own harness spawns `herdr-governor relay`
//! as its MCP server, once per session. The relay derives the F1 caller
//! envelope once at start, then forwards every JSON-RPC request to the
//! daemon's unix socket on a fresh connection inside the v1 framing —
//! stateless, so a harness crash-respawn and a daemon restart are both
//! safe. std-only: no tokio, no tracing, nothing but the stdio loop under
//! the 8 MB RSS bound.

use std::io::{self, BufRead as _, Write as _, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use thiserror::Error;

use crate::adapters::herdr::codec::LineAccumulator;

use forward::Inbound;

mod forward;
mod identity;

/// The JSON-RPC parse fault the relay answers itself: a stdin line that
/// never decodes into a request names no `id`, so the reply's is null.
const PARSE_ERROR: &[u8] =
    b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"Parse error\"}}\n";

const USAGE: &str = "usage: herdr-governor relay [--socket PATH]";

/// The default governor socket under the state dir (p5-plan §4.15);
/// `--socket` overrides. `HOME` is the relay's one env read beyond
/// `HERDR_*` (OQ-F).
const DEFAULT_SOCKET: &str = ".local/state/herdr-governor/governor.sock";

/// Fatal exits only: bad argv (2) or a runtime failure (1) — the relay
/// logs nothing but its own fatal error.
#[derive(Debug, Error)]
enum RelayError {
    /// Bad argv, or `--socket` omitted with `HOME` unset.
    #[error("{0}")]
    Usage(String),
    /// The F1 derivation could not run — no envelope exists to attach.
    #[error("{0}")]
    Identity(#[from] identity::IdentityError),
    /// A stdio read/write failure; nothing sane remains to do.
    #[error("stdio: {0}")]
    Io(#[from] io::Error),
}

/// The `relay` subcommand entry — `args` is argv after the subcommand
/// word; `main.rs` calls `relay::run(&argv[2..])`. Exit 0 on stdin EOF;
/// SIGINT/SIGTERM keep their default disposition and end the process.
#[must_use]
pub fn run(args: &[String]) -> ExitCode {
    match serve(args) {
        Ok(code) => code,
        Err(err) => {
            let _ignored = writeln!(io::stderr().lock(), "relay: {err}");
            match err {
                RelayError::Usage(_) => ExitCode::from(2),
                RelayError::Identity(_) | RelayError::Io(_) => ExitCode::FAILURE,
            }
        }
    }
}

/// Write one reply line and flush — the per-line lock is deliberate:
/// each line is a whole reply and flush-on-write keeps the stdio wire
/// prompt without a long-lived guard.
fn emit(reply: &[u8]) -> io::Result<()> {
    let mut out = stdout().lock();
    out.write_all(reply)?;
    out.flush()
}

/// The stdio loop: bounded lines in, one reply per request out. A line
/// over the 1 MiB bound — pending or completed — is answered as the
/// `-32700` parse fault it cannot outgrow into a request.
fn serve(args: &[String]) -> Result<ExitCode, RelayError> {
    let socket = socket_path(args)?;
    let identity = identity::derive()?;
    let mut acc = LineAccumulator::new();
    loop {
        match acc.take_line() {
            Ok(Some(line)) => {
                if let Some(reply) = respond(&socket, &identity, &line) {
                    emit(&reply)?;
                }
            }
            Ok(None) => {
                // A fresh lock per read: `fill_buf`/`consume` borrow the
                // guard, which dies at the end of this arm.
                let mut lock = stdin().lock();
                let chunk = lock.fill_buf()?;
                if chunk.is_empty() {
                    return Ok(ExitCode::SUCCESS);
                }
                let oversized = acc.push(chunk).is_err();
                let used = chunk.len();
                lock.consume(used);
                drop(lock);
                if oversized {
                    emit(PARSE_ERROR)?;
                }
            }
            Err(_) => emit(PARSE_ERROR)?,
        }
    }
}

/// Classify, then forward or answer locally. `None` means nothing is
/// owed on stdio — the dropped notification.
fn respond(socket: &Path, identity: &identity::Identity, line: &[u8]) -> Option<Vec<u8>> {
    match forward::classify(line) {
        Inbound::Notification => None,
        Inbound::Malformed => Some(PARSE_ERROR.to_vec()),
        Inbound::Request {
            id,
            method,
            request,
        } => Some(
            forward::exchange(socket, identity, &request)
                .unwrap_or_else(|_| forward::unavailable_reply(&id, &method)),
        ),
    }
}

fn socket_path(args: &[String]) -> Result<PathBuf, RelayError> {
    match args {
        [] => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                RelayError::Usage(format!("{USAGE}: HOME unset and no --socket given"))
            })?;
            Ok(PathBuf::from(home).join(DEFAULT_SOCKET))
        }
        [flag, path] if flag == "--socket" => Ok(PathBuf::from(path)),
        _ => Err(RelayError::Usage(USAGE.to_owned())),
    }
}
