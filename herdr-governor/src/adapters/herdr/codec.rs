//! The NDJSON codec for the protocol-22 socket: the bounded line
//! accumulator, the `{id,method,params}` / `{id,result}` / `{id,error}` /
//! `{event,data}` envelope decoder, the evidence-pinned error-code map, and
//! the derived subscription-id parse. Pure — no I/O, no clock.

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::types::{AgentStatus, Observed, PaneRead, PaneScrollInfo, SubEvent};

/// The governor-side socket-frame bound (spec N5): 1 MiB per line,
/// payload bytes before the `\n`. The server's own bound is 2 MiB — ours
/// is stricter and that is correct: a frame we cannot trust in memory is
/// not a frame.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Every failure an op or stream surfaces, typed at the boundary. Server
/// error codes map to dedicated variants only where the evidence records
/// them (`a2-subscription-evidence`, `a3-start-prompt-evidence`); every
/// other code passes through verbatim as [`HerdrError::Server`]. Kept
/// exhaustive on purpose: F15's pre-flight-vs-runtime mapping and F8's
/// certainty bookkeeping both depend on seeing every variant.
#[derive(Debug, Error)]
pub enum HerdrError {
    /// The unix connect (or the socket stat that precedes it) failed —
    /// refused, absent, or permission. On a `rearm` this is the A2 ruling's
    /// "server gone" signal.
    #[error("herdr connect failed: {0}")]
    Connect(#[source] std::io::Error),
    /// A read or write on an established connection failed (incl. the
    /// ECONNRESET of the recorded silent-teardown shape).
    #[error("herdr socket io: {0}")]
    Io(#[source] std::io::Error),
    /// A line over the governor-side bound — spec N5 pins the socket frame
    /// at 1 MiB; the server's own bound is 2 MiB, ours is stricter.
    #[error("frame exceeds the 1 MiB bound")]
    FrameTooLarge,
    /// A line that is not a valid protocol-22 envelope: bad JSON, missing
    /// envelope keys, a result id that does not echo the request, or an
    /// event frame on a unary connection. Never parse-and-continue.
    #[error("malformed frame: {detail}")]
    Malformed {
        /// Why the line failed the envelope rules.
        detail: String,
    },
    /// A server error frame carrying `id: ""` — the correlation-less form
    /// the server emits for a malformed submission on a fresh connection.
    #[error("uncorrelated server error {code}: {message}")]
    Uncorrelated {
        /// The wire error code.
        code: String,
        /// The wire error message.
        message: String,
    },
    /// `agent_pane_busy` — the typed pre-flight refusal; F15's only
    /// candidate-fallback shape.
    #[error("agent_pane_busy: {message}")]
    AgentPaneBusy {
        /// The wire error message.
        message: String,
    },
    /// `agent_not_found` — the prompt/get target is not a known agent.
    #[error("agent_not_found: {message}")]
    AgentNotFound {
        /// The wire error message.
        message: String,
    },
    /// `pane_not_found` — the pane locator resolves to nothing (also the
    /// per-subscription failure code on a subscribe leg).
    #[error("pane_not_found: {message}")]
    PaneNotFound {
        /// The wire error message.
        message: String,
    },
    /// Wire `code:"timeout"` — the server's typed "the operation timed
    /// out". For `agent.start` this is the erased runtime-startup failure:
    /// every cause (missing binary, rejected args, mid-start death) lands
    /// here, and F15 treats it as "any other outcome". Distinct from
    /// [`HerdrError::DeadlineExceeded`]: this reply is the server's own
    /// verdict, not our clock's.
    #[error("server timeout: {message}")]
    Timeout {
        /// The wire error message.
        message: String,
    },
    /// The caller-supplied per-op deadline elapsed before a reply arrived.
    /// The request may still have landed — for effect certainty this is
    /// `unconfirmed`, never `absent`.
    #[error("op deadline elapsed; the request may still be running")]
    DeadlineExceeded,
    /// A subscription arm that failed: the server reports it under the
    /// derived id `<request-id>:sub:<index>:probe` and closes the
    /// connection (the recorded `a2sub_subscribe_error_id_derived` shape).
    #[error("subscription {index} failed ({code}): {message}")]
    SubscriptionFailed {
        /// The zero-based index of the failed subscription in the request.
        index: u64,
        /// The wire error code (e.g. `pane_not_found`, `internal_error`).
        code: String,
        /// The wire error message.
        message: String,
    },
    /// A clean EOF before the reply (unary) or mid-stream (subscription).
    /// The recorded teardown shape is silent: zero bytes, no error frame.
    #[error("herdr stream closed without the frame we were owed")]
    StreamClosed,
    /// Any other server `{code, message}` the evidence does not promote to
    /// a dedicated variant (`internal_error`, `invalid_request`,
    /// `unsupported_event_wait_match`, …). Code and message pass through
    /// verbatim.
    #[error("server error {code}: {message}")]
    Server {
        /// The wire error code, verbatim.
        code: String,
        /// The wire error message, verbatim.
        message: String,
    },
}

/// Check a reply's `type` const against the one the asking op expects,
/// then decode the payload. The schema's result union is internally
/// tagged; the op that asked is the only authority on which variant it
/// wanted.
pub(crate) fn typed<R: serde::de::DeserializeOwned>(
    observed: Observed<Value>,
    want: &'static str,
) -> Result<Observed<R>, HerdrError> {
    let got = observed.value.get("type").and_then(Value::as_str);
    if got != Some(want) {
        return Err(malformed(format!(
            "expected result type \"{want}\", got {got:?}"
        )));
    }
    let Observed { epoch, value: raw } = observed;
    let value = serde_json::from_value::<R>(raw)
        .map_err(|e| malformed(format!("\"{want}\" payload: {e}")))?;
    Ok(Observed { epoch, value })
}

/// The shared constructor for the envelope-violation error — sibling
/// modules build theirs through here so the wording convention stays.
pub(crate) fn malformed(detail: impl Into<String>) -> HerdrError {
    HerdrError::Malformed {
        detail: detail.into(),
    }
}

/// A bounded newline splitter: bytes go in with [`Self::push`], complete
/// lines come out of [`Self::take_line`]. The bound applies to each line's
/// payload (bytes before the `\n`); a stream of short lines buffers no
/// bound, a pending line over [`MAX_FRAME_BYTES`] is a typed error, never a
/// truncation.
pub(crate) struct LineAccumulator {
    buf: Vec<u8>,
    /// `buf[..scanned]` holds no `\n`: every search resumes here, so each
    /// byte is scanned once however finely the frame was chunked.
    scanned: usize,
}

impl LineAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            buf: Vec::new(),
            scanned: 0,
        }
    }

    /// Append a chunk read off the socket.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), HerdrError> {
        self.buf.extend_from_slice(bytes);
        // Guard memory while a first line is still pending — the line-length
        // check itself lives in `take_line`, which also covers lines that
        // complete inside this chunk.
        if let Some(n) = self.newline() {
            self.scanned = n;
        } else {
            self.scanned = self.buf.len();
            if self.scanned > MAX_FRAME_BYTES {
                self.reset();
                return Err(HerdrError::FrameTooLarge);
            }
        }
        Ok(())
    }

    /// Take the next complete line (without its `\n`), or `None` while the
    /// line is still pending. A line over the bound is a typed error and
    /// the buffer resets — the connection it fed is closed anyway.
    pub(crate) fn take_line(&mut self) -> Result<Option<Vec<u8>>, HerdrError> {
        match self.newline() {
            Some(n) if n > MAX_FRAME_BYTES => {
                self.reset();
                Err(HerdrError::FrameTooLarge)
            }
            Some(n) => {
                let mut rest = self.buf.split_off(n.saturating_add(1));
                self.buf.truncate(n);
                std::mem::swap(&mut self.buf, &mut rest);
                // The remainder past the `\n` has not been searched yet.
                self.scanned = 0;
                Ok(Some(rest))
            }
            None if self.buf.len() > MAX_FRAME_BYTES => {
                self.reset();
                Err(HerdrError::FrameTooLarge)
            }
            None => {
                self.scanned = self.buf.len();
                Ok(None)
            }
        }
    }

    /// Position of the first `\n`, searching only the not-yet-scanned tail.
    fn newline(&self) -> Option<usize> {
        self.buf
            .get(self.scanned..)?
            .iter()
            .position(|b| *b == b'\n')
            .map(|off| self.scanned.saturating_add(off))
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.scanned = 0;
    }

    /// Bytes a further search would still have to visit — the cost pin:
    /// zero after a push that completes no line, so a push scans only
    /// its own chunk.
    #[cfg(test)]
    pub(crate) fn unscanned(&self) -> usize {
        self.buf.len().saturating_sub(self.scanned)
    }
}

/// One decoded inbound frame — the three envelope shapes the confirmed
/// surface produces (`{id,result}`, `{id,error{code,message}}`,
/// `{event,data}`).
#[derive(Debug)]
pub(crate) enum Frame {
    /// A success response: `{id, result}`.
    Result {
        /// The echoed request id.
        id: String,
        /// The result object; the op knows its `type` const.
        result: Value,
    },
    /// An error response: `{id, error{code, message}}`; `id` may be `""`
    /// (the correlation-less shape on a fresh connection).
    Error {
        /// The wire id — request echo, `""`, or the derived
        /// `<request>:sub:<i>:probe` form.
        id: String,
        /// The wire error code.
        code: String,
        /// The wire error message.
        message: String,
    },
    /// A subscription event: `{event, data}` — legal only on an armed
    /// subscription stream; a unary connection that receives one is
    /// malformed.
    Event {
        /// The event kind (`pane.output_matched`, …).
        event: String,
        /// The event payload.
        data: Value,
    },
}

#[derive(Serialize)]
struct RequestFrame<'a, P: Serialize> {
    id: &'a str,
    method: &'a str,
    params: &'a P,
}

/// Encode one request line: `{id, method, params}` plus the `\n`.
pub(crate) fn encode_request(
    id: &str,
    method: &str,
    params: &impl Serialize,
) -> Result<Vec<u8>, HerdrError> {
    let mut out = serde_json::to_vec(&RequestFrame { id, method, params })
        .map_err(|e| malformed(format!("request params do not serialize: {e}")))?;
    out.push(b'\n');
    Ok(out)
}

/// Decode one inbound line into its envelope class. Raw serde on a
/// `Value` first (the member layout is the classifier), then the typed
/// envelope rules: `error` wins over `result` wins over `event`; anything
/// else — non-object, missing `id`, missing `error`/`result`/`event`/`data`
/// — is malformed.
pub(crate) fn decode_frame(line: &[u8]) -> Result<Frame, HerdrError> {
    let value: Value =
        serde_json::from_slice(line).map_err(|e| malformed(format!("line is not json: {e}")))?;
    let Value::Object(map) = value else {
        return Err(malformed("envelope is not an object"));
    };
    let id_opt = map.get("id").and_then(Value::as_str);
    if let Some(err) = map.get("error") {
        let Some(id) = id_opt else {
            return Err(malformed("error envelope without string id"));
        };
        let obj = err
            .as_object()
            .ok_or_else(|| malformed("error member is not an object"))?;
        let (Some(code), Some(message)) = (
            obj.get("code").and_then(Value::as_str),
            obj.get("message").and_then(Value::as_str),
        ) else {
            return Err(malformed("error member lacks code/message"));
        };
        return Ok(Frame::Error {
            id: id.to_owned(),
            code: code.to_owned(),
            message: message.to_owned(),
        });
    }
    if let Some(result) = map.get("result") {
        let Some(id) = id_opt else {
            return Err(malformed("result envelope without string id"));
        };
        if id.is_empty() {
            return Err(malformed("result envelope with empty id"));
        }
        return Ok(Frame::Result {
            id: id.to_owned(),
            result: result.clone(),
        });
    }
    if let (Some(event), Some(data)) = (map.get("event").and_then(Value::as_str), map.get("data")) {
        return Ok(Frame::Event {
            event: event.to_owned(),
            data: data.clone(),
        });
    }
    Err(malformed("no result/error/event envelope member"))
}

/// Map an error frame to its typed variant: `id: ""` is the
/// correlation-less shape, `<request>:sub:<i>:probe` the derived
/// subscription-failure id, a matching id the request's own error — the
/// evidence-pinned codes get dedicated variants, everything else is
/// `Server` verbatim. An error id that is none of those is malformed (on a
/// one-request-per-connection socket there is nothing else it can be).
pub(crate) fn error_to_typed(
    request_id: &str,
    frame_id: &str,
    code: &str,
    message: String,
) -> HerdrError {
    if frame_id.is_empty() {
        return HerdrError::Uncorrelated {
            code: code.to_owned(),
            message,
        };
    }
    if let Some(index) = subscription_error_index(request_id, frame_id) {
        return HerdrError::SubscriptionFailed {
            index,
            code: code.to_owned(),
            message,
        };
    }
    if frame_id != request_id {
        return malformed(format!("error id {frame_id:?} does not correlate"));
    }
    match code {
        "agent_pane_busy" => HerdrError::AgentPaneBusy { message },
        "agent_not_found" => HerdrError::AgentNotFound { message },
        "pane_not_found" => HerdrError::PaneNotFound { message },
        "timeout" => HerdrError::Timeout { message },
        other => HerdrError::Server {
            code: other.to_owned(),
            message,
        },
    }
}

/// Parse the derived subscription-failure id `<request-id>:sub:<i>:probe`
/// (recorded shape: `replyIdSuffix ":sub:0:probe"` on a failed arm). The
/// request id is matched verbatim first so ids containing `:` stay safe.
fn subscription_error_index(request_id: &str, frame_id: &str) -> Option<u64> {
    frame_id
        .strip_prefix(request_id)?
        .strip_prefix(":sub:")?
        .strip_suffix(":probe")?
        .parse::<u64>()
        .ok()
}

/// Decode the `{event, data}` payload of an armed subscription stream into
/// its typed event. The schema's `SubscriptionEventKind` lists exactly
/// three kinds; anything else is malformed, never dropped.
pub(crate) fn decode_subevent(event: &str, data: Value) -> Result<SubEvent, HerdrError> {
    match event {
        "pane.output_matched" => {
            #[derive(serde::Deserialize)]
            struct D {
                pane_id: String,
                matched_line: String,
                read: Box<PaneRead>,
            }
            let d: D = serde_json::from_value(data)
                .map_err(|e| malformed(format!("pane.output_matched data: {e}")))?;
            Ok(SubEvent::OutputMatched {
                pane_id: d.pane_id,
                matched_line: d.matched_line,
                read: d.read,
            })
        }
        "pane.scroll_changed" => {
            #[derive(serde::Deserialize)]
            struct D {
                pane_id: String,
                workspace_id: String,
                scroll: PaneScrollInfo,
            }
            let d: D = serde_json::from_value(data)
                .map_err(|e| malformed(format!("pane.scroll_changed data: {e}")))?;
            Ok(SubEvent::ScrollChanged {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
                scroll: d.scroll,
            })
        }
        "pane.agent_status_changed" => {
            #[derive(serde::Deserialize)]
            struct D {
                pane_id: String,
                workspace_id: String,
                agent_status: AgentStatus,
                agent: Option<String>,
                display_agent: Option<String>,
                title: Option<String>,
                #[serde(default)]
                state_labels: std::collections::BTreeMap<String, String>,
            }
            let d: D = serde_json::from_value(data)
                .map_err(|e| malformed(format!("pane.agent_status_changed data: {e}")))?;
            Ok(SubEvent::AgentStatusChanged {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
                agent_status: d.agent_status,
                agent: d.agent,
                display_agent: d.display_agent,
                title: d.title,
                state_labels: d.state_labels,
            })
        }
        other => Err(malformed(format!("unsubscribed event kind {other:?}"))),
    }
}
