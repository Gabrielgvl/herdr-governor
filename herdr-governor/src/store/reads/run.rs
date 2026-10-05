//! `run` — the run-scoped reads F6's `herdr_run` composes (§6.2): the
//! unacked mailbox events a settled Run's adoption may be claimed for,
//! and the `pending` recovery obligation a predecessor carries (F19's
//! "unread events or a pending recovery" adoptability rule).

use rusqlite::params;

use governor_core::delivery::MailboxEvent;
use governor_core::identity::RunId;
use governor_core::recovery::{RecoveryObligation, RecoveryStatus};

use super::{RecoveryRow, Store, StoreError, mailbox_from};

/// F19 — the mailbox events still bound to `run` that no owner has
/// acknowledged: adoption redirects them to the claimant (H#91), so a
/// settled Run stays adoptable while one is unread.
const UNACKED_FOR_RUN: &str = "SELECT m.* FROM mailbox m \
     WHERE m.run_id = ?1 AND m.acked_at IS NULL \
     ORDER BY m.created_at, m.event_id";

/// F19/F21 — the `pending` obligation `run` is predecessor to (one at
/// most — `predecessor_run_id` is the primary key).
const PENDING_FOR_RUN: &str = "SELECT rc.* FROM recoveries rc \
     WHERE rc.predecessor_run_id = ?1 AND rc.state = ?2";

/// F21 — the obligation `run` is predecessor to in ANY state (one at
/// most): a `blocked`/`failed`/`dispatched` row is still the one
/// recovery that predecessor is allowed, so a second `recoveryOf`
/// meets `RECOVERY_EXISTS`, never an upsert conflict.
const OBLIGATION_FOR_RUN: &str = "SELECT rc.* FROM recoveries rc \
     WHERE rc.predecessor_run_id = ?1";

impl Store {
    /// The mailbox events bound to `run` and still unacknowledged —
    /// F19's "unread events" an adoption may be claimed for.
    pub fn unacked_events_for_run(&self, run: &RunId) -> Result<Vec<MailboxEvent>, StoreError> {
        self.all(UNACKED_FOR_RUN, params![run.0], mailbox_from)
    }

    /// The `pending` recovery obligation `run` is the predecessor of, or
    /// `None` — F19's other adoptability claim.
    pub fn pending_recovery_for_run(
        &self,
        run: &RunId,
    ) -> Result<Option<RecoveryObligation>, StoreError> {
        self.first(
            PENDING_FOR_RUN,
            params![run.0, RecoveryStatus::Pending.as_str()],
            |row| RecoveryRow::read(row)?.to_core(),
        )
    }

    /// The obligation `run` is the predecessor of in ANY state, or
    /// `None` — the `recoveryOf` admission's one-recovery-per-
    /// predecessor read; a terminal row is still `RECOVERY_EXISTS`.
    pub fn recovery_for_run(&self, run: &RunId) -> Result<Option<RecoveryObligation>, StoreError> {
        self.first(OBLIGATION_FOR_RUN, params![run.0], |row| {
            RecoveryRow::read(row)?.to_core()
        })
    }
}
