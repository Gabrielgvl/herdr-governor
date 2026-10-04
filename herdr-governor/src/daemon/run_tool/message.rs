//! `message` — F17's owner-bound follow-up admission, moved to its own
//! module so `run_tool.rs` stays under the file-size bound.

use governor_core::delivery::{
    FollowUpWrite, MessageBody, OutboxState, enqueue_follow_up, follow_up_body_needs_file,
    follow_up_body_too_large,
};
use governor_core::identity::{CallerKey, Digest, MessageKey, RunId, Timestamp};
use governor_core::lifecycle::{StateChange, Transition};
use governor_core::task::Refusal;
use serde_json::json;
use sha2::Digest as _;

use crate::daemon::api::{ToolError, ToolResponse};
use crate::daemon::coordinator::apply::apply_with_retry;
use crate::daemon::coordinator::empty;
use crate::daemon::delivery::publish_body;
use crate::daemon::paths::Paths;
use crate::store::Store;

use super::unavailable;

/// F17 — `message {runId, messageKey, text}`: the caller must own the
/// Run (`NOT_OWNER` — a Run the store does not know has no owner), a
/// settled Run refuses `RUN_SETTLED`, and `(run, messageKey)` dedups on
/// the body digest — a repeat of the same text returns the existing
/// `seq`, a different text refuses `MESSAGE_KEY_CONFLICT`. A body over
/// the 16 KiB inline bound publishes first; anything over 1 MiB refuses
/// `REQUEST_INVALID` (the domain bound the strict schema cannot
/// express). The answer is the entry's sequence and delivery state.
pub(super) fn enqueue(
    store: &mut Store,
    paths: &Paths,
    caller: &CallerKey,
    run_id: &RunId,
    key: &MessageKey,
    text: &str,
    now: Timestamp,
) -> ToolResponse {
    if follow_up_body_too_large(text.len()) {
        return Err(ToolError::new(
            ToolError::REQUEST_INVALID,
            "follow-up body exceeds the 1 MiB file bound",
        ));
    }
    let Some(run) = store.run(run_id).map_err(|_err| unavailable())? else {
        return Err(ToolError::refusal(
            Refusal::NotOwner,
            "the caller does not own this run",
        ));
    };
    let outbox = store.outbox(run_id).map_err(|_err| unavailable())?;
    let digest = Digest(sha2::Sha256::digest(text.as_bytes()).into());
    // The admission probe — `enqueue_follow_up` checks owner, settlement
    // and the key's dedup; the apply below re-runs it on fresh state.
    let (seq, row) = enqueue_follow_up(
        &run,
        &outbox,
        caller,
        key,
        MessageBody::Inline(text.to_owned()),
        digest,
    )
    .map_err(|refusal| ToolError::refusal(refusal, "follow-up refused"))?;
    if row.is_none() {
        // Same key + same body — the entry already exists; its seq and
        // current state are the answer (F17 idempotency).
        let state = outbox
            .iter()
            .find(|m| m.message_key == *key)
            .map_or(OutboxState::Queued, |m| m.state);
        return Ok(json!({"seq": seq, "state": state.as_str()}));
    }
    // Body placement precedes the enqueue — the transition commits only
    // with the file already verified (H#62–64).
    let body = if follow_up_body_needs_file(text.len()) {
        let path = publish_body(paths, run_id, seq, text, &digest).map_err(|_io| {
            ToolError::new(
                ToolError::FOLLOWUP_PUBLISH_FAILED,
                "follow-up body publication failed",
            )
        })?;
        MessageBody::File { path }
    } else {
        MessageBody::Inline(text.to_owned())
    };
    // The enqueue write — recomputed against fresh state per attempt.
    // `landed`/`replay` record what the last recompute saw: the write, a
    // mid-apply dedup hit, or a moved gate (→ `DAEMON_UNAVAILABLE`, the
    // honest answer to state changing under the call).
    let mut landed = false;
    let mut replay: Option<(u64, OutboxState)> = None;
    apply_with_retry(store, now, |st| {
        let Some(current) = st.run(run_id).ok().flatten() else {
            return empty();
        };
        let queue = st.outbox(run_id).unwrap_or_default();
        match enqueue_follow_up(&current, &queue, caller, key, body.clone(), digest) {
            Ok((s, Some(entry))) if s == seq => {
                landed = true;
                Transition {
                    state_changes: vec![StateChange::WriteFollowUp(FollowUpWrite::Enqueue(entry))],
                    events: Vec::new(),
                    effects: Vec::new(),
                }
            }
            Ok((s, None)) => {
                replay = queue
                    .iter()
                    .find(|m| m.message_key == *key)
                    .map(|m| (s, m.state));
                empty()
            }
            _ => empty(),
        }
    })
    .map_err(|_apply| ToolError::new(ToolError::DAEMON_UNAVAILABLE, "follow-up enqueue failed"))?;
    if landed {
        return Ok(json!({"seq": seq, "state": OutboxState::Queued.as_str()}));
    }
    if let Some((replayed, state)) = replay {
        return Ok(json!({"seq": replayed, "state": state.as_str()}));
    }
    Err(ToolError::new(
        ToolError::DAEMON_UNAVAILABLE,
        "follow-up admission raced a state change",
    ))
}
