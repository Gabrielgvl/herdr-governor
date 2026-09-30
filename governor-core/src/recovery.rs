//! F21 — recovery obligations and provider cooldowns (ADR-0003): a
//! `provider_limited` settlement creates exactly one obligation; dispatch
//! waits for fresh proof the predecessor's identity is absent. Panes are
//! never closed automatically.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::config::{Policy, Provider};
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{
    CallerKey, ChildStatus, DedupKey, EventId, IdempotencyKey, LaunchId, Observation, RunId,
    Timestamp,
};
use crate::lifecycle::{Run, Settlement, StateChange, Transition};
use crate::task::{AbstainReason, Refusal, Task};

/// F21 — the successor Launch's idempotency key is
/// `"recovery:" ++ predecessorRunId`, which makes one recovery per
/// predecessor and never collides with a caller key.
pub const RECOVERY_KEY_PREFIX: &str = "recovery:";

/// F21/Appendix B `recoveries.origin` — where the obligation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryOrigin {
    /// `provider_limit` — created by a `provider_limited` settlement.
    ProviderLimit,
    /// `caller` — requested with `recoveryOf` on a successor Launch (F21).
    Caller,
}

impl RecoveryOrigin {
    /// Appendix B — the stored spelling of the origin.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProviderLimit => "provider_limit",
            Self::Caller => "caller",
        }
    }
}

/// F21/Appendix B `recoveries.state` — the obligation's lifecycle:
/// `pending` → `dispatched` | `blocked` | `failed`; a still-pending
/// obligation past `expires_at` fails `expired` (recorded in `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryStatus {
    /// `pending` — waiting for fresh proof the predecessor is absent.
    Pending,
    /// `blocked` — the successor Launch abstained, no candidates (F21).
    Blocked,
    /// `dispatched` — the successor Launch was admitted (Appendix B CHECK:
    /// `successor_launch_id` is then required).
    Dispatched,
    /// `failed` — the obligation could not be satisfied (e.g. `expired`).
    Failed,
}

impl RecoveryStatus {
    /// Appendix B — the stored spelling of the state.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Dispatched => "dispatched",
            Self::Failed => "failed",
        }
    }
}

/// F21/Appendix B `recoveries` — one obligation per settled predecessor
/// (`predecessor_run_id` is the primary key — `RECOVERY_EXISTS` refuses a
/// second).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryObligation {
    /// The settled Run it continues.
    pub predecessor: RunId,
    /// `origin`.
    pub origin: RecoveryOrigin,
    /// `state`.
    pub status: RecoveryStatus,
    /// `reason` — e.g. `expired` for a still-pending obligation past
    /// `expires_at` (F21).
    pub reason: Option<String>,
    /// `successor_launch_id` — the Launch the dispatch admitted; required
    /// iff `status` is `dispatched` (Appendix B CHECK).
    pub successor_launch: Option<LaunchId>,
    /// `expires_at` — the policy expiry (default `DEFAULT_RECOVERY_EXPIRY`).
    pub expires_at: Timestamp,
}

/// F21/Appendix B `cooldowns` — one provider's exclusion period. Upserts keep
/// the later `until`: cooldowns only ever lengthen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cooldown {
    /// `provider` — the excluded provider (primary key).
    pub provider: Provider,
    /// `until` — the absolute expiry of the exclusion.
    pub until: Timestamp,
    /// `reason` — why the provider was limited.
    pub reason: String,
    /// `source_run_id` — the Run whose `provider_limited` settlement caused
    /// it, when known.
    pub source_run: Option<RunId>,
}

/// `now + duration`, saturating: an expiry computed past the representable
/// range pins to `i64::MAX` rather than wrapping — the function stays total.
fn after(now: Timestamp, duration: Duration) -> Timestamp {
    let millis = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    Timestamp(now.0.saturating_add(millis))
}

impl RecoveryObligation {
    /// F21 — a freshly recorded obligation: `pending`, unclaimed, expiring
    /// `expiry` after `now` (the policy expiry, `Policy::recovery_expiry`;
    /// default `DEFAULT_RECOVERY_EXPIRY` = 24 h).
    #[must_use]
    pub fn pending(
        predecessor: RunId,
        origin: RecoveryOrigin,
        now: Timestamp,
        expiry: Duration,
    ) -> Self {
        Self {
            predecessor,
            origin,
            status: RecoveryStatus::Pending,
            reason: None,
            successor_launch: None,
            expires_at: after(now, expiry),
        }
    }

    /// F21 — `pending` → `dispatched`: the successor Launch was admitted
    /// (Appendix B CHECK — `successor_launch_id` is then required). The
    /// terminal states absorb: a non-pending obligation returns `None`.
    #[must_use]
    pub fn dispatched(&self, successor: LaunchId) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Dispatched,
                successor_launch: Some(successor),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — `pending` → `blocked`: the successor Launch abstained; `reason`
    /// records the abstention (`no_candidates`, `no_higher_tier`, ...).
    #[must_use]
    pub fn blocked(&self, reason: AbstainReason) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Blocked,
                reason: Some(reason.as_str().into()),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — `pending` → `failed` with `reason` (e.g. `expired`).
    #[must_use]
    pub fn failed(&self, reason: String) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending => Some(Self {
                status: RecoveryStatus::Failed,
                reason: Some(reason),
                ..self.clone()
            }),
            RecoveryStatus::Blocked | RecoveryStatus::Dispatched | RecoveryStatus::Failed => None,
        }
    }

    /// F21 — a still-`pending` obligation at or past `expires_at` fails
    /// `expired`; every other input leaves it untouched.
    #[must_use]
    pub fn expired(&self, now: Timestamp) -> Option<Self> {
        match self.status {
            RecoveryStatus::Pending if now.0 >= self.expires_at.0 => {
                self.failed(String::from("expired"))
            }
            RecoveryStatus::Pending
            | RecoveryStatus::Blocked
            | RecoveryStatus::Dispatched
            | RecoveryStatus::Failed => None,
        }
    }
}

impl Cooldown {
    /// F21 — the exclusion a `provider_limited` settlement records: `until`
    /// is `now + duration` (the policy cooldown window, `Policy::cooldown`).
    #[must_use]
    pub fn limited(
        provider: Provider,
        source_run: RunId,
        now: Timestamp,
        duration: Duration,
    ) -> Self {
        Self {
            provider,
            until: after(now, duration),
            reason: Settlement::ProviderLimited.as_str().into(),
            source_run: Some(source_run),
        }
    }

    /// F21 — merge a newly observed limit into this provider's cooldown:
    /// `until` only ever lengthens (Appendix B upsert keeps `max(existing,
    /// new)`), and the winning exclusion keeps its own `reason`/`source_run`.
    /// Both inputs share the provider — it is the row's key.
    #[must_use]
    pub fn merged(&self, candidate: Cooldown) -> Self {
        if candidate.until > self.until {
            candidate
        } else {
            self.clone()
        }
    }
}

/// F21 — the successor Launch's idempotency key:
/// `"recovery:" ++ predecessorRunId` — one recovery per predecessor, and it
/// never collides with a caller key.
#[must_use]
pub fn successor_key(predecessor: &RunId) -> IdempotencyKey {
    IdempotencyKey(format!("{RECOVERY_KEY_PREFIX}{}", predecessor.0))
}

/// F21 — the successor Task: the predecessor's Task plus the prescribed
/// preamble — continue from the observed git and transcript state, and do not
/// repeat side effects that already happened. `recovery_of` re-keys to the
/// immediate predecessor so routing applies the F13 step-4 recovery minimum
/// and the predecessor's provider exclusion.
#[must_use]
pub fn successor_task(predecessor: &RunId, task: &Task) -> Task {
    const PREAMBLE: &str = "This Task continues a predecessor Run's work. \
        Continue from the observed git and transcript state; do not repeat \
        side effects that already happened.";
    Task {
        objective: format!(
            "{PREAMBLE}\n\npredecessor_run_id: {}\n\n{}",
            predecessor.0, task.objective
        ),
        recovery_of: Some(predecessor.clone()),
        ..task.clone()
    }
}

/// F21/ADR-0003 — the dispatch precondition: only a fresh snapshot showing
/// the predecessor's identity `absent` permits dispatch. `unique` and
/// `invalid` never prove a stop — `invalid` never counts as absence (F3).
#[must_use]
pub fn dispatch_ready(observation: &Observation) -> bool {
    match observation {
        Observation::Absent => true,
        Observation::Unique {
            status: _,
            pane: _,
            native_session: _,
        }
        | Observation::Invalid => false,
    }
}

/// F21 — the recovery share of a `provider_limited` settlement's transaction
/// (Appendix B "Settle"): the unique `pending` obligation, the provider's
/// merged cooldown, and the `cooldown_hit` + `recovery_pending` mailbox
/// events — the latter tells the owner that `cancel` with `closePane`
/// triggers the dispatch (ADR-0003).
#[must_use]
pub fn provider_limited(
    predecessor: &RunId,
    provider: Provider,
    existing_cooldown: Option<&Cooldown>,
    now: Timestamp,
    policy: &Policy,
    cooldown_event: EventId,
    recovery_event: EventId,
) -> Transition {
    let obligation = RecoveryObligation::pending(
        predecessor.clone(),
        RecoveryOrigin::ProviderLimit,
        now,
        policy.recovery_expiry,
    );
    let candidate = Cooldown::limited(provider, predecessor.clone(), now, policy.cooldown);
    let cooldown = match existing_cooldown {
        Some(existing) => existing.merged(candidate),
        None => candidate,
    };
    let events = Vec::from([
        MailboxEvent {
            id: cooldown_event,
            dedup_key: DedupKey(format!("run:{}:cooldown_hit", predecessor.0)),
            subject: MailboxSubject::Run(predecessor.clone()),
            kind: MailboxEventKind::CooldownHit,
            body: format!(
                "{{\"provider\":{},\"until\":{}}}",
                json_str(&cooldown.provider.0),
                cooldown.until.0
            ),
        },
        MailboxEvent {
            id: recovery_event,
            dedup_key: DedupKey(format!("run:{}:recovery_pending", predecessor.0)),
            subject: MailboxSubject::Run(predecessor.clone()),
            kind: MailboxEventKind::RecoveryPending,
            body: format!(
                "{{\"predecessor\":{},\"expires_at\":{},\"message\":{}}}",
                json_str(&predecessor.0),
                obligation.expires_at.0,
                json_str(
                    "close the predecessor's pane (cancel with closePane) to dispatch the recovery"
                )
            ),
        },
    ]);
    Transition {
        state_changes: Vec::from([
            StateChange::RecordRecovery(obligation),
            StateChange::SetCooldown(cooldown),
        ]),
        events,
        effects: Vec::new(),
    }
}

/// F21 — the `recoveryOf` admission rules for a caller-requested recovery:
///
/// - the caller must own the predecessor (F4 — claiming a Run's obligation
///   is a Run operation) — `Err(NOT_OWNER)`;
/// - an existing obligation is claimed iff it is an unclaimed `pending`
///   `provider_limit` one; any other existing obligation is a second
///   recovery of the same predecessor — `Err(RECOVERY_EXISTS)`;
/// - the predecessor must be settled;
/// - a `provider_limited` predecessor must be observed `absent` first
///   (ADR-0003); any other settled predecessor must be observed `idle`,
///   `done` or `absent` — `invalid` never proves anything (F3).
///
/// `Ok(Some(_))` is the obligation to record — `dispatched` and bound to the
/// successor Launch, per Appendix B's "Recovery dispatch" transaction.
/// `Ok(None)` means a settled/observation gate is unmet and nothing may be
/// recorded; the typed-refusal vocabulary names no code for those gates, so
/// the daemon maps `Ok(None)` to its own answer (reported as an escalation).
pub fn caller_admission(
    predecessor: &Run,
    obligation: Option<&RecoveryObligation>,
    observation: &Observation,
    successor: &LaunchId,
    caller: &CallerKey,
    now: Timestamp,
    policy: &Policy,
) -> Result<Option<RecoveryObligation>, Refusal> {
    if predecessor.owner != *caller {
        return Err(Refusal::NotOwner);
    }
    let claim = match obligation {
        Some(existing)
            if existing.origin == RecoveryOrigin::ProviderLimit
                && existing.status == RecoveryStatus::Pending =>
        {
            Some(existing)
        }
        Some(_) => return Err(Refusal::RecoveryExists),
        None => None,
    };
    let Some(settled) = predecessor.settlement else {
        return Ok(None);
    };
    let observed = match settled {
        Settlement::ProviderLimited => dispatch_ready(observation),
        Settlement::Accepted
        | Settlement::Rejected
        | Settlement::NoHandoff
        | Settlement::PaneLost
        | Settlement::Cancelled
        | Settlement::Unresolved { reason: _ } => match observation {
            Observation::Absent => true,
            Observation::Unique {
                status,
                pane: _,
                native_session: _,
            } => match status {
                Some(ChildStatus::Idle | ChildStatus::Done) => true,
                Some(ChildStatus::Working | ChildStatus::Blocked) | None => false,
            },
            Observation::Invalid => false,
        },
    };
    if !observed {
        return Ok(None);
    }
    Ok(match claim {
        Some(existing) => existing.dispatched(successor.clone()),
        None => Some(RecoveryObligation {
            predecessor: predecessor.id.clone(),
            origin: RecoveryOrigin::Caller,
            status: RecoveryStatus::Dispatched,
            reason: None,
            successor_launch: Some(successor.clone()),
            expires_at: after(now, policy.recovery_expiry),
        }),
    })
}

/// Minimal JSON string escaping for mailbox `body_json` interpolation —
/// quotes, backslashes and control characters are escaped so a free-form
/// `Provider` or `RunId` value cannot break the event body.
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < '\u{20}' => {
                let code = u32::from(c);
                out.push_str("\\u00");
                out.push(char::from_digit(code >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit(code & 0xf, 16).unwrap_or('0'));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::time::Duration;

    use super::{
        Cooldown, RECOVERY_KEY_PREFIX, RecoveryObligation, RecoveryOrigin, RecoveryStatus,
        caller_admission, dispatch_ready, provider_limited, successor_key, successor_task,
    };
    use crate::config::{Policy, Provider, Tier};
    use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
    use crate::identity::{
        AgentKind, CallerKey, ChildStatus, DedupKey, EventId, LaunchId, NativeSession, Observation,
        PaneId, RunId, Timestamp,
    };
    use crate::lifecycle::{Run, Settlement, State, StateChange, Transition};
    use crate::task::{AbstainReason, Refusal, Task};

    fn run(id: &str, owner: &CallerKey, settlement: Option<Settlement>) -> Run {
        Run {
            id: RunId(id.into()),
            launch: LaunchId("launch-1".into()),
            owner: owner.clone(),
            owner_generation: 0,
            version: 3,
            state: State::Settled,
            prompt_certainty: None,
            child_name: "gov-deadbeef".into(),
            identity: None,
            operating_point: None,
            provider: None,
            tier_start: None,
            cwd: "/proj".into(),
            base_commit: None,
            work_generation: 0,
            evidence_generation: 0,
            child_status: None,
            idle_since: None,
            idle_deadline: None,
            repair_deadline: None,
            judgment_deadline: None,
            max_age_deadline: Timestamp(86_400_000),
            nudge_episode: 0,
            nudged_episode: None,
            settlement,
            settled_at: settlement.map(|_| Timestamp(1_000)),
        }
    }

    fn caller() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("kind-a".into()),
            native_session: NativeSession("sess-a".into()),
        }
    }

    fn policy() -> Policy {
        Policy {
            tiers: Vec::from([Tier("t0".into()), Tier("t1".into()), Tier("t2".into())]),
            no_change_cap: None,
            security_floor: None,
            broad_change_floor: None,
            provider_limit_threshold: 0.7,
            exploration_rate: 0.05,
            recovery_expiry: Duration::from_hours(24),
            cooldown: Duration::from_hours(1),
            max_age: Duration::from_hours(24),
            repair_window: Duration::from_mins(15),
            judgment_window: Duration::from_mins(30),
            idle_window: Duration::from_mins(15),
        }
    }

    fn task() -> Task {
        Task {
            objective: "do the thing".into(),
            scope: "the repo".into(),
            done_when: Vec::from(["it works".into()]),
            constraints: Vec::from(["stay quiet".into()]),
            tier: None,
            recovery_of: None,
            label: Some("lbl".into()),
            cwd: Some("/proj".into()),
        }
    }

    fn obligation(status: RecoveryStatus) -> RecoveryObligation {
        RecoveryObligation {
            predecessor: RunId("run-1".into()),
            origin: RecoveryOrigin::ProviderLimit,
            status,
            reason: None,
            successor_launch: match status {
                RecoveryStatus::Dispatched => Some(LaunchId("launch-9".into())),
                RecoveryStatus::Pending | RecoveryStatus::Blocked | RecoveryStatus::Failed => None,
            },
            expires_at: Timestamp(86_400_000),
        }
    }

    fn unique(status: Option<ChildStatus>) -> Observation {
        Observation::Unique {
            status,
            pane: PaneId("w6:p1".into()),
            native_session: Some(NativeSession("sess-1".into())),
        }
    }

    /// `caller_admission`'s `Ok(Some(_))` case, asserted into the obligation —
    /// a `None`/`Err` means the gate that should have admitted did not.
    fn admitted(result: Result<Option<RecoveryObligation>, Refusal>) -> RecoveryObligation {
        match result {
            Ok(Some(obligation)) => obligation,
            Ok(None) => panic!("expected the recovery to be admitted"),
            Err(refusal) => panic!("expected the recovery to be admitted, not {refusal:?}"),
        }
    }

    #[test]
    fn f21_recovery_origin_spellings() {
        let cases = [
            (RecoveryOrigin::ProviderLimit, "provider_limit"),
            (RecoveryOrigin::Caller, "caller"),
        ];
        for (origin, name) in cases {
            assert_eq!(
                origin.as_str(),
                name,
                "recovery origin spelling must match the DDL"
            );
        }
    }

    #[test]
    fn f21_recovery_status_spellings() {
        let cases = [
            (RecoveryStatus::Pending, "pending"),
            (RecoveryStatus::Blocked, "blocked"),
            (RecoveryStatus::Dispatched, "dispatched"),
            (RecoveryStatus::Failed, "failed"),
        ];
        for (status, name) in cases {
            assert_eq!(
                status.as_str(),
                name,
                "recovery status spelling must match the DDL"
            );
        }
    }

    #[test]
    fn f21_successor_key_is_recovery_prefixed() {
        let key = successor_key(&RunId("run-123".into()));
        assert_eq!(
            key.0, "recovery:run-123",
            "successor key is recovery:<predecessorRunId>"
        );
        assert!(
            key.0.starts_with(RECOVERY_KEY_PREFIX),
            "recovery key never collides with a caller key"
        );
    }

    #[test]
    fn f21_successor_task_carries_task_plus_preamble() {
        let predecessor = RunId("run-1".into());
        let mut task = task();
        task.recovery_of = Some(RunId("grandparent".into()));
        let successor = successor_task(&predecessor, &task);
        assert!(
            successor
                .objective
                .contains("Continue from the observed git and transcript state"),
            "preamble continues from observed git/transcript state"
        );
        assert!(
            successor
                .objective
                .contains("do not repeat side effects that already happened"),
            "preamble forbids repeating side effects"
        );
        assert!(
            successor.objective.ends_with("do the thing"),
            "the predecessor's objective is carried verbatim"
        );
        assert_eq!(
            successor.recovery_of,
            Some(predecessor),
            "recovery_of re-keys to the immediate predecessor (F13 step 4)"
        );
        assert_eq!(successor.scope, "the repo", "scope preserved");
        assert_eq!(successor.done_when, ["it works"], "doneWhen preserved");
        assert_eq!(
            successor.constraints,
            ["stay quiet"],
            "constraints preserved"
        );
        assert_eq!(successor.cwd, Some("/proj".into()), "cwd preserved");
        assert_eq!(successor.label, Some("lbl".into()), "label preserved");
        assert_eq!(successor.tier, None, "tier preserved");
    }

    #[test]
    fn f21_pending_obligation_expires_at_policy_expiry() {
        let obligation = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::ProviderLimit,
            Timestamp(1_000),
            Duration::from_hours(24),
        );
        assert_eq!(obligation.status, RecoveryStatus::Pending, "starts pending");
        assert_eq!(obligation.origin, RecoveryOrigin::ProviderLimit, "origin");
        assert_eq!(obligation.reason, None, "no reason yet");
        assert_eq!(obligation.successor_launch, None, "unclaimed");
        assert_eq!(
            obligation.expires_at,
            Timestamp(1_000 + 86_400_000),
            "expires at now + the 24 h policy expiry"
        );
    }

    #[test]
    fn f21_pending_dispatches_blocks_and_fails() {
        let dispatched =
            obligation(RecoveryStatus::Pending).dispatched(LaunchId("launch-2".into()));
        let mut expected = obligation(RecoveryStatus::Dispatched);
        expected.successor_launch = Some(LaunchId("launch-2".into()));
        assert_eq!(
            dispatched,
            Some(expected),
            "pending -> dispatched binds the successor launch (Appendix B CHECK)"
        );

        let blocked = obligation(RecoveryStatus::Pending).blocked(AbstainReason::NoCandidates);
        let mut expected_blocked = obligation(RecoveryStatus::Blocked);
        expected_blocked.reason = Some("no_candidates".into());
        assert_eq!(
            blocked,
            Some(expected_blocked),
            "pending -> blocked records the abstain reason"
        );

        let failed = obligation(RecoveryStatus::Pending).failed("gone".into());
        let mut expected_failed = obligation(RecoveryStatus::Failed);
        expected_failed.reason = Some("gone".into());
        assert_eq!(
            failed,
            Some(expected_failed),
            "pending -> failed records the reason"
        );
    }

    #[test]
    fn f21_terminal_states_absorb_every_transition() {
        for status in [
            RecoveryStatus::Blocked,
            RecoveryStatus::Dispatched,
            RecoveryStatus::Failed,
        ] {
            let obligation = obligation(status);
            assert_eq!(
                obligation.dispatched(LaunchId("x".into())),
                None,
                "dispatched is pending-only"
            );
            assert_eq!(
                obligation.blocked(AbstainReason::NoCandidates),
                None,
                "blocked is pending-only"
            );
            assert_eq!(
                obligation.failed("x".into()),
                None,
                "failed is pending-only"
            );
            assert_eq!(
                obligation.expired(Timestamp(i64::MAX)),
                None,
                "expired never rewrites a terminal obligation"
            );
        }
    }

    #[test]
    fn f21_pending_expires_at_or_past_expires_at() {
        let pending = obligation(RecoveryStatus::Pending);
        assert_eq!(
            pending.expired(Timestamp(86_400_000 - 1)),
            None,
            "one millisecond early is not expired"
        );
        let mut expected = obligation(RecoveryStatus::Failed);
        expected.reason = Some("expired".into());
        assert_eq!(
            pending.expired(Timestamp(86_400_000)),
            Some(expected.clone()),
            "at expires_at the pending obligation fails expired"
        );
        assert_eq!(
            pending.expired(Timestamp(86_400_000 + 1)),
            Some(expected),
            "past expires_at is expired"
        );
    }

    #[test]
    fn f21_expiry_arithmetic_saturates() {
        let obligation = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::Caller,
            Timestamp(i64::MAX - 10),
            Duration::from_hours(24),
        );
        assert_eq!(
            obligation.expires_at,
            Timestamp(i64::MAX),
            "expiry saturates rather than wrapping"
        );
        let huge = RecoveryObligation::pending(
            RunId("run-1".into()),
            RecoveryOrigin::Caller,
            Timestamp(0),
            Duration::MAX,
        );
        assert_eq!(
            huge.expires_at,
            Timestamp(i64::MAX),
            "an unrepresentable duration pins to i64::MAX"
        );
    }

    #[test]
    fn f21_cooldown_limited_fields() {
        let cooldown = Cooldown::limited(
            Provider("prov-1".into()),
            RunId("run-1".into()),
            Timestamp(1_000),
            Duration::from_hours(1),
        );
        assert_eq!(cooldown.provider, Provider("prov-1".into()), "provider");
        assert_eq!(
            cooldown.until,
            Timestamp(1_000 + 3_600_000),
            "until is now + the policy window"
        );
        assert_eq!(
            cooldown.reason, "provider_limited",
            "reason records the limiting settlement"
        );
        assert_eq!(
            cooldown.source_run,
            Some(RunId("run-1".into())),
            "source run recorded"
        );
    }

    #[test]
    fn f21_cooldowns_only_lengthen() {
        let existing = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(5_000),
            reason: "first".into(),
            source_run: Some(RunId("run-1".into())),
        };
        let shorter = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(4_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_shorter = existing.merged(shorter);
        assert_eq!(
            merged_shorter.until,
            Timestamp(5_000),
            "a shorter limit never shortens"
        );
        assert_eq!(
            merged_shorter.reason, "first",
            "the surviving exclusion keeps its reason"
        );

        let equal = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(5_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_equal = existing.merged(equal);
        assert_eq!(merged_equal.until, Timestamp(5_000), "equal until stays");
        assert_eq!(
            merged_equal.reason, "first",
            "a tie keeps the existing record"
        );

        let longer = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(9_000),
            reason: "second".into(),
            source_run: Some(RunId("run-2".into())),
        };
        let merged_longer = existing.merged(longer);
        assert_eq!(
            merged_longer.until,
            Timestamp(9_000),
            "a later limit lengthens"
        );
        assert_eq!(
            merged_longer.reason, "second",
            "the extending exclusion's reason rides with it"
        );
        assert_eq!(
            merged_longer.source_run,
            Some(RunId("run-2".into())),
            "the extending exclusion's source rides with it"
        );
    }

    #[test]
    fn f21_dispatch_requires_absent() {
        assert!(
            dispatch_ready(&Observation::Absent),
            "only absent dispatches"
        );
        for status in [
            Some(ChildStatus::Working),
            Some(ChildStatus::Idle),
            Some(ChildStatus::Done),
            Some(ChildStatus::Blocked),
            None,
        ] {
            assert!(
                !dispatch_ready(&unique(status)),
                "a present predecessor never dispatches (ADR-0003)"
            );
        }
        assert!(
            !dispatch_ready(&Observation::Invalid),
            "invalid never counts as absence (F3)"
        );
    }

    #[test]
    fn f21_provider_limited_writes() {
        let predecessor = RunId("run-1".into());
        let transition = provider_limited(
            &predecessor,
            Provider("prov-1".into()),
            None,
            Timestamp(1_000),
            &policy(),
            EventId("event-1".into()),
            EventId("event-2".into()),
        );
        let mut expected_obligation = obligation(RecoveryStatus::Pending);
        expected_obligation.expires_at = Timestamp(1_000 + 86_400_000);
        let expected = Transition {
            state_changes: Vec::from([
                StateChange::RecordRecovery(expected_obligation),
                StateChange::SetCooldown(Cooldown {
                    provider: Provider("prov-1".into()),
                    until: Timestamp(1_000 + 3_600_000),
                    reason: "provider_limited".into(),
                    source_run: Some(predecessor.clone()),
                }),
            ]),
            events: Vec::from([
                MailboxEvent {
                    id: EventId("event-1".into()),
                    dedup_key: DedupKey("run:run-1:cooldown_hit".into()),
                    subject: MailboxSubject::Run(predecessor.clone()),
                    kind: MailboxEventKind::CooldownHit,
                    body: "{\"provider\":\"prov-1\",\"until\":3601000}".into(),
                },
                MailboxEvent {
                    id: EventId("event-2".into()),
                    dedup_key: DedupKey("run:run-1:recovery_pending".into()),
                    subject: MailboxSubject::Run(predecessor.clone()),
                    kind: MailboxEventKind::RecoveryPending,
                    body: "{\"predecessor\":\"run-1\",\"expires_at\":86401000,\"message\":\"close the predecessor's pane (cancel with closePane) to dispatch the recovery\"}"
                        .into(),
                },
            ]),
            effects: Vec::new(),
        };
        assert_eq!(
            transition, expected,
            "settle records the pending obligation, the provider cooldown and both events in one transaction"
        );
    }

    #[test]
    fn f21_provider_limited_merges_existing_cooldown() {
        let existing = Cooldown {
            provider: Provider("prov-1".into()),
            until: Timestamp(9_999_999),
            reason: "earlier".into(),
            source_run: Some(RunId("run-0".into())),
        };
        let transition = provider_limited(
            &RunId("run-1".into()),
            Provider("prov-1".into()),
            Some(&existing),
            Timestamp(1_000),
            &policy(),
            EventId("event-1".into()),
            EventId("event-2".into()),
        );
        assert_eq!(
            transition.state_changes[1],
            StateChange::SetCooldown(existing),
            "the longer existing cooldown is never shortened"
        );
        assert_eq!(
            transition.events[0].body, "{\"provider\":\"prov-1\",\"until\":9999999}",
            "the event reports the effective cooldown"
        );
    }

    #[test]
    fn f21_recoveryof_requires_owner() {
        let predecessor = run("run-1", &caller(), Some(Settlement::Accepted));
        let foreign = CallerKey {
            agent_kind: AgentKind("kind-b".into()),
            native_session: NativeSession("sess-b".into()),
        };
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &foreign,
            Timestamp(0),
            &policy(),
        );
        assert_eq!(
            result,
            Err(Refusal::NotOwner),
            "a foreign caller cannot claim a run's recovery (F4)"
        );
    }

    #[test]
    fn f21_recoveryof_second_recovery_refused() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::ProviderLimited));
        for origin in [RecoveryOrigin::Caller, RecoveryOrigin::ProviderLimit] {
            for status in [
                RecoveryStatus::Pending,
                RecoveryStatus::Blocked,
                RecoveryStatus::Dispatched,
                RecoveryStatus::Failed,
            ] {
                let mut existing = obligation(status);
                existing.origin = origin;
                let claimable =
                    origin == RecoveryOrigin::ProviderLimit && status == RecoveryStatus::Pending;
                let result = caller_admission(
                    &predecessor,
                    Some(&existing),
                    &Observation::Absent,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                if claimable {
                    let claimed = admitted(result);
                    assert_eq!(
                        claimed.status,
                        RecoveryStatus::Dispatched,
                        "the unclaimed provider_limit obligation is claimable"
                    );
                } else {
                    assert_eq!(
                        result,
                        Err(Refusal::RecoveryExists),
                        "a second recovery of the same predecessor is refused ({origin:?}/{status:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn f21_recoveryof_requires_settled_predecessor() {
        let predecessor = run("run-1", &caller(), None);
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller(),
            Timestamp(0),
            &policy(),
        );
        assert_eq!(
            result,
            Ok(None),
            "an unsettled predecessor cannot be recovered"
        );
    }

    #[test]
    fn f21_recoveryof_provider_limited_needs_absent() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::ProviderLimited));
        for observation in [
            Observation::Invalid,
            unique(Some(ChildStatus::Working)),
            unique(Some(ChildStatus::Idle)),
            unique(Some(ChildStatus::Done)),
            unique(Some(ChildStatus::Blocked)),
            unique(None),
        ] {
            let result = caller_admission(
                &predecessor,
                None,
                &observation,
                &LaunchId("launch-2".into()),
                &caller,
                Timestamp(0),
                &policy(),
            );
            assert_eq!(
                result,
                Ok(None),
                "a provider_limited predecessor must be observed absent first"
            );
        }
        let claimed = caller_admission(
            &predecessor,
            Some(&obligation(RecoveryStatus::Pending)),
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(0),
            &policy(),
        );
        let mut expected = obligation(RecoveryStatus::Dispatched);
        expected.successor_launch = Some(LaunchId("launch-2".into()));
        assert_eq!(
            claimed,
            Ok(Some(expected)),
            "the claim dispatches the obligation and binds the caller's launch, preserving origin"
        );
    }

    #[test]
    fn f21_recoveryof_other_settlements_idle_done_or_absent() {
        let caller = caller();
        for settlement in [
            Settlement::Accepted,
            Settlement::Rejected,
            Settlement::NoHandoff,
            Settlement::PaneLost,
            Settlement::Cancelled,
        ] {
            let predecessor = run("run-1", &caller, Some(settlement));
            for observation in [
                unique(Some(ChildStatus::Idle)),
                unique(Some(ChildStatus::Done)),
                Observation::Absent,
            ] {
                let result = caller_admission(
                    &predecessor,
                    None,
                    &observation,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                let obligation = admitted(result);
                assert_eq!(
                    obligation.status,
                    RecoveryStatus::Dispatched,
                    "settled predecessor observed idle/done/absent admits ({settlement:?})"
                );
            }
            for observation in [
                unique(Some(ChildStatus::Working)),
                unique(Some(ChildStatus::Blocked)),
                unique(None),
                Observation::Invalid,
            ] {
                let result = caller_admission(
                    &predecessor,
                    None,
                    &observation,
                    &LaunchId("launch-2".into()),
                    &caller,
                    Timestamp(0),
                    &policy(),
                );
                assert_eq!(
                    result,
                    Ok(None),
                    "a working/blocked/unknown/invalid predecessor admits nothing ({settlement:?})"
                );
            }
        }
    }

    #[test]
    fn f21_recoveryof_creates_caller_obligation() {
        let caller = caller();
        let predecessor = run("run-1", &caller, Some(Settlement::Rejected));
        let result = caller_admission(
            &predecessor,
            None,
            &Observation::Absent,
            &LaunchId("launch-2".into()),
            &caller,
            Timestamp(1_000),
            &policy(),
        );
        let obligation = admitted(result);
        assert_eq!(
            obligation.origin,
            RecoveryOrigin::Caller,
            "a caller-requested recovery records origin caller"
        );
        assert_eq!(
            obligation.status,
            RecoveryStatus::Dispatched,
            "the admitted successor dispatch is recorded"
        );
        assert_eq!(
            obligation.successor_launch,
            Some(LaunchId("launch-2".into())),
            "successor bound (Appendix B CHECK)"
        );
        assert_eq!(
            obligation.predecessor,
            RunId("run-1".into()),
            "predecessor key"
        );
        assert_eq!(
            obligation.expires_at,
            Timestamp(1_000 + 86_400_000),
            "the policy expiry still stamps"
        );
    }

    #[test]
    fn f21_event_bodies_escape_json() {
        let transition = provider_limited(
            &RunId("run-\"1".into()),
            Provider("prov\\\"1".into()),
            None,
            Timestamp(0),
            &policy(),
            EventId("event-1".into()),
            EventId("event-2".into()),
        );
        assert_eq!(
            transition.events[0].body, "{\"provider\":\"prov\\\\\\\"1\",\"until\":3600000}",
            "free-form provider values cannot break body_json"
        );
        assert_eq!(
            transition.events[1].body,
            "{\"predecessor\":\"run-\\\"1\",\"expires_at\":86400000,\"message\":\"close the predecessor's pane (cancel with closePane) to dispatch the recovery\"}",
            "free-form run ids cannot break body_json"
        );

        let control = provider_limited(
            &RunId("run-2".into()),
            Provider("p\u{1f}".into()),
            None,
            Timestamp(0),
            &policy(),
            EventId("event-1".into()),
            EventId("event-2".into()),
        );
        assert_eq!(
            control.events[0].body, "{\"provider\":\"p\\u001f\",\"until\":3600000}",
            "control characters escape as \\u00XX"
        );
    }
}
