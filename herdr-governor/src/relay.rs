//! `relay` — the per-session stdio transport (ADR-0004, spec F1/N4/N7,
//! p5-plan §4.11). A caller's own harness spawns `herdr-governor relay`
//! as its MCP server, once per session. The relay derives the F1 caller
//! envelope once at start, then forwards every JSON-RPC request to the
//! daemon's unix socket on a fresh connection inside the v1 framing —
//! stateless, so a harness crash-respawn and a daemon restart are both
//! safe. std-only: no tokio, no tracing, nothing but the stdio loop under
//! the 8 MB RSS bound.

use std::io::{self, BufRead, Write, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use thiserror::Error;

use crate::adapters::herdr::codec::LineAccumulator;
use crate::mcp::jsonrpc;

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

/// The stdio loop: bounded lines in, one reply per request out. A line
/// over the 1 MiB bound — pending or completed — is answered as the
/// `-32700` parse fault it cannot outgrow into a request.
fn serve(args: &[String]) -> Result<ExitCode, RelayError> {
    let socket = socket_path(args)?;
    let identity = identity::derive()?;
    pump(
        &socket,
        &identity,
        &mut stdin().lock(),
        &mut stdout().lock(),
    )?;
    Ok(ExitCode::SUCCESS)
}

/// The pump over any buffered byte source: `take_line` yields what the
/// accumulator holds, `fill_buf`/`consume` feed it. Each push stops at
/// the chunk's first newline, so an oversized-line refusal can only
/// clear the rejected line — every later request survives; a line that
/// overruns the bound still pending flips `draining`, which discards
/// stdin through that line's newline so its tail is never re-parsed as
/// a request of its own. The single locked `out` replaces per-reply
/// `emit` — one guard for the loop's life, flushed per reply.
fn pump(
    socket: &Path,
    identity: &identity::Identity,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> io::Result<()> {
    let mut acc = LineAccumulator::new();
    let mut draining = false;
    loop {
        match acc.take_line() {
            Ok(Some(line)) => {
                if let Some(reply) = respond(socket, identity, &line) {
                    out.write_all(&reply)?;
                    out.flush()?;
                }
            }
            Ok(None) => {
                let chunk = input.fill_buf()?;
                if chunk.is_empty() {
                    return Ok(());
                }
                if draining {
                    // Discard through the refused line's newline; a
                    // newline-free chunk is all tail and drops whole.
                    if let Some(n) = chunk.iter().position(|b| *b == b'\n') {
                        input.consume(n.saturating_add(1));
                        draining = false;
                    } else {
                        let used = chunk.len();
                        input.consume(used);
                    }
                    continue;
                }
                let end = chunk
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(chunk.len(), |n| n.saturating_add(1));
                let oversized = acc.push(chunk.get(..end).unwrap_or(chunk)).is_err();
                input.consume(end);
                if oversized {
                    out.write_all(PARSE_ERROR)?;
                    out.flush()?;
                    draining = true;
                }
            }
            Err(_) => {
                out.write_all(PARSE_ERROR)?;
                out.flush()?;
            }
        }
    }
}

/// Classify, then forward or answer locally. `None` means nothing is
/// owed on stdio — the dropped notification. A `Fault` is the typed
/// error envelope `jsonrpc::parse` built (`-32700`/`-32600`), answered
/// verbatim.
fn respond(socket: &Path, identity: &identity::Identity, line: &[u8]) -> Option<Vec<u8>> {
    match forward::classify(line) {
        Inbound::Notification => None,
        Inbound::Fault(response) => {
            let mut reply = jsonrpc::serialize(&response);
            reply.push(b'\n');
            Some(reply)
        }
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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Cursor, Read};
    use std::path::Path;

    use serde_json::Value;

    use super::{identity::Identity, pump};
    use crate::adapters::herdr::MAX_FRAME_BYTES;

    fn identity() -> Identity {
        Identity {
            pane_id: "w0:pAA".to_owned(),
            project_root: "/repo".to_owned(),
            relay_instance_id: "ab".repeat(16),
        }
    }

    /// A `BufRead` yielding `data` in `step`-sized chunks — the line
    /// bound lives at chunk granularity, so the oversized cases need
    /// control over where `fill_buf` cuts.
    struct Chunked<'a> {
        data: &'a [u8],
        step: usize,
    }

    impl Read for Chunked<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let take = self.data.len().min(buf.len());
            buf[..take].copy_from_slice(&self.data[..take]);
            self.consume(take);
            Ok(take)
        }
    }

    impl BufRead for Chunked<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Ok(&self.data[..self.data.len().min(self.step)])
        }
        fn consume(&mut self, amt: usize) {
            self.data = &self.data[amt..];
        }
    }

    /// Run the pump to stdin EOF against a nowhere socket — every request
    /// then answers `DAEMON_UNAVAILABLE` — and return the reply lines.
    fn run(input: &mut impl BufRead) -> Vec<Value> {
        let mut out = Vec::new();
        pump(
            Path::new("/nonexistent/governor.sock"),
            &identity(),
            input,
            &mut out,
        )
        .expect("pump runs to EOF");
        String::from_utf8(out)
            .expect("replies are utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("reply is json"))
            .collect()
    }

    fn ping(id: u64) -> Vec<u8> {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"ping\"}}\n").into_bytes()
    }

    /// F4 — one chunk carries an oversized line's newline followed by a
    /// valid request: only the rejected line is answered `-32700`; the
    /// request still forwards and gets its own reply. `Cursor` returns
    /// the whole input from one `fill_buf`, so both lines ride the same
    /// chunk — the case the accumulator's whole-buffer reset used to lose.
    #[test]
    fn oversized_completed_line_keeps_the_following_request() {
        let mut input = Vec::new();
        input.extend_from_slice(&vec![b'x'; MAX_FRAME_BYTES + 64]);
        input.push(b'\n');
        input.extend_from_slice(&ping(7));
        let replies = run(&mut Cursor::new(input));
        assert_eq!(
            replies.len(),
            2,
            "a parse fault then the ping reply — nothing else: {replies:?}"
        );
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(
            replies[1]["id"], 7,
            "the ping after the oversized line is answered"
        );
    }

    /// F3 — `id`-less objects that are not valid requests are refused
    /// `-32600` with a null id, never dropped: only a *valid* request
    /// object without `id` is a notification. The daemon's `parse`
    /// assigns the same refusal; the stdin boundary must too.
    #[test]
    fn invalid_id_less_objects_answer_32600_not_silence() {
        for line in [
            b"{}\n".as_slice(),
            b"{\"jsonrpc\":\"2.0\",\"method\":7}\n".as_slice(),
            b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n".as_slice(),
        ] {
            let replies = run(&mut Cursor::new(line));
            assert_eq!(
                replies.len(),
                1,
                "an invalid id-less request is refused, never dropped: {replies:?}"
            );
            assert_eq!(replies[0]["id"], Value::Null, "no usable id echoes");
            assert_eq!(
                replies[0]["error"]["code"], -32600,
                "the invalid-request fault is -32600"
            );
        }
    }

    /// F3 — valid-JSON non-objects (`42`, `[]`) are `-32600` invalid
    /// requests, not `-32700` parse faults; only bytes that are not JSON
    /// at all earn `-32700`.
    #[test]
    fn invalid_jsonrpc_values_answer_32600_not_32700() {
        for (line, code) in [
            ("42\n", -32600_i64),
            ("[1,2,3]\n", -32600_i64),
            ("not json\n", -32700_i64),
        ] {
            let replies = run(&mut Cursor::new(line.as_bytes()));
            assert_eq!(replies.len(), 1, "{line} gets one reply: {replies:?}");
            assert_eq!(replies[0]["id"], Value::Null);
            assert_eq!(
                replies[0]["error"]["code"], code,
                "{line} classifies to {code}"
            );
        }
    }

    /// F4 — a line that crosses the bound while still pending is drained
    /// through its newline: its tail is never re-parsed as a request (no
    /// second `-32700`), and the line after it still answers.
    #[test]
    fn oversized_pending_line_drains_to_its_newline() {
        let mut input = Vec::new();
        // Cross the bound inside a newline-free chunk (8 KiB pieces keep
        // the boundary ahead of the line's own terminator).
        input.extend_from_slice(&vec![b'x'; MAX_FRAME_BYTES + 9_000]);
        input.push(b'\n');
        input.extend_from_slice(&ping(9));
        let replies = run(&mut Chunked {
            data: &input,
            step: 8 * 1024,
        });
        assert_eq!(
            replies.len(),
            2,
            "one parse fault, one ping reply — the mid-line tail is not re-parsed: {replies:?}"
        );
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[1]["id"], 9);
    }
}
