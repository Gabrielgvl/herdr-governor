//! The relay↔daemon frame (plan §4.11, ADR-0004): one line per request on
//! a fresh connection —
//! `{"v":1,"caller":{"paneId","projectRoot","relayInstanceId"},"rpc":<request>}`
//! in, `{"v":1,"rpc":<response>}` back — under the shared
//! `LineAccumulator`'s 1 MiB bound each way. Notifications never cross:
//! the relay drops them before framing. The inner `rpc` member is an
//! opaque JSON-RPC envelope — this layer never opens it.

use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::adapters::herdr::MAX_FRAME_BYTES;

/// §4.11 — the only frame revision on the wire.
const FRAME_VERSION: u8 = 1;

/// Every way a frame line is refused — typed at the boundary, never a
/// silent drop.
#[derive(Debug, Error)]
pub enum FrameError {
    /// A line over the 1 MiB bound (§4.11 — the same bound
    /// `LineAccumulator` enforces while splitting the stream).
    #[error("frame line exceeds the 1 MiB bound")]
    TooLarge,
    /// The line is not the `{"v":1,…}` frame: bad JSON, a missing or extra
    /// member, a malformed `caller`, or an unknown `v`.
    #[error("malformed frame: {detail}")]
    Malformed {
        /// Why the line failed the frame rules.
        detail: String,
    },
}

/// One decoded request frame — the caller envelope plus the opaque `rpc`
/// request the daemon answers.
#[derive(Debug)]
pub struct Inbound {
    /// The relay-attached caller identity (F1) — lexical shape only;
    /// `identity::validate_caller_envelope` owns validity.
    pub caller: CallerEnvelope,
    /// The forwarded JSON-RPC request object.
    pub rpc: Value,
}

/// The serde view of `CallerEnvelope` — the domain type carries no serde,
/// so the frame owns the `{paneId, projectRoot, relayInstanceId}`
/// spellings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Caller {
    pane_id: String,
    project_root: String,
    relay_instance_id: String,
}

impl From<&CallerEnvelope> for Caller {
    fn from(envelope: &CallerEnvelope) -> Self {
        Self {
            pane_id: envelope.pane_id.0.clone(),
            project_root: envelope.project_root.0.clone(),
            relay_instance_id: envelope.relay_instance_id.0.clone(),
        }
    }
}

impl From<Caller> for CallerEnvelope {
    fn from(caller: Caller) -> Self {
        Self {
            pane_id: PaneId(caller.pane_id),
            project_root: ProjectRoot(caller.project_root),
            relay_instance_id: RelayInstanceId(caller.relay_instance_id),
        }
    }
}

#[derive(Serialize)]
struct RequestWire<'a> {
    v: u8,
    caller: Caller,
    rpc: &'a Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestWireIn {
    v: u64,
    caller: Caller,
    rpc: Value,
}

#[derive(Serialize)]
struct ReplyWire<'a> {
    v: u8,
    rpc: &'a Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyWireIn {
    v: u64,
    rpc: Value,
}

/// Encode one request frame plus its `\n` (§4.11). The member order is the
/// struct field order — `relay`'s shape test pins the same bytes. The
/// members cannot fail serialization; `unwrap_or_default` only quiets the
/// typed `Result`.
#[must_use]
pub fn encode_request(caller: &CallerEnvelope, rpc: &Value) -> Vec<u8> {
    let wire = RequestWire {
        v: FRAME_VERSION,
        caller: Caller::from(caller),
        rpc,
    };
    let mut bytes = serde_json::to_vec(&wire).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Decode one inbound request line: the bound first, then the
/// `v`/`caller`/`rpc` envelope rules (`deny_unknown_fields`), `v` pinned.
pub fn decode_request(line: &[u8]) -> Result<Inbound, FrameError> {
    let wire: RequestWireIn = parse_bounded(line)?;
    versioned(wire.v)?;
    Ok(Inbound {
        caller: wire.caller.into(),
        rpc: wire.rpc,
    })
}

/// Encode one reply frame plus its `\n`.
#[must_use]
pub fn encode_reply(rpc: &Value) -> Vec<u8> {
    let wire = ReplyWire {
        v: FRAME_VERSION,
        rpc,
    };
    let mut bytes = serde_json::to_vec(&wire).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Decode one inbound reply line — same bound, `{"v":1,"rpc":…}` only.
pub fn decode_reply(line: &[u8]) -> Result<Value, FrameError> {
    let wire: ReplyWireIn = parse_bounded(line)?;
    versioned(wire.v)?;
    Ok(wire.rpc)
}

/// The shared first checks: the line must be inside the 1 MiB bound and
/// strict-JSON for the frame shape.
fn parse_bounded<T: serde::de::DeserializeOwned>(line: &[u8]) -> Result<T, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    serde_json::from_slice(line).map_err(|e| FrameError::Malformed {
        detail: format!("line is not the frame: {e}"),
    })
}

/// `v` is pinned at 1 — an unknown revision is refused, never interpreted.
fn versioned(v: u64) -> Result<(), FrameError> {
    if v != u64::from(FRAME_VERSION) {
        return Err(FrameError::Malformed {
            detail: format!("unsupported frame version {v}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
    use serde_json::json;

    use super::{
        FrameError, MAX_FRAME_BYTES, decode_reply, decode_request, encode_reply, encode_request,
    };
    use crate::adapters::herdr::codec::LineAccumulator;

    fn envelope() -> CallerEnvelope {
        CallerEnvelope {
            pane_id: PaneId("w6:pKQ".into()),
            project_root: ProjectRoot("/repo".into()),
            relay_instance_id: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
        }
    }

    #[test]
    fn framing_round_trip_and_bound() {
        let rpc = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
        let line = encode_request(&envelope(), &rpc);
        // The §4.11 request shape, byte for byte — `rpc` is a serde_json
        // map, so its members serialize sorted.
        assert_eq!(
            String::from_utf8_lossy(&line),
            "{\"v\":1,\"caller\":{\"paneId\":\"w6:pKQ\",\"projectRoot\":\"/repo\",\"relayInstanceId\":\"0123456789abcdef0123456789abcdef\"},\"rpc\":{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"ping\"}}\n",
            "the request frame is the §4.11 shape verbatim"
        );

        // A line split mid-frame still assembles — the accumulator scans
        // each byte once and holds the 1 MiB bound.
        let mut acc = LineAccumulator::new();
        let split = line.len().div_euclid(2);
        acc.push(&line[..split]).expect("first half fits");
        assert!(
            acc.take_line().expect("no error").is_none(),
            "a half line yields nothing yet"
        );
        acc.push(&line[split..]).expect("second half fits");
        let assembled = acc
            .take_line()
            .expect("no error")
            .expect("the frame completes");
        let inbound = decode_request(&assembled).expect("the frame decodes");
        assert_eq!(
            inbound.caller.pane_id,
            PaneId("w6:pKQ".into()),
            "the caller round-trips"
        );
        assert_eq!(
            inbound.caller.relay_instance_id,
            RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
            "the relay id round-trips"
        );
        assert_eq!(inbound.rpc, rpc, "the rpc member round-trips verbatim");

        let reply = json!({"jsonrpc":"2.0","id":1,"result":{}});
        let reply_line = encode_reply(&reply);
        assert_eq!(
            String::from_utf8_lossy(&reply_line),
            "{\"v\":1,\"rpc\":{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{}}}\n",
            "the reply frame is the §4.11 shape verbatim"
        );
        assert_eq!(
            decode_reply(&reply_line[..reply_line.len() - 1]).expect("the reply decodes"),
            reply,
            "the reply round-trips"
        );

        // The bound: a pending line over 1 MiB is a typed error from the
        // accumulator, and `decode_*` refuses an oversized line outright.
        assert!(
            acc.push(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err(),
            "a pending line over the bound is refused"
        );
        let bad: [&[u8]; 4] = [
            &vec![b'x'; MAX_FRAME_BYTES + 1],
            b"{\"v\":2,\"caller\":{\"paneId\":\"p\",\"projectRoot\":\"/r\",\"relayInstanceId\":\"i\"},\"rpc\":{}}",
            b"{\"v\":1,\"caller\":{\"paneId\":\"p\"},\"rpc\":{}}",
            b"not json",
        ];
        for bad_line in bad {
            assert!(
                decode_request(bad_line).is_err(),
                "an oversized, wrong-version, short-member or non-JSON line is refused"
            );
        }
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(
            decode_reply(&oversized).is_err(),
            "the reply decode holds the same bound"
        );
        match decode_request(&oversized) {
            Err(FrameError::TooLarge) => {}
            other => panic!("an oversized line is TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn framing_unknown_members_and_versions_refuse() {
        let cases: &[&[u8]] = &[
            b"{\"v\":1,\"caller\":{\"paneId\":\"p\",\"projectRoot\":\"/r\",\"relayInstanceId\":\"i\"},\"rpc\":{},\"extra\":1}",
            b"{\"v\":1,\"caller\":{\"paneId\":\"p\",\"projectRoot\":\"/r\",\"relayInstanceId\":\"i\",\"x\":0},\"rpc\":{}}",
            b"{\"v\":0,\"caller\":{\"paneId\":\"p\",\"projectRoot\":\"/r\",\"relayInstanceId\":\"i\"},\"rpc\":{}}",
            b"{\"v\":1,\"rpc\":{},\"junk\":true}",
        ];
        for line in cases {
            assert!(
                decode_request(line).is_err(),
                "the request decode refuses unknown members and versions"
            );
            assert!(
                decode_reply(line).is_err(),
                "the reply decode refuses unknown members and versions"
            );
        }
    }
}
