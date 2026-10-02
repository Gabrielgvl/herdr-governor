//! `run` — the `runs` row ↔ `Run`. `owner_caller_id` is the surrogate the
//! `callers` join resolves; `to_core` takes the `CallerKey` back. The six
//! identity columns are one group: all of `herdr_incarnation`/`terminal_id`/
//! `agent_kind`/`agent_name`/`pane_id` present (with `native_session`
//! optional) decodes a `ChildIdentity`; all absent decodes `None`; anything
//! in between is corrupt. The `settlement`/`settlement_reason`/`settled_at`
//! trio mirrors the Appendix B CHECK constraints strictly.

use rusqlite::Row;

use governor_core::config::{OperatingPointId, Provider, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, HerdrIncarnation, LaunchId,
    NativeSession, PaneId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{PromptCertainty, Run, Settlement, State, UnresolvedReason};

use crate::store::error::StoreError;
use crate::store::rows::{
    Params, corrupt, enum_decode, enum_opt_decode, hex_encode, hex_opt_decode, i64_to_u64,
    read_col, ts_decode, ts_encode, ts_opt_decode, ts_opt_encode, u64_to_col,
};

const TABLE: &str = "runs";

const STATES: &[State] = &[
    State::Reserved,
    State::Starting,
    State::Prompting,
    State::Active,
    State::Judging,
    State::Repair,
    State::Settled,
];

const PROMPT_CERTAINTIES: &[PromptCertainty] =
    &[PromptCertainty::Acknowledged, PromptCertainty::Unconfirmed];

const SETTLEMENTS: &[Settlement] = &[
    Settlement::Accepted,
    Settlement::Rejected,
    Settlement::NoHandoff,
    Settlement::PaneLost,
    Settlement::Cancelled,
    Settlement::ProviderLimited,
];

const UNRESOLVED_REASONS: &[UnresolvedReason] = &[
    UnresolvedReason::LaunchNotStarted,
    UnresolvedReason::LaunchFailed,
    UnresolvedReason::JudgmentUnavailable,
    UnresolvedReason::IdentityUnprovable,
    UnresolvedReason::MaxAge,
];

const CHILD_STATUSES: &[ChildStatus] = &[
    ChildStatus::Working,
    ChildStatus::Idle,
    ChildStatus::Done,
    ChildStatus::Blocked,
];

/// The `runs` row. `created_at`/`updated_at` are store-stamped — `Run`
/// carries neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct RunRow {
    run_id: String,
    launch_id: String,
    owner_caller_id: i64,
    owner_generation: i64,
    version: i64,
    state: String,
    prompt_certainty: Option<String>,
    child_name: String,
    herdr_incarnation: Option<String>,
    terminal_id: Option<String>,
    agent_kind: Option<String>,
    agent_name: Option<String>,
    native_session: Option<String>,
    pane_id: Option<String>,
    operating_point_id: Option<String>,
    provider: Option<String>,
    tier_start: Option<String>,
    cwd: String,
    base_commit: Option<String>,
    work_generation: i64,
    evidence_generation: i64,
    evidence_digest: Option<String>,
    child_status: Option<String>,
    idle_since: Option<String>,
    idle_deadline: Option<String>,
    repair_deadline: Option<String>,
    rejected_at: Option<String>,
    judgment_deadline: Option<String>,
    judging_digest: Option<String>,
    max_age_deadline: String,
    nudge_episode: i64,
    nudged_episode: Option<i64>,
    blocked_episode: i64,
    settlement: Option<String>,
    settlement_reason: Option<String>,
    settled_at: Option<String>,
    created_at: String,
    updated_at: String,
}

fn settlement_parts(settlement: Option<&Settlement>) -> (Option<String>, Option<String>) {
    let reason = match settlement {
        Some(Settlement::Unresolved { reason }) => Some(reason.as_str().into()),
        Some(
            Settlement::Accepted
            | Settlement::Rejected
            | Settlement::NoHandoff
            | Settlement::PaneLost
            | Settlement::Cancelled
            | Settlement::ProviderLimited,
        )
        | None => None,
    };
    (settlement.map(|s| s.as_str().into()), reason)
}

fn identity_fields(identity: &ChildIdentity) -> [Option<String>; 6] {
    [
        Some(identity.herdr_incarnation.0.clone()),
        Some(identity.terminal_id.0.clone()),
        Some(identity.agent_kind.0.clone()),
        Some(identity.agent_name.0.clone()),
        identity.native_session.as_ref().map(|s| s.0.clone()),
        Some(identity.pane_id.0.clone()),
    ]
}

impl RunRow {
    /// Encode `run`; `owner` is the surrogate id the writer resolved, `now`
    /// stamps `created_at`/`updated_at`.
    ///
    /// # Errors
    /// Propagates time/hex encode failures and `u64` fields that do not fit
    /// `INTEGER` as [`StoreError::CorruptRow`].
    pub(in crate::store) fn from_core(
        run: &Run,
        owner: i64,
        now: Timestamp,
    ) -> Result<Self, StoreError> {
        let [incarnation, terminal, kind, name, session, pane] = run
            .identity
            .as_ref()
            .map_or([const { None }; 6], identity_fields);
        let (settlement, settlement_reason) = settlement_parts(run.settlement.as_ref());
        Ok(Self {
            run_id: run.id.0.clone(),
            launch_id: run.launch.0.clone(),
            owner_caller_id: owner,
            owner_generation: u64_to_col(run.owner_generation, TABLE, "owner_generation")?,
            version: u64_to_col(run.version, TABLE, "version")?,
            state: run.state.as_str().into(),
            prompt_certainty: run.prompt_certainty.map(|c| c.as_str().into()),
            child_name: run.child_name.clone(),
            herdr_incarnation: incarnation,
            terminal_id: terminal,
            agent_kind: kind,
            agent_name: name,
            native_session: session,
            pane_id: pane,
            operating_point_id: run.operating_point.as_ref().map(|o| o.0.clone()),
            provider: run.provider.as_ref().map(|p| p.0.clone()),
            tier_start: run.tier_start.as_ref().map(|t| t.0.clone()),
            cwd: run.cwd.clone(),
            base_commit: run.base_commit.clone(),
            work_generation: u64_to_col(run.work_generation, TABLE, "work_generation")?,
            evidence_generation: u64_to_col(run.evidence_generation, TABLE, "evidence_generation")?,
            evidence_digest: run.evidence_digest.map(hex_encode),
            child_status: run.child_status.map(|s| s.as_str().into()),
            idle_since: ts_opt_encode(run.idle_since, TABLE, "idle_since")?,
            idle_deadline: ts_opt_encode(run.idle_deadline, TABLE, "idle_deadline")?,
            repair_deadline: ts_opt_encode(run.repair_deadline, TABLE, "repair_deadline")?,
            rejected_at: ts_opt_encode(run.rejected_at, TABLE, "rejected_at")?,
            judgment_deadline: ts_opt_encode(run.judgment_deadline, TABLE, "judgment_deadline")?,
            judging_digest: run.judging_digest.map(hex_encode),
            max_age_deadline: ts_encode(run.max_age_deadline, TABLE, "max_age_deadline")?,
            nudge_episode: u64_to_col(run.nudge_episode, TABLE, "nudge_episode")?,
            nudged_episode: run
                .nudged_episode
                .map(|e| u64_to_col(e, TABLE, "nudged_episode"))
                .transpose()?,
            blocked_episode: u64_to_col(run.blocked_episode, TABLE, "blocked_episode")?,
            settlement,
            settlement_reason,
            settled_at: ts_opt_encode(run.settled_at, TABLE, "settled_at")?,
            created_at: ts_encode(now, TABLE, "created_at")?,
            updated_at: ts_encode(now, TABLE, "updated_at")?,
        })
    }

    /// The checked decode back to `Run`; `owner` is the key the read's
    /// `callers` join selected.
    ///
    /// # Errors
    /// [`StoreError::CorruptRow`] on unknown enum/digest text, a partial
    /// identity group, or a violation of the Appendix B CHECK constraints
    /// (`settled` ⇔ `settlement`, `settlement` ⇔ `settled_at`, `unresolved`
    /// ⇒ reason — and a reason on any other settlement is likewise foreign
    /// data the codec never produced).
    pub(in crate::store) fn to_core(&self, owner: CallerKey) -> Result<Run, StoreError> {
        let state = enum_decode(&self.state, TABLE, "state", STATES, State::as_str)?;
        let settlement = self.decode_settlement()?;
        if (state == State::Settled) != settlement.is_some() {
            return Err(corrupt(
                TABLE,
                "settlement",
                "state is 'settled' exactly when settlement is set",
            ));
        }
        Ok(Run {
            id: RunId(self.run_id.clone()),
            launch: LaunchId(self.launch_id.clone()),
            owner,
            owner_generation: i64_to_u64(self.owner_generation, TABLE, "owner_generation")?,
            version: i64_to_u64(self.version, TABLE, "version")?,
            state,
            prompt_certainty: enum_opt_decode(
                self.prompt_certainty.as_deref(),
                TABLE,
                "prompt_certainty",
                PROMPT_CERTAINTIES,
                PromptCertainty::as_str,
            )?,
            child_name: self.child_name.clone(),
            identity: self.decode_identity()?,
            operating_point: self.operating_point_id.clone().map(OperatingPointId),
            provider: self.provider.clone().map(Provider),
            tier_start: self.tier_start.clone().map(Tier),
            cwd: self.cwd.clone(),
            base_commit: self.base_commit.clone(),
            work_generation: i64_to_u64(self.work_generation, TABLE, "work_generation")?,
            evidence_generation: i64_to_u64(
                self.evidence_generation,
                TABLE,
                "evidence_generation",
            )?,
            evidence_digest: hex_opt_decode(
                self.evidence_digest.as_deref(),
                TABLE,
                "evidence_digest",
            )?,
            child_status: enum_opt_decode(
                self.child_status.as_deref(),
                TABLE,
                "child_status",
                CHILD_STATUSES,
                ChildStatus::as_str,
            )?,
            idle_since: ts_opt_decode(self.idle_since.as_deref(), TABLE, "idle_since")?,
            idle_deadline: ts_opt_decode(self.idle_deadline.as_deref(), TABLE, "idle_deadline")?,
            repair_deadline: ts_opt_decode(
                self.repair_deadline.as_deref(),
                TABLE,
                "repair_deadline",
            )?,
            rejected_at: ts_opt_decode(self.rejected_at.as_deref(), TABLE, "rejected_at")?,
            judgment_deadline: ts_opt_decode(
                self.judgment_deadline.as_deref(),
                TABLE,
                "judgment_deadline",
            )?,
            judging_digest: hex_opt_decode(
                self.judging_digest.as_deref(),
                TABLE,
                "judging_digest",
            )?,
            max_age_deadline: ts_decode(&self.max_age_deadline, TABLE, "max_age_deadline")?,
            nudge_episode: i64_to_u64(self.nudge_episode, TABLE, "nudge_episode")?,
            nudged_episode: self
                .nudged_episode
                .map(|e| i64_to_u64(e, TABLE, "nudged_episode"))
                .transpose()?,
            blocked_episode: i64_to_u64(self.blocked_episode, TABLE, "blocked_episode")?,
            settlement,
            settled_at: ts_opt_decode(self.settled_at.as_deref(), TABLE, "settled_at")?,
        })
    }

    /// The identity group: all five required columns or none; a partial
    /// group — or a `native_session` without the rest — is foreign data.
    fn decode_identity(&self) -> Result<Option<ChildIdentity>, StoreError> {
        let fields = (
            &self.herdr_incarnation,
            &self.terminal_id,
            &self.agent_kind,
            &self.agent_name,
            &self.pane_id,
        );
        match fields {
            (Some(incarnation), Some(terminal), Some(kind), Some(name), Some(pane)) => {
                Ok(Some(ChildIdentity {
                    herdr_incarnation: HerdrIncarnation(incarnation.clone()),
                    terminal_id: TerminalId(terminal.clone()),
                    agent_kind: AgentKind(kind.clone()),
                    agent_name: AgentName(name.clone()),
                    native_session: self.native_session.clone().map(NativeSession),
                    pane_id: PaneId(pane.clone()),
                }))
            }
            (None, None, None, None, None) if self.native_session.is_none() => Ok(None),
            _ => Err(corrupt(
                TABLE,
                "identity",
                "identity columns are a group — all required parts or none",
            )),
        }
    }

    /// The settlement triple with the Appendix B CHECK constraints restated:
    /// `settlement`/`settled_at` are a pair, `unresolved` requires the
    /// reason, and a reason on any other settlement is foreign data.
    fn decode_settlement(&self) -> Result<Option<Settlement>, StoreError> {
        match (
            self.settlement.as_deref(),
            &self.settled_at,
            &self.settlement_reason,
        ) {
            (None, None, None) => Ok(None),
            (Some("unresolved"), Some(_), Some(reason)) => Ok(Some(Settlement::Unresolved {
                reason: enum_decode(
                    reason,
                    TABLE,
                    "settlement_reason",
                    UNRESOLVED_REASONS,
                    UnresolvedReason::as_str,
                )?,
            })),
            (Some("unresolved"), Some(_), None) => Err(corrupt(
                TABLE,
                "settlement_reason",
                "'unresolved' requires a reason",
            )),
            (Some(kind), Some(_), None) => {
                enum_decode(kind, TABLE, "settlement", SETTLEMENTS, Settlement::as_str).map(Some)
            }
            (Some(_), Some(_), Some(_)) => Err(corrupt(
                TABLE,
                "settlement_reason",
                "a reason is only meaningful on 'unresolved'",
            )),
            _ => Err(corrupt(
                TABLE,
                "settlement",
                "settlement and settled_at come as a pair",
            )),
        }
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("run_id", self.run_id.clone().into()),
            ("launch_id", self.launch_id.clone().into()),
            ("owner_caller_id", self.owner_caller_id.into()),
            ("owner_generation", self.owner_generation.into()),
            ("version", self.version.into()),
            ("state", self.state.clone().into()),
            ("prompt_certainty", self.prompt_certainty.clone().into()),
            ("child_name", self.child_name.clone().into()),
            ("herdr_incarnation", self.herdr_incarnation.clone().into()),
            ("terminal_id", self.terminal_id.clone().into()),
            ("agent_kind", self.agent_kind.clone().into()),
            ("agent_name", self.agent_name.clone().into()),
            ("native_session", self.native_session.clone().into()),
            ("pane_id", self.pane_id.clone().into()),
            ("operating_point_id", self.operating_point_id.clone().into()),
            ("provider", self.provider.clone().into()),
            ("tier_start", self.tier_start.clone().into()),
            ("cwd", self.cwd.clone().into()),
            ("base_commit", self.base_commit.clone().into()),
            ("work_generation", self.work_generation.into()),
            ("evidence_generation", self.evidence_generation.into()),
            ("evidence_digest", self.evidence_digest.clone().into()),
            ("child_status", self.child_status.clone().into()),
            ("idle_since", self.idle_since.clone().into()),
            ("idle_deadline", self.idle_deadline.clone().into()),
            ("repair_deadline", self.repair_deadline.clone().into()),
            ("rejected_at", self.rejected_at.clone().into()),
            ("judgment_deadline", self.judgment_deadline.clone().into()),
            ("judging_digest", self.judging_digest.clone().into()),
            ("max_age_deadline", self.max_age_deadline.clone().into()),
            ("nudge_episode", self.nudge_episode.into()),
            ("nudged_episode", self.nudged_episode.into()),
            ("blocked_episode", self.blocked_episode.into()),
            ("settlement", self.settlement.clone().into()),
            ("settlement_reason", self.settlement_reason.clone().into()),
            ("settled_at", self.settled_at.clone().into()),
            ("created_at", self.created_at.clone().into()),
            ("updated_at", self.updated_at.clone().into()),
        ]
    }

    /// Pull the row's own columns out of a query row (the joined owner
    /// columns go through `rows::caller::key_from_row`).
    ///
    /// # Errors
    /// Typed column mismatches surface as [`StoreError::CorruptRow`].
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            run_id: read_col(row, TABLE, "run_id")?,
            launch_id: read_col(row, TABLE, "launch_id")?,
            owner_caller_id: read_col(row, TABLE, "owner_caller_id")?,
            owner_generation: read_col(row, TABLE, "owner_generation")?,
            version: read_col(row, TABLE, "version")?,
            state: read_col(row, TABLE, "state")?,
            prompt_certainty: read_col(row, TABLE, "prompt_certainty")?,
            child_name: read_col(row, TABLE, "child_name")?,
            herdr_incarnation: read_col(row, TABLE, "herdr_incarnation")?,
            terminal_id: read_col(row, TABLE, "terminal_id")?,
            agent_kind: read_col(row, TABLE, "agent_kind")?,
            agent_name: read_col(row, TABLE, "agent_name")?,
            native_session: read_col(row, TABLE, "native_session")?,
            pane_id: read_col(row, TABLE, "pane_id")?,
            operating_point_id: read_col(row, TABLE, "operating_point_id")?,
            provider: read_col(row, TABLE, "provider")?,
            tier_start: read_col(row, TABLE, "tier_start")?,
            cwd: read_col(row, TABLE, "cwd")?,
            base_commit: read_col(row, TABLE, "base_commit")?,
            work_generation: read_col(row, TABLE, "work_generation")?,
            evidence_generation: read_col(row, TABLE, "evidence_generation")?,
            evidence_digest: read_col(row, TABLE, "evidence_digest")?,
            child_status: read_col(row, TABLE, "child_status")?,
            idle_since: read_col(row, TABLE, "idle_since")?,
            idle_deadline: read_col(row, TABLE, "idle_deadline")?,
            repair_deadline: read_col(row, TABLE, "repair_deadline")?,
            rejected_at: read_col(row, TABLE, "rejected_at")?,
            judgment_deadline: read_col(row, TABLE, "judgment_deadline")?,
            judging_digest: read_col(row, TABLE, "judging_digest")?,
            max_age_deadline: read_col(row, TABLE, "max_age_deadline")?,
            nudge_episode: read_col(row, TABLE, "nudge_episode")?,
            nudged_episode: read_col(row, TABLE, "nudged_episode")?,
            blocked_episode: read_col(row, TABLE, "blocked_episode")?,
            settlement: read_col(row, TABLE, "settlement")?,
            settlement_reason: read_col(row, TABLE, "settlement_reason")?,
            settled_at: read_col(row, TABLE, "settled_at")?,
            created_at: read_col(row, TABLE, "created_at")?,
            updated_at: read_col(row, TABLE, "updated_at")?,
        })
    }
}
