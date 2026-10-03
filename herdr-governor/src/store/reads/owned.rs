//! `owned` — the caller-scoped reads F7's `herdr_status` composes
//! (§4.12): Runs and pending recoveries owned through their caller, the
//! outbox page, one destined mailbox event, and the answered acceptance
//! verdict. Caller-owned rows join `callers` under role-prefixed aliases
//! so the surrogate id stays inside the store; an unknown caller owns
//! nothing.

use rusqlite::params;

use governor_core::delivery::{MailboxEvent, OutboxMessage};
use governor_core::identity::{CallerKey, EventId, RunId};
use governor_core::lifecycle::{EffectKind, EffectState, Run, Settlement, State};
use governor_core::recovery::{RecoveryObligation, RecoveryStatus};
use governor_core::routing::{JudgmentOutcome, JudgmentPurpose, JudgmentRecord};

use super::{
    JudgmentSetRow, OUTBOX, RUNS, RecoveryRow, Store, StoreError, caller_id, mailbox_from,
    outbox_from, run_from,
};

/// `(created_at, run_id)` strictly after `cursor` (MAILBOX_UNACKED's
/// resume-by-id pattern).
const RUNS_OWNED: &str = "WHERE r.owner_caller_id = ?1 \
     AND (r.state <> ?2 \
     OR (r.settlement = ?3 AND EXISTS( \
         SELECT 1 FROM effects e \
         WHERE e.subject_run_id = r.run_id AND e.kind = ?4 \
         AND e.state IN (?5, ?6)))) \
     AND (?7 IS NULL OR (r.created_at, r.run_id) > \
     (SELECT c.created_at, c.run_id FROM runs c WHERE c.run_id = ?7)) \
     ORDER BY r.created_at, r.run_id LIMIT ?8";

/// `pending` recoveries owned through their predecessor Run's current
/// owner (a handover/adopt re-homes the obligation with the Run), paged
/// by `(created_at, predecessor_run_id)` strictly after `cursor` — the
/// predecessor id doubles as the keyset key.
const RECOVERIES_OWNED: &str = "SELECT rc.* FROM recoveries rc \
     JOIN runs r ON r.run_id = rc.predecessor_run_id \
     WHERE rc.state = ?1 AND r.owner_caller_id = ?2 \
     AND (?3 IS NULL OR (rc.created_at, rc.predecessor_run_id) > \
     (SELECT c.created_at, c.predecessor_run_id FROM recoveries c \
     WHERE c.predecessor_run_id = ?3)) \
     ORDER BY rc.created_at, rc.predecessor_run_id LIMIT ?4";

/// One mailbox event fetched for its derived destination only — F7's
/// `eventId` fetch: the same owner rule as `MAILBOX_UNACKED`, so a caller
/// can never read another caller's event body (not even existence — an
/// undestined or absent id both yield `None`).
const MAILBOX_DESTINED: &str = "SELECT m.* FROM mailbox m \
     LEFT JOIN runs r ON r.run_id = m.run_id \
     LEFT JOIN launches l ON l.launch_id = m.launch_id \
     WHERE m.event_id = ?2 AND COALESCE(r.owner_caller_id, l.caller_id) = ?1";

/// The newest *answered* acceptance judgment set of `run` (F24): a stale
/// or still-open later round does not shadow the verdict that counted.
const LATEST_ACCEPTANCE: &str = "SELECT * FROM judgment_sets \
     WHERE run_id = ?1 AND purpose = ?2 AND outcome = ?3 \
     ORDER BY requested_at DESC, set_id DESC LIMIT 1";

impl Store {
    /// Up to `limit` of `caller`'s Runs for F7's runs section — unsettled
    /// Runs plus `accepted` Runs still tracked for retirement — strictly
    /// after `cursor` in `(created_at, run_id)` order (see `RUNS_OWNED`).
    /// An unknown caller owns none.
    pub fn unsettled_runs_owned_by(
        &self,
        caller: &CallerKey,
        cursor: Option<&RunId>,
        limit: u32,
    ) -> Result<Vec<Run>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(Vec::new());
        };
        let after = cursor.map(|run| run.0.as_str());
        let sql = format!("{RUNS} {RUNS_OWNED}");
        self.all(
            &sql,
            params![
                id,
                State::Settled.as_str(),
                Settlement::Accepted.as_str(),
                EffectKind::Close.as_str(),
                EffectState::Planned.as_str(),
                EffectState::Dispatching.as_str(),
                after,
                i64::from(limit)
            ],
            run_from,
        )
    }

    /// Up to `limit` `pending` recoveries owned through their predecessor
    /// Run's current owner, strictly after `cursor` (a predecessor id) in
    /// `(created_at, predecessor_run_id)` order — see `RECOVERIES_OWNED`.
    /// An unknown caller owns none.
    pub fn recoveries_pending_owned_by(
        &self,
        caller: &CallerKey,
        cursor: Option<&RunId>,
        limit: u32,
    ) -> Result<Vec<RecoveryObligation>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(Vec::new());
        };
        let after = cursor.map(|run| run.0.as_str());
        self.all(
            RECOVERIES_OWNED,
            params![
                RecoveryStatus::Pending.as_str(),
                id,
                after,
                i64::from(limit)
            ],
            |row| RecoveryRow::read(row)?.to_core(),
        )
    }

    /// Up to `limit` outbox entries of `run` strictly after `seq`, in
    /// `seq` order — F7/F17's `observe` page (`outbox_page(run, after,
    /// 200)`); bodies ride the row, the renderer drops them.
    pub fn outbox_page(
        &self,
        run: &RunId,
        after: Option<u64>,
        limit: u32,
    ) -> Result<Vec<OutboxMessage>, StoreError> {
        let sql = format!(
            "{OUTBOX} WHERE o.run_id = ?1 AND (?2 IS NULL OR o.seq > ?2) \
             ORDER BY o.seq LIMIT ?3"
        );
        let after_seq = after.map(|seq| i64::try_from(seq).unwrap_or(i64::MAX));
        self.all(
            &sql,
            params![run.0, after_seq, i64::from(limit)],
            outbox_from,
        )
    }

    /// The mailbox event `id` when its derived destination is `caller`
    /// (the `MAILBOX_UNACKED` owner rule, ack state aside); `None` when
    /// absent or owned by someone else — no existence oracle (F7/F4).
    pub fn mailbox_event_destined_to(
        &self,
        caller: &CallerKey,
        event: &EventId,
    ) -> Result<Option<MailboxEvent>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(None);
        };
        self.first(MAILBOX_DESTINED, params![id, event.0], mailbox_from)
    }

    /// The newest *answered* acceptance judgment set of `run` with its
    /// per-question rows — F7/F24's verdict record (see
    /// `LATEST_ACCEPTANCE`).
    pub fn latest_acceptance(&self, run: &RunId) -> Result<Option<JudgmentRecord>, StoreError> {
        let Some(set) = self.first(
            LATEST_ACCEPTANCE,
            params![
                run.0,
                JudgmentPurpose::Acceptance.as_str(),
                JudgmentOutcome::Answered.as_str()
            ],
            |row| JudgmentSetRow::read(row)?.to_core(),
        )?
        else {
            return Ok(None);
        };
        self.judgment_record(&set.id)
    }
}
