//! Pure codec tests for `rows` (P4.S2): the shared `SELECT`-bound row
//! helper, the fixtures, and the time/hex helper tests; `codecs` holds the
//! launch/run/judgment/effect round-trips. No table is touched — a `SELECT`
//! with bound parameters is the only SQL, so each round-trip is
//! `from_core → params → SELECT → read → to_core` through SQLite's own
//! typing, and the `julianday`/`json_extract` proofs run against the codec's
//! own output.

mod codecs;

use std::collections::BTreeMap;

use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, Digest, EffectId, EffectKey,
    HerdrIncarnation, IdempotencyKey, JudgmentSetId, LaunchId, NativeSession, PaneId, ProjectRoot,
    RunId, TabId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{
    CreatedTopology, Effect, EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectTarget,
    PromptCertainty, Run, Settlement, State, VersionTriple,
};
use governor_core::routing::{
    Candidate, Decision, Exploration, Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord,
    JudgmentSet, Probability, Question, QuestionVersion,
};
use governor_core::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase, Task};
use rusqlite::{Connection, Row, ToSql};

use super::{Params, hex_decode, hex_encode, ts_decode, ts_encode};
use crate::store::StoreError;

const T: &str = "t";
const C: &str = "c";
pub(super) const NOW: Timestamp = Timestamp(1_790_812_800_000);

/// Bind `params` as `SELECT ?1 AS col1, ?2 AS col2, …` on an in-memory
/// connection and hand the resulting row to `read` — the row exactly as a
/// table read would present it, without any table.
pub(super) fn via_sqlite<T>(
    params: &Params,
    read: impl FnOnce(&Row<'_>) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let conn = Connection::open_in_memory()?;
    let columns = (1..)
        .zip(params)
        .map(|(i, (name, _))| format!("?{i} AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!("SELECT {columns}"))?;
    let values: Vec<&dyn ToSql> = params.iter().map(|(_, v)| -> &dyn ToSql { v }).collect();
    stmt.query_row(values.as_slice(), |row| Ok(read(row)))?
}

/// Overwrite one bound column — the fixture for a foreign persisted value.
pub(super) fn poison(params: &mut Params, column: &str, value: &str) {
    let slot = params.iter_mut().find(|(name, _)| *name == column).unwrap();
    slot.1 = value.to_owned().into();
}

pub(super) fn digest(fill: u8) -> Digest {
    Digest([fill; 32])
}

pub(super) fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-1".into()),
    }
}

pub(super) fn identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName("gov-abcdef12".into()),
        native_session: Some(NativeSession("child-sess".into())),
        pane_id: PaneId("pane-9".into()),
    }
}

pub(super) fn task() -> Task {
    Task {
        objective: "ship it".into(),
        scope: "src/".into(),
        done_when: vec!["tests pass".into(), "lint clean".into()],
        constraints: vec![],
        tier: Some(Tier("high".into())),
        recovery_of: None,
        label: Some("label".into()),
        cwd: None,
    }
}

pub(super) fn decision() -> Decision {
    Decision {
        judged_tier: Tier("mid".into()),
        requested_tier: Some(Tier("high".into())),
        policy_cap: None,
        policy_floor: Some(Tier("mid".into())),
        caller_uplift: Some(Tier("high".into())),
        recovery_minimum: None,
        exploration: Exploration {
            assigned: true,
            executed: false,
        },
        start_tier: Tier("high".into()),
        candidates: vec![Candidate {
            operating_point: OperatingPointId("op-1".into()),
            provider: Provider("prov".into()),
            tier: Tier("high".into()),
            harness: AgentKind("kind-a".into()),
            args: vec!["--flag".into(), "x".into()],
        }],
        config_version: ConfigVersion("cfg-1".into()),
    }
}

pub(super) fn launch(phase: LaunchPhase, outcome: Option<LaunchOutcome>) -> Launch {
    Launch {
        id: LaunchId("l-1".into()),
        caller: caller(),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey("k-1".into()),
        digest_version: 1,
        task_digest: digest(0xab),
        task: task(),
        phase,
        decision: Some(decision()),
        config_version: Some(ConfigVersion("cfg-1".into())),
        outcome,
    }
}

pub(super) fn launch_outcomes() -> Vec<LaunchOutcome> {
    vec![
        LaunchOutcome::Launched {
            run: RunId("r-1".into()),
            operating_point: OperatingPointId("op-1".into()),
            requested_operating_point: None,
            tier_evidence: decision(),
        },
        LaunchOutcome::Abstained {
            reason: AbstainReason::NoCandidates,
        },
        LaunchOutcome::Rejected,
        LaunchOutcome::Failed {
            certainty: EffectCertainty::Unknown,
            run: Some(RunId("r-1".into())),
            created_topology: CreatedTopology {
                tab: Some(TabId("tab-1".into())),
                panes: vec![PaneId("p-1".into()), PaneId("p-2".into())],
            },
        },
    ]
}

pub(super) fn run_bare() -> Run {
    Run {
        id: RunId("r-1".into()),
        launch: LaunchId("l-1".into()),
        owner: caller(),
        owner_generation: 0,
        version: 1,
        state: State::Reserved,
        prompt_certainty: None,
        child_name: "w1:r-1".into(),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/p".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(NOW.0 + 86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

pub(super) fn run_full(settlement: Settlement) -> Run {
    Run {
        owner_generation: 2,
        version: 9,
        state: State::Settled,
        prompt_certainty: Some(PromptCertainty::Acknowledged),
        identity: Some(identity()),
        operating_point: Some(OperatingPointId("op-1".into())),
        provider: Some(Provider("prov".into())),
        tier_start: Some(Tier("high".into())),
        base_commit: Some("deadbeef".into()),
        work_generation: 3,
        evidence_generation: 4,
        evidence_digest: Some(digest(0x01)),
        child_status: Some(ChildStatus::Done),
        idle_since: Some(Timestamp(NOW.0 + 1)),
        idle_deadline: Some(Timestamp(NOW.0 + 2)),
        repair_deadline: Some(Timestamp(NOW.0 + 3)),
        rejected_at: Some(Timestamp(NOW.0 + 4)),
        judgment_deadline: Some(Timestamp(NOW.0 + 5)),
        judging_digest: Some(digest(0x02)),
        nudge_episode: 2,
        nudged_episode: Some(1),
        blocked_episode: 1,
        settlement: Some(settlement),
        settled_at: Some(Timestamp(NOW.0 + 6)),
        ..run_bare()
    }
}

pub(super) fn judgment_set(versions: Option<VersionTriple>) -> JudgmentSet {
    JudgmentSet {
        id: JudgmentSetId("js-1".into()),
        purpose: JudgmentPurpose::Acceptance,
        launch: None,
        run: Some(RunId("r-1".into())),
        versions,
        task_digest: digest(0xab),
        handoff_digest: Some(digest(0xcd)),
        evidence_digest: None,
        model: "model-x".into(),
        question_version: QuestionVersion("q1".into()),
        policy_version: ConfigVersion("cfg-1".into()),
        outcome: JudgmentOutcome::Answered,
    }
}

pub(super) fn judgment(question: Question) -> Judgment {
    let mut probabilities = BTreeMap::new();
    probabilities.insert("yes".to_owned(), Probability(0.75));
    probabilities.insert("no".to_owned(), Probability(0.25));
    Judgment {
        question,
        probabilities,
        answer: "yes".into(),
        threshold: Some(0.6),
    }
}

pub(super) fn record() -> JudgmentRecord {
    JudgmentRecord {
        set: judgment_set(Some(VersionTriple {
            version: 9,
            work_generation: 3,
            evidence_generation: 4,
        })),
        judgments: vec![
            judgment(Question::HandoffMeetsItem { item: 1 }),
            judgment(Question::DoneWhenVerifiable),
        ],
    }
}

pub(super) fn effect(target: Option<EffectTarget>, receipt: Option<EffectReceipt>) -> Effect {
    Effect {
        id: EffectId("e-1".into()),
        key: EffectKey("run:r-1:prompt:task".into()),
        kind: EffectKind::Prompt,
        subject_launch: None,
        subject_run: Some(RunId("r-1".into())),
        target,
        payload_digest: Some(digest(0x33)),
        state: EffectState::Acknowledged,
        certainty: None,
        receipt,
        dispatched_at: Some(Timestamp(NOW.0 + 10)),
    }
}

#[test]
fn timestamp_encodes_rfc3339_utc_millis() {
    assert_eq!(
        ts_encode(Timestamp(0), T, C).unwrap(),
        "1970-01-01T00:00:00.000Z"
    );
    assert_eq!(ts_encode(NOW, T, C).unwrap(), "2026-10-01T00:00:00.000Z");
    assert_eq!(
        ts_encode(Timestamp(951_782_400_123), T, C).unwrap(),
        "2000-02-29T00:00:00.123Z",
        "leap day in a 400-year leap year"
    );
    assert_eq!(
        ts_encode(Timestamp(-1), T, C).unwrap(),
        "1969-12-31T23:59:59.999Z",
        "pre-epoch instants floor towards the past"
    );
}

#[test]
fn timestamp_round_trips_across_the_range() {
    // A deterministic sweep: a fixed-stride walk plus an LCG scatter over the
    // whole RFC3339 year range (0000-01-01 … 9999-12-31T23:59:59.999Z),
    // every point decoding to itself.
    let lo: i64 = -62_167_219_200_000;
    let hi: i64 = 253_402_300_799_999;
    let stride = (hi - lo).div_euclid(20_000);
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for step in 0..20_000_i64 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let scatter = i64::try_from(seed.rem_euclid(1_000_000_007)).unwrap() - 500_000_003;
        let millis = (lo + step * stride + scatter).clamp(lo, hi);
        let ts = Timestamp(millis);
        let text = ts_encode(ts, T, C).unwrap();
        assert_eq!(text.len(), 24, "fixed width: {text}");
        assert_eq!(ts_decode(&text, T, C).unwrap(), ts, "round-trip of {text}");
    }
}

#[test]
fn timestamp_decode_rejects_foreign_shapes() {
    for bad in [
        "2026-10-01T00:00:00Z",
        "2026-10-01T00:00:00.000",
        "2026-10-01 00:00:00.000Z",
        "2026-13-01T00:00:00.000Z",
        "2026-02-29T00:00:00.000Z",
        "2026-10-01T24:00:00.000Z",
        "2026-10-01T00:00:60.000Z",
        "2026-10-01T00:00:00.000+00:00",
        "",
    ] {
        assert!(
            matches!(
                ts_decode(bad, T, C),
                Err(StoreError::CorruptRow {
                    table: "t",
                    column: "c",
                    ..
                })
            ),
            "{bad:?} must be a typed corrupt-row error"
        );
    }
}

#[test]
fn timestamp_beyond_year_9999_is_a_typed_error() {
    // `Timestamp::after` saturates to i64::MAX ("effectively never"); that
    // instant has no RFC3339 spelling and must fail loudly, not be written.
    assert!(
        matches!(
            ts_encode(Timestamp(i64::MAX), T, C),
            Err(StoreError::CorruptRow { .. })
        ),
        "i64::MAX has no RFC3339 spelling"
    );
}

#[test]
fn time_columns_survive_julianday() {
    // The `outcomes` view computes `julianday(settled_at) - julianday(created_at)`:
    // the codec's spelling must parse, and the difference must be exact.
    let conn = Connection::open_in_memory().unwrap();
    let created = ts_encode(NOW, T, C).unwrap();
    let settled = ts_encode(Timestamp(NOW.0 + 90_000), T, C).unwrap();
    let seconds: Option<f64> = conn
        .query_row(
            "SELECT (julianday(?2) - julianday(?1)) * 86400",
            [&created, &settled],
            |row| row.get(0),
        )
        .unwrap();
    let elapsed = seconds.expect("julianday must parse the stored spelling");
    assert!((elapsed - 90.0).abs() < 1e-3, "90s apart, got {elapsed}");
}

#[test]
fn digest_round_trips_as_lowercase_hex() {
    let mut bytes = [0_u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from((i * 8).rem_euclid(256)).unwrap();
    }
    let text = hex_encode(Digest(bytes));
    assert_eq!(text.len(), 64, "32 bytes → 64 chars");
    assert!(
        text.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(hex_decode(&text, T, C).unwrap(), Digest(bytes));
    for bad in ["", "00", &"0".repeat(63), &"g".repeat(64), &"A".repeat(64)] {
        assert!(hex_decode(bad, T, C).is_err(), "{bad:?} must be rejected");
    }
}
