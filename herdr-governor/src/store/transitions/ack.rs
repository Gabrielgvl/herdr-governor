//! F6/F18 — `ack`: stamps `mailbox.acked_at` once; a second ack (or an
//! unknown event id) changes nothing and is not an error.

use rusqlite::{Transaction, params};

use governor_core::identity::{EventId, Timestamp};

use crate::store::error::ApplyError;
use crate::store::transitions::{execute, stamp};

const ACK: &str = "UPDATE mailbox SET acked_at = ?1 WHERE event_id = ?2 AND acked_at IS NULL";

pub(super) fn apply(
    tx: &Transaction<'_>,
    event: &EventId,
    now: Timestamp,
) -> Result<(), ApplyError> {
    execute(
        tx,
        ACK,
        params![stamp(now, "mailbox", "acked_at")?, event.0],
    )?;
    Ok(())
}
