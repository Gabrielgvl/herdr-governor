//! `observe` — F6's Run page: the Run's state, settlement, frozen
//! handoff and latest answered acceptance, plus `OUTBOX_PAGE` outbox
//! entries strictly after the cursor's `seq`. Bodies never ride the
//! page — digest and byte length only (N5).

use std::fmt::Write as _;

use governor_core::delivery::MessageBody;
use governor_core::identity::{CallerKey, Digest, RunId};
use governor_core::lifecycle::Settlement;
use serde_json::{Value, json};

use crate::daemon::api::{ToolError, ToolResponse};
use crate::store::Store;

use super::{read_run, unavailable};

/// The outbox page `observe` fills — §4.12's bound keeps a full page
/// under the 60,000-byte result ceiling by a wide margin.
const OUTBOX_PAGE: u32 = 200;

/// `observe {runId, cursor?}` — owner-gated (F4: a Run the store does
/// not know, or one the caller does not own, answers `NOT_OWNER`), the
/// outbox cursor is `{"k": <seq>}` rendered hex-of-JSON, and a malformed
/// cursor is `REQUEST_INVALID`, never silently rewound (§4.12).
pub(super) fn page(
    store: &Store,
    caller: &CallerKey,
    run_id: &RunId,
    cursor: Option<&str>,
) -> ToolResponse {
    let Some(run) = read_run(store, run_id, caller)? else {
        return super::not_owner();
    };
    let after = match cursor {
        Some(text) => Some(
            decode_cursor(text)
                .ok_or_else(|| ToolError::new(ToolError::REQUEST_INVALID, "unreadable cursor"))?,
        ),
        None => None,
    };
    let page_size = usize::try_from(OUTBOX_PAGE).unwrap_or(usize::MAX);
    let page = store
        .outbox_page(run_id, after, OUTBOX_PAGE.saturating_add(1))
        .map_err(|_err| unavailable())?;
    let has_more = page.len() > page_size;
    let items: Vec<Value> = page.iter().take(page_size).map(outbox_item).collect();
    let next_cursor = match (has_more, page.get(items.len().saturating_sub(1))) {
        (true, Some(last)) => Some(encode_cursor(last.seq)),
        _ => None,
    };
    let handoff = store
        .handoffs(run_id)
        .map_err(|_err| unavailable())?
        .into_iter()
        .max_by_key(|frozen| frozen.work_generation);
    let acceptance = store
        .latest_acceptance(run_id)
        .map_err(|_err| unavailable())?
        .map(|record| {
            record
                .judgments
                .iter()
                .map(judgment_item)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({
        "run": {
            "runId": run.id.0,
            "state": run.state.as_str(),
            "settlement": run.settlement.map(|s| s.as_str()),
            "settlementReason": settlement_reason(&run),
            "ownerGeneration": run.owner_generation,
            "judgingDigest": run.judging_digest.as_ref().map(hex),
            "handoffPath": handoff.as_ref().map(|h| h.frozen_path.clone()),
        },
        "acceptance": acceptance,
        "outbox": {
            "items": items,
            "nextCursor": next_cursor,
        },
    }))
}

/// One `observe` outbox item — seq, key, state and the body digests;
/// the body itself never leaves the store (N5, §4.12).
fn outbox_item(message: &governor_core::delivery::OutboxMessage) -> Value {
    let mut item = json!({
        "seq": message.seq,
        "messageKey": message.message_key.0,
        "state": message.state.as_str(),
        "bodyDigest": hex(&message.body_digest),
        "bodyBytes": match &message.body {
            MessageBody::Inline(text) => Some(text.len()),
            MessageBody::File { .. } => None,
        },
    });
    if let (Some(reason), Some(object)) = (&message.expiry_reason, item.as_object_mut()) {
        object.insert("expiryReason".into(), json!(reason.as_str()));
    }
    item
}

/// One `observe` acceptance item — the question's spec spelling (the
/// `handoff_meets_item` family carries its `item` index), Jev's answer
/// and distribution, and the policy threshold it cleared against.
fn judgment_item(judgment: &governor_core::routing::Judgment) -> Value {
    let mut item = json!({
        "question": judgment.question.as_str(),
        "answer": judgment.answer,
        "probabilities": judgment
            .probabilities
            .iter()
            .map(|(label, p)| (label.clone(), json!(p.0)))
            .collect::<serde_json::Map<String, Value>>(),
    });
    if let Some(object) = item.as_object_mut() {
        if let governor_core::routing::Question::HandoffMeetsItem { item: index } =
            judgment.question
        {
            object.insert("item".into(), json!(index));
        }
        if let Some(threshold) = judgment.threshold {
            object.insert("threshold".into(), json!(threshold));
        }
    }
    item
}

/// The unresolved reason a settled Run carries — `settlement_reason`'s
/// spelling; `None` for every other settlement.
pub(super) fn settlement_reason(run: &governor_core::lifecycle::Run) -> Option<&'static str> {
    match run.settlement {
        Some(Settlement::Unresolved { reason }) => Some(reason.as_str()),
        _ => None,
    }
}

/// `nextCursor` — `{"k": <seq>}` rendered hex-of-JSON, the same
/// opaque-cursor convention `status` uses (§4.12): stable across
/// restarts, resumable strictly after `seq`.
fn encode_cursor(seq: u64) -> String {
    json!({"k": seq})
        .to_string()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _unused = write!(out, "{byte:02x}");
            out
        })
}

/// The inverse of `encode_cursor`; `None` on any deviation — a malformed
/// cursor is `REQUEST_INVALID`, never silently rewound (§4.12).
fn decode_cursor(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(bytes.len().saturating_div(2));
    for &[hi, lo] in bytes.as_chunks::<2>().0 {
        let byte = hex_value(hi)?.saturating_mul(16);
        decoded.push(byte.saturating_add(hex_value(lo)?));
    }
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value.get("k")?.as_u64()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').saturating_add(10)),
        _ => None,
    }
}

/// Lowercase hex of a digest — the same convention `handoff` renders.
fn hex(digest: &Digest) -> String {
    digest.0.iter().fold(String::new(), |mut out, byte| {
        let _unused = write!(out, "{byte:02x}");
        out
    })
}
