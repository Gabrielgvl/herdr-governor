//! `reads` — the store's typed read API (`impl Store` methods, spec §9):
//! every query returns core types, never a `rusqlite::Row`, and a read never
//! writes. Caller-owned rows join `callers` under role-prefixed aliases so
//! the surrogate id stays inside the store. Result ordering is stable
//! (time, then id) so callers can page and compare deterministically.

use rusqlite::{Params as SqlParams, Row, params};

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{Capability, OperatingPointId, Qualification};
use governor_core::delivery::{MailboxEvent, OutboxMessage, OutboxState};
use governor_core::identity::{
    CallerBinding, CallerKey, Digest, EffectKey, EventId, IdempotencyKey, JudgmentSetId, LaunchId,
    PaneId, ProjectRoot, RelayInstanceId, RunId, Timestamp,
};
use governor_core::lifecycle::{Effect, EffectKind, EffectState, Run, State};
use governor_core::recovery::{Cooldown, RecoveryObligation, RecoveryStatus};
use governor_core::routing::{JudgmentOutcome, JudgmentPurpose, JudgmentRecord};
use governor_core::task::{Launch, LaunchPhase};

use super::rows::caller::{CallerRow, RelayBindingRow, caller_id, key_from_row};
use super::rows::cooldown::CooldownRow;
use super::rows::effect::EffectRow;
use super::rows::handoff::HandoffRow;
use super::rows::hex_encode;
use super::rows::judgment::{JudgmentRow, JudgmentSetRow};
use super::rows::launch::LaunchRow;
use super::rows::mailbox::MailboxRow;
use super::rows::outbox::OutboxRow;
use super::rows::qualification::QualificationRow;
use super::rows::recovery::RecoveryRow;
use super::rows::run::RunRow;
use super::{Store, StoreError};

const LAUNCHES: &str = "SELECT l.*, c.agent_kind AS caller_agent_kind, \
     c.native_session AS caller_native_session \
     FROM launches l JOIN callers c ON c.caller_id = l.caller_id";

const RUNS: &str = "SELECT r.*, c.agent_kind AS owner_agent_kind, \
     c.native_session AS owner_native_session \
     FROM runs r JOIN callers c ON c.caller_id = r.owner_caller_id";

const OUTBOX: &str = "SELECT o.*, c.agent_kind AS sender_agent_kind, \
     c.native_session AS sender_native_session \
     FROM outbox o JOIN callers c ON c.caller_id = o.sender_caller_id";

const HANDOFFS: &str = "SELECT h.*, EXISTS(SELECT 1 FROM judgment_sets j \
     WHERE j.run_id = h.run_id AND j.work_generation = h.work_generation \
     AND j.handoff_digest = h.digest AND j.purpose = ?2 AND j.outcome = ?3) AS assessed \
     FROM handoffs h WHERE h.run_id = ?1 ORDER BY h.work_generation, h.frozen_at, h.digest";

/// `planned` effects a dispatcher may pick up (§4.4): a launch-bound
/// effect (no Run subject) whose Launch is already `done` is excluded (F10
/// defense-in-depth on top of OQ-13's terminal write — a row that bypassed
/// `finish` must still never reach a dispatcher), as is a run-subject
/// effect whose Run is `settled` — except `close` (close/retire effects
/// dispatch against settled subjects by design) and `event:%` hints, which
/// are exempt from both subject gates (§4.8). A Run-bound effect carries
/// its Launch as a subject too (`planned_effect`), and a `launched` Launch
/// is `done` for the Run's whole supervised life — its prompts and
/// follow-ups answer to the Run's settlement, never the Launch's phase.
const READY_EFFECTS: &str = "SELECT e.* FROM effects e \
     LEFT JOIN launches l ON l.launch_id = e.subject_launch_id \
     LEFT JOIN runs r ON r.run_id = e.subject_run_id \
     WHERE e.state = ?1 \
     AND (e.subject_launch_id IS NULL OR e.subject_run_id IS NOT NULL OR l.phase <> ?2 \
          OR e.effect_key LIKE 'event:%') \
     AND (e.subject_run_id IS NULL OR r.state <> ?3 OR e.kind = ?4 OR e.effect_key LIKE 'event:%') \
     ORDER BY e.planned_at, e.effect_id";

/// Unacked events whose derived destination is the caller: a Run event goes
/// to the Run's current owner, a launch-only event to the Launch's caller
/// (Appendix B). The page starts strictly after `cursor` in
/// `(created_at, event_id)` order; a cursor naming no event yields an empty
/// page.
const MAILBOX_UNACKED: &str = "SELECT m.* FROM mailbox m \
     LEFT JOIN runs r ON r.run_id = m.run_id \
     LEFT JOIN launches l ON l.launch_id = m.launch_id \
     WHERE m.acked_at IS NULL AND COALESCE(r.owner_caller_id, l.caller_id) = ?1 \
     AND (?2 IS NULL OR (m.created_at, m.event_id) > \
     (SELECT c.created_at, c.event_id FROM mailbox c WHERE c.event_id = ?2)) \
     ORDER BY m.created_at, m.event_id LIMIT ?3";

/// `owned` — the caller-scoped reads F7 composes (§4.12).
mod owned;

fn launch_from(row: &Row<'_>) -> Result<Launch, StoreError> {
    let caller = key_from_row(
        row,
        "launches",
        "caller_agent_kind",
        "caller_native_session",
    )?;
    LaunchRow::read(row)?.to_core(caller)
}

fn run_from(row: &Row<'_>) -> Result<Run, StoreError> {
    let owner = key_from_row(row, "runs", "owner_agent_kind", "owner_native_session")?;
    RunRow::read(row)?.to_core(owner)
}

fn outbox_from(row: &Row<'_>) -> Result<OutboxMessage, StoreError> {
    let sender = key_from_row(row, "outbox", "sender_agent_kind", "sender_native_session")?;
    OutboxRow::read(row)?.to_core(sender)
}

fn effect_from(row: &Row<'_>) -> Result<Effect, StoreError> {
    EffectRow::read(row)?.to_core()
}

fn mailbox_from(row: &Row<'_>) -> Result<MailboxEvent, StoreError> {
    MailboxRow::read(row)?.to_core()
}

impl Store {
    /// Every row of `sql`, mapped through `map`.
    fn all<T>(
        &self,
        sql: &str,
        params: impl SqlParams,
        map: fn(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_and_then(params, map)?;
        rows.collect()
    }

    /// The first row of `sql`, or `None`.
    fn first<T>(
        &self,
        sql: &str,
        params: impl SqlParams,
        map: fn(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<Option<T>, StoreError> {
        Ok(self.all(sql, params, map)?.into_iter().next())
    }

    /// The Launch admitted under `(caller, project_root, key)` — the F11
    /// idempotency scope — when one exists.
    pub fn launch_by_idempotency(
        &self,
        caller: &CallerKey,
        project_root: &ProjectRoot,
        key: &IdempotencyKey,
    ) -> Result<Option<Launch>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(None);
        };
        let sql = format!(
            "{LAUNCHES} WHERE l.caller_id = ?1 AND l.project_root = ?2 AND l.idempotency_key = ?3"
        );
        self.first(&sql, params![id, project_root.0, key.0], launch_from)
    }

    /// The Launch `task.recovery_of` names `predecessor` for — the F21
    /// successor in ANY `(caller, project_root, key)` idempotency scope:
    /// a `recovery:<predecessor>` row admitted under another root is
    /// invisible to `launch_by_idempotency` but is still the one
    /// recovery the predecessor is allowed.
    pub fn recovery_successor(&self, predecessor: &RunId) -> Result<Option<Launch>, StoreError> {
        let sql = format!("{LAUNCHES} WHERE json_extract(l.task_json, '$.recovery_of') = ?1");
        self.first(&sql, params![predecessor.0], launch_from)
    }

    /// The Launch with `id`.
    pub fn launch(&self, id: &LaunchId) -> Result<Option<Launch>, StoreError> {
        let sql = format!("{LAUNCHES} WHERE l.launch_id = ?1");
        self.first(&sql, params![id.0], launch_from)
    }

    /// Every Launch in `phase`, oldest first.
    pub fn launches_in_phase(&self, phase: LaunchPhase) -> Result<Vec<Launch>, StoreError> {
        let sql = format!("{LAUNCHES} WHERE l.phase = ?1 ORDER BY l.created_at, l.launch_id");
        self.all(&sql, params![phase.as_str()], launch_from)
    }

    /// The Run with `id`.
    pub fn run(&self, id: &RunId) -> Result<Option<Run>, StoreError> {
        let sql = format!("{RUNS} WHERE r.run_id = ?1");
        self.first(&sql, params![id.0], run_from)
    }

    /// The Run of `launch` (one per Launch).
    pub fn run_by_launch(&self, launch: &LaunchId) -> Result<Option<Run>, StoreError> {
        let sql = format!("{RUNS} WHERE r.launch_id = ?1");
        self.first(&sql, params![launch.0], run_from)
    }

    /// Every Run not yet `settled`, oldest first.
    pub fn unsettled_runs(&self) -> Result<Vec<Run>, StoreError> {
        let sql = format!("{RUNS} WHERE r.state <> ?1 ORDER BY r.created_at, r.run_id");
        self.all(&sql, params![State::Settled.as_str()], run_from)
    }

    /// The effect journal of `run`, in plan order.
    pub fn journal(&self, run: &RunId) -> Result<Vec<Effect>, StoreError> {
        self.all(
            "SELECT * FROM effects WHERE subject_run_id = ?1 ORDER BY planned_at, effect_id",
            params![run.0],
            effect_from,
        )
    }

    /// Every effect in `state`, in plan order.
    pub fn effects_in_state(&self, state: EffectState) -> Result<Vec<Effect>, StoreError> {
        self.all(
            "SELECT * FROM effects WHERE state = ?1 ORDER BY planned_at, effect_id",
            params![state.as_str()],
            effect_from,
        )
    }

    /// The effect journaled under `key`.
    pub fn effect(&self, key: &EffectKey) -> Result<Option<Effect>, StoreError> {
        self.first(
            "SELECT * FROM effects WHERE effect_key = ?1",
            params![key.0],
            effect_from,
        )
    }

    /// The `planned` effects eligible for dispatch — see `READY_EFFECTS`.
    pub fn ready_effects(&self) -> Result<Vec<Effect>, StoreError> {
        self.all(
            READY_EFFECTS,
            params![
                EffectState::Planned.as_str(),
                LaunchPhase::Done.as_str(),
                State::Settled.as_str(),
                EffectKind::Close.as_str()
            ],
            effect_from,
        )
    }

    /// The frozen handoffs of `run` with the derived `assessed` flag (F24).
    pub fn handoffs(&self, run: &RunId) -> Result<Vec<FrozenHandoff>, StoreError> {
        self.all(
            HANDOFFS,
            params![
                run.0,
                JudgmentPurpose::Acceptance.as_str(),
                JudgmentOutcome::Answered.as_str()
            ],
            |row| HandoffRow::read(row)?.to_core(),
        )
    }

    /// The judgment set `id` with its per-question rows, in question order
    /// — what a `Judgments` receipt committed.
    pub fn judgment_record(
        &self,
        id: &JudgmentSetId,
    ) -> Result<Option<JudgmentRecord>, StoreError> {
        let Some(set) = self.first(
            "SELECT * FROM judgment_sets WHERE set_id = ?1",
            params![id.0],
            |row| JudgmentSetRow::read(row)?.to_core(),
        )?
        else {
            return Ok(None);
        };
        let judgments = self.all(
            "SELECT * FROM judgments WHERE set_id = ?1 ORDER BY question",
            params![id.0],
            |row| JudgmentRow::read(row)?.to_core(),
        )?;
        Ok(Some(JudgmentRecord { set, judgments }))
    }

    /// The follow-ups of `run` in `seq` order (F17).
    pub fn outbox(&self, run: &RunId) -> Result<Vec<OutboxMessage>, StoreError> {
        let sql = format!("{OUTBOX} WHERE o.run_id = ?1 ORDER BY o.seq");
        self.all(&sql, params![run.0], outbox_from)
    }

    /// Every `queued` follow-up across Runs — the candidates the delivery
    /// rules consider — by Run, then `seq`.
    pub fn outbox_pending(&self) -> Result<Vec<OutboxMessage>, StoreError> {
        let sql = format!("{OUTBOX} WHERE o.state = ?1 ORDER BY o.run_id, o.seq");
        self.all(&sql, params![OutboxState::Queued.as_str()], outbox_from)
    }

    /// Up to `limit` unacked mailbox events addressed to `caller`, after
    /// `cursor` — see `MAILBOX_UNACKED`. An unknown caller has no events.
    pub fn mailbox_unacked(
        &self,
        caller: &CallerKey,
        cursor: Option<&EventId>,
        limit: u32,
    ) -> Result<Vec<MailboxEvent>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(Vec::new());
        };
        let after = cursor.map(|event| event.0.as_str());
        self.all(
            MAILBOX_UNACKED,
            params![id, after, i64::from(limit)],
            mailbox_from,
        )
    }

    /// The mailbox event with `id`.
    pub fn mailbox_event(&self, id: &EventId) -> Result<Option<MailboxEvent>, StoreError> {
        self.first(
            "SELECT * FROM mailbox WHERE event_id = ?1",
            params![id.0],
            mailbox_from,
        )
    }

    /// Unacked mailbox events with no `event:<id>:hint` effect journaled —
    /// the §4.7 step-6 hint scan (F18's once-only rule: a journaled row
    /// in any state counts as hinted, since a hint is never retried).
    /// `created_at` order is the rate limiter's fairness order.
    pub fn mailbox_unhinted(&self) -> Result<Vec<MailboxEvent>, StoreError> {
        self.all(
            "SELECT m.* FROM mailbox m \
             WHERE m.acked_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM effects e \
             WHERE e.effect_key = 'event:' || m.event_id || ':hint') \
             ORDER BY m.created_at, m.event_id",
            [],
            mailbox_from,
        )
    }

    /// Every provider cooldown, by provider.
    pub fn cooldowns(&self) -> Result<Vec<Cooldown>, StoreError> {
        self.all("SELECT * FROM cooldowns ORDER BY provider", [], |row| {
            CooldownRow::read(row)?.to_core()
        })
    }

    /// Every recovery obligation in `state`, oldest first.
    pub fn recoveries_by_state(
        &self,
        state: RecoveryStatus,
    ) -> Result<Vec<RecoveryObligation>, StoreError> {
        self.all(
            "SELECT * FROM recoveries WHERE state = ?1 ORDER BY created_at, predecessor_run_id",
            params![state.as_str()],
            |row| RecoveryRow::read(row)?.to_core(),
        )
    }

    /// The qualification row for `(operating_point, args_digest, capability)`
    /// (F26).
    pub fn qualification(
        &self,
        operating_point: &OperatingPointId,
        args_digest: Digest,
        capability: &Capability,
    ) -> Result<Option<Qualification>, StoreError> {
        self.first(
            "SELECT * FROM qualifications WHERE operating_point_id = ?1 \
             AND args_digest = ?2 AND capability = ?3",
            params![operating_point.0, hex_encode(args_digest), capability.0],
            |row| QualificationRow::read(row)?.to_core(),
        )
    }

    /// Whether `key` is a known caller — `Some(first_seen_at)` when it is.
    pub fn caller(&self, key: &CallerKey) -> Result<Option<Timestamp>, StoreError> {
        self.first(
            "SELECT * FROM callers WHERE agent_kind = ?1 AND native_session = ?2",
            params![key.agent_kind.0, key.native_session.0],
            |row| CallerRow::read(row)?.first_seen_at(),
        )
    }

    /// The pane `caller` most recently bound from — the latest
    /// `relay_bindings.pane_id_at_bind` by `bound_at` (ties break on the
    /// relay id). The prompt envelope's sender pane and the hint/tab
    /// re-resolve read it; `None` for a caller with no binding.
    pub fn caller_pane(&self, caller: &CallerKey) -> Result<Option<PaneId>, StoreError> {
        let Some(id) = caller_id(&self.conn, caller)? else {
            return Ok(None);
        };
        self.first(
            "SELECT pane_id_at_bind FROM relay_bindings WHERE caller_id = ?1 \
             ORDER BY bound_at DESC, relay_instance_id DESC LIMIT 1",
            params![id],
            |row| Ok(PaneId(row.get::<_, String>(0)?)),
        )
    }

    /// The persisted binding of `relay_instance` (ADR-0004).
    pub fn relay_binding(
        &self,
        relay_instance: &RelayInstanceId,
    ) -> Result<Option<CallerBinding>, StoreError> {
        self.first(
            "SELECT b.*, c.agent_kind AS caller_agent_kind, \
             c.native_session AS caller_native_session \
             FROM relay_bindings b JOIN callers c ON c.caller_id = b.caller_id \
             WHERE b.relay_instance_id = ?1",
            params![relay_instance.0],
            |row| {
                let caller = key_from_row(
                    row,
                    "relay_bindings",
                    "caller_agent_kind",
                    "caller_native_session",
                )?;
                Ok(RelayBindingRow::read(row)?.to_core(caller))
            },
        )
    }
}
