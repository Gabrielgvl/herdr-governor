//! `transitions` — the only lifecycle-write site in the repo (I10, spec §9):
//! one child module per Appendix-B transaction family, dispatched by
//! `store::apply` inside a single `BEGIN IMMEDIATE … COMMIT`. Order inside
//! the transaction is the transition's own: `state_changes`, then `events`
//! (`mailbox` — the F18 dedup no-op), then `effects` (`planned`,
//! `payload_digest` verbatim — the N1 replan dedup). The dedup inserts
//! skip their row only when its idempotency key is already stored
//! ([`insert_dedup`]); every other constraint, and a conditional write
//! that matches no row, returns a typed [`ApplyError`] and the caller's
//! transaction rolls back whole.
//!
//! Every statement is followed by [`crash_checkpoint`] — the one deliberate
//! test-only production seam (P4.S3): env-gated, a no-op in production.

mod ack;
mod bind_caller;
mod change_owner;
mod follow_up;
mod handoff;
mod qualification;
mod record_launch;
mod recovery;
mod reserve_run;
mod update_run;
mod write_effect;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::types::Value as SqlValue;
use rusqlite::{Params as SqlParams, Transaction, ffi, params_from_iter};

use governor_core::config::Qualification;
use governor_core::delivery::{FollowUpWrite, OutboxState};
use governor_core::identity::{CallerKey, Timestamp};
use governor_core::lifecycle::{EffectState, StateChange, Transition};

use super::error::{ApplyError, ConflictKind};
use super::rows::caller::caller_id;
use super::rows::effect::EffectRow;
use super::rows::mailbox::MailboxRow;
use super::rows::{Params, ts_encode};

/// Refuses a transition the vocabulary forbids before any transaction
/// opens: an outbox enqueue that is not `queued`, a `queued` row carrying
/// an `effect` or `expiry_reason` (Appendix B gives it neither), an outbox
/// resolution to a state that is not `submitted`/`unconfirmed` (F17/F9 —
/// the only edges), or a planned effect that is not `planned`. A journal
/// write needs no check: [`governor_core::lifecycle::EffectWrite`] makes a
/// `failed` without a certainty, a `planned` write and a receipt on a
/// failure unrepresentable (OQ-11/OQ-13 — the shape is the core's
/// decision, never the store's).
pub(super) fn check_well_formed(transition: &Transition) -> Result<(), ApplyError> {
    for change in &transition.state_changes {
        let (key, reason) = match change {
            StateChange::WriteFollowUp(FollowUpWrite::Enqueue(message))
                if message.state != OutboxState::Queued =>
            {
                (
                    follow_up::entry_key(&message.run, message.seq),
                    "a follow-up enqueues queued",
                )
            }
            StateChange::WriteFollowUp(FollowUpWrite::Enqueue(message))
                if message.effect.is_some() || message.expiry_reason.is_some() =>
            {
                (
                    follow_up::entry_key(&message.run, message.seq),
                    "a queued follow-up carries no effect or expiry reason",
                )
            }
            StateChange::WriteFollowUp(FollowUpWrite::Resolve { run, seq, state })
                if !matches!(state, OutboxState::Submitted | OutboxState::Unconfirmed) =>
            {
                (
                    follow_up::entry_key(run, *seq),
                    "a follow-up resolves to submitted or unconfirmed",
                )
            }
            StateChange::WriteEffect(_)
            | StateChange::WriteFollowUp(_)
            | StateChange::BindCaller(_)
            | StateChange::RecordLaunch(_)
            | StateChange::ReserveRun(_)
            | StateChange::UpdateRun(_)
            | StateChange::ChangeOwner(_)
            | StateChange::ExpireFollowUps { .. }
            | StateChange::RecordRecovery(_)
            | StateChange::SetCooldown(_)
            | StateChange::FreezeHandoff(_)
            | StateChange::AckEvent(_) => continue,
        };
        return Err(ApplyError::MalformedWrite { key, reason });
    }
    if let Some(effect) = transition
        .effects
        .iter()
        .find(|effect| effect.state != EffectState::Planned)
    {
        return Err(ApplyError::MalformedWrite {
            key: effect.key.0.clone(),
            reason: "a planned effect must be journaled planned",
        });
    }
    Ok(())
}

/// Runs the whole transition inside `tx`; the caller commits.
pub(super) fn apply_in(
    tx: &Transaction<'_>,
    transition: &Transition,
    now: Timestamp,
) -> Result<(), ApplyError> {
    for change in &transition.state_changes {
        match change {
            StateChange::BindCaller(binding) => bind_caller::apply(tx, binding, now)?,
            StateChange::RecordLaunch(launch) => record_launch::apply(tx, launch, now)?,
            StateChange::ReserveRun(run) => reserve_run::apply(tx, run, now)?,
            StateChange::UpdateRun(update) => update_run::apply(tx, update, now)?,
            StateChange::ChangeOwner(owner) => change_owner::apply(tx, owner, now)?,
            StateChange::WriteEffect(write) => write_effect::apply(tx, write, now)?,
            StateChange::WriteFollowUp(write) => follow_up::apply(tx, write, now)?,
            StateChange::ExpireFollowUps { run, reason } => {
                follow_up::expire(tx, run, *reason, now)?;
            }
            StateChange::RecordRecovery(obligation) => recovery::record(tx, obligation, now)?,
            StateChange::SetCooldown(cooldown) => recovery::set_cooldown(tx, cooldown, now)?,
            StateChange::FreezeHandoff(frozen) => handoff::apply(tx, frozen)?,
            StateChange::AckEvent(event) => ack::apply(tx, event, now)?,
        }
    }
    for event in &transition.events {
        let row = MailboxRow::from_core(event, now, None)?;
        insert_dedup(tx, "mailbox", &row.params(), &["dedup_key"])?;
    }
    for effect in &transition.effects {
        let row = EffectRow::from_core(effect, now, None)?;
        insert_dedup(tx, "effects", &row.params(), &["effect_key"])?;
    }
    Ok(())
}

/// F26 — the `qualify` driver's row write (plan §10: the table's writer is
/// S3, the driver is Phase 6): re-qualifying the same
/// `(operating point, args digest, capability)` replaces the verdict.
pub(super) fn record_qualification(
    tx: &Transaction<'_>,
    qualification: &Qualification,
    now: Timestamp,
) -> Result<(), ApplyError> {
    qualification::apply(tx, qualification, now)
}

/// Runs `sql` with `params` and passes the crash checkpoint.
pub(super) fn execute(
    tx: &Transaction<'_>,
    sql: &str,
    params: impl SqlParams,
) -> Result<usize, ApplyError> {
    let changed = tx.execute(sql, params)?;
    crash_checkpoint();
    Ok(changed)
}

/// The `(col, col)` column list and `?1, ?2` slots a row's params bind.
fn columns_slots(params: &Params) -> (String, String) {
    let columns = params
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>()
        .join(", ");
    let slots = (1..)
        .take(params.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    (columns, slots)
}

/// `<verb> INTO <table> (cols) VALUES (?…) <tail>` for a row's params.
fn insert_sql(verb: &str, table: &str, params: &Params, tail: &str) -> String {
    let (columns, slots) = columns_slots(params);
    format!("{verb} INTO {table} ({columns}) VALUES ({slots}) {tail}")
}

/// Runs [`insert_sql`] with the row's values bound in column order.
pub(super) fn insert(
    tx: &Transaction<'_>,
    verb: &str,
    table: &str,
    params: &Params,
    tail: &str,
) -> Result<usize, ApplyError> {
    let sql = insert_sql(verb, table, params, tail);
    execute(tx, &sql, params_from_iter(params.iter().map(|(_, v)| v)))
}

/// `INSERT … ON CONFLICT(<keys>) DO NOTHING` — the idempotent insert: a
/// row whose idempotency key is already stored is a silent no-op (SQLite
/// checks the upsert target's index first, so an exact replay never trips
/// the primary key), while every other violation — a primary key colliding
/// under a different key, CHECK, NOT NULL, FOREIGN KEY — still aborts the
/// transaction as a typed [`ApplyError`]. `keys` must name a PRIMARY KEY or
/// UNIQUE constraint exactly; SQLite refuses the statement otherwise.
pub(super) fn insert_dedup(
    tx: &Transaction<'_>,
    table: &str,
    params: &Params,
    keys: &[&str],
) -> Result<usize, ApplyError> {
    let tail = format!("ON CONFLICT({}) DO NOTHING", keys.join(", "));
    insert(tx, "INSERT", table, params, &tail)
}

/// A plain `INSERT` whose primary-key / UNIQUE collision is a typed
/// [`ApplyError::Conflict`] of `kind` (the row exists; re-read, don't
/// retry); every other constraint surfaces as [`ApplyError::Constraint`].
pub(super) fn insert_unique(
    tx: &Transaction<'_>,
    table: &str,
    params: &Params,
    kind: ConflictKind,
    key: &str,
) -> Result<(), ApplyError> {
    let sql = insert_sql("INSERT", table, params, "");
    match tx.execute(&sql, params_from_iter(params.iter().map(|(_, v)| v))) {
        Ok(_) => {
            crash_checkpoint();
            Ok(())
        }
        Err(rusqlite::Error::SqliteFailure(ffi::Error { extended_code, .. }, _))
            if extended_code == ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || extended_code == ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Err(ApplyError::Conflict {
                kind,
                key: key.to_owned(),
            })
        }
        Err(err) => Err(err.into()),
    }
}

/// `UPDATE <table> SET c = ?… WHERE <where_sql>`; `where_sql` binds
/// `?1..=?k` for `where_vals`, the SET values follow.
pub(super) fn update(
    tx: &Transaction<'_>,
    table: &str,
    set: &Params,
    where_sql: &str,
    where_vals: &[SqlValue],
) -> Result<usize, ApplyError> {
    let assignments = (1..)
        .skip(where_vals.len())
        .zip(set)
        .map(|(i, (column, _))| format!("{column} = ?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("UPDATE {table} SET {assignments} WHERE {where_sql}");
    let values = where_vals.iter().chain(set.iter().map(|(_, v)| v));
    execute(tx, &sql, params_from_iter(values))
}

/// Keeps only the named columns of a row's params, in row order.
pub(super) fn only(params: Params, keep: &[&str]) -> Params {
    params
        .into_iter()
        .filter(|(column, _)| keep.contains(column))
        .collect()
}

/// Drops the named columns from a row's params.
pub(super) fn without(params: Params, drop: &[&str]) -> Params {
    params
        .into_iter()
        .filter(|(column, _)| !drop.contains(column))
        .collect()
}

/// `now` as the RFC3339 stamp a `TEXT` time column takes.
pub(super) fn stamp(
    now: Timestamp,
    table: &'static str,
    column: &'static str,
) -> Result<SqlValue, ApplyError> {
    Ok(ts_encode(now, table, column)?.into())
}

/// The surrogate `caller_id` of a bound caller; an unbound key is a
/// FOREIGN KEY violation surfaced before SQLite would name it less clearly.
pub(super) fn resolve_caller(tx: &Transaction<'_>, key: &CallerKey) -> Result<i64, ApplyError> {
    caller_id(tx, key)?.ok_or_else(|| ApplyError::Constraint {
        message: format!(
            "caller {}/{} is not bound",
            key.agent_kind.0, key.native_session.0
        ),
    })
}

/// The crash-injection mode, read from the environment once per process.
enum CrashMode {
    /// Production: every checkpoint is a no-op.
    Off,
    /// `GOV_STORE_CRASH_COUNT=1`: count checkpoints, never abort.
    Count,
    /// `GOV_STORE_CRASH_AT=N`: `abort()` when the N-th checkpoint passes.
    AbortAt(usize),
}

static MODE: OnceLock<CrashMode> = OnceLock::new();
static PASSED: AtomicUsize = AtomicUsize::new(0);

fn crash_mode() -> &'static CrashMode {
    MODE.get_or_init(|| {
        if let Some(at) = std::env::var_os("GOV_STORE_CRASH_AT")
            .and_then(|v| v.to_str().and_then(|s| s.parse::<usize>().ok()))
        {
            return CrashMode::AbortAt(at);
        }
        if std::env::var_os("GOV_STORE_CRASH_COUNT").is_some_and(|v| v == "1") {
            return CrashMode::Count;
        }
        CrashMode::Off
    })
}

/// The statement-boundary seam the P4.S4 crash suite drives: a child
/// process aborts (no ROLLBACK, no WAL flush — power loss, not a clean
/// drop) at the chosen boundary. Unset env = no-op.
///
/// ponytail: env-gated kill only, read once; upgrade path is a
/// `failpoints`-style crate if more fault shapes are ever needed.
fn crash_checkpoint() {
    match crash_mode() {
        CrashMode::Off => {}
        CrashMode::Count => {
            PASSED.fetch_add(1, Ordering::SeqCst);
        }
        CrashMode::AbortAt(at) => {
            let passed = PASSED.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            if passed == *at {
                std::process::abort();
            }
        }
    }
}

/// How many statement boundaries this process has passed (meaningful under
/// `GOV_STORE_CRASH_COUNT=1`; the probe prints it to enumerate the matrix).
#[doc(hidden)]
#[must_use]
pub fn crash_checkpoint_count() -> usize {
    PASSED.load(Ordering::SeqCst)
}
