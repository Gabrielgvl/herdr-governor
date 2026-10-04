//! Pure wire tests against the committed fixtures: request key order
//! against the launch fixture and the recorded response shapes — no
//! socket. Fixture-free wire tests stay in `src/adapters/jev/tests/wire.rs`.

use std::collections::BTreeSet;

use governor_core::routing::Probability;
use governor_core::task::Task;
use serde_json::Value;

use herdr_governor::adapters::jev::wire::{
    AcceptanceState, BlockedState, GitState, Kind, LimitRecordState, Questions, Request,
    ReviewState, State, TaskDigest, TaskState, TranscriptLine, WireQuestion, decode, encode,
};

use crate::support::{fixture, fixture_questions, json as parse};

const LAUNCH_ORDER: [&str; 3] = ["done_when_verifiable", "intent", "weakest_sufficient_tier"];
const SUPERVISION_ORDER: [&str; 7] = [
    "evidence_sufficient",
    "progress",
    "stalled",
    "blocked",
    "risk",
    "appears_complete",
    "reason",
];
const REVIEW_ORDER: [&str; 3] = ["blocked_on_input", "no_recent_progress", "outside_scope"];
const BLOCKED_ORDER: [&str; 2] = ["provider_limited", "blocked_on_input"];
const ACCEPTANCE_ORDER: [&str; 2] = ["handoff_meets_item_0", "handoff_meets_item_1"];

fn launch_task() -> Task {
    let body = parse(&fixture("jev-launch-evaluation.json"));
    let t = &body["request"]["body"]["state"]["task"];
    let strings = |v: &Value| {
        v.as_array()
            .expect("array")
            .iter()
            .map(|s| s.as_str().expect("str").to_owned())
            .collect()
    };
    Task {
        objective: t["objective"].as_str().expect("objective").to_owned(),
        scope: t["scope"].as_str().expect("scope").to_owned(),
        done_when: strings(&t["doneWhen"]),
        constraints: strings(&t["constraints"]),
        tier: Some(governor_core::config::Tier("leaked-tier".to_owned())),
        recovery_of: Some(governor_core::identity::RunId("run-1".to_owned())),
        label: Some("secret label".to_owned()),
        cwd: Some("/tmp/x".to_owned()),
        retention: Some(governor_core::task::Retention::Keep),
    }
}

/// The encoded launch request equals the fixture body and keeps its key
/// order: `state`, `questions`, `model`; criteria `true` before `false`.
#[test]
fn launch_request_matches_fixture_bytes() {
    let body = parse(&fixture("jev-launch-evaluation.json"))["request"]["body"].clone();
    let questions = fixture_questions(&body, &LAUNCH_ORDER);
    let state = State::Task(TaskState::from(&launch_task()));
    let bytes = encode(&Request {
        state: &state,
        questions: Questions(&questions),
        model: "jev-latest",
    })
    .expect("encode");
    let text = String::from_utf8(bytes.clone()).expect("utf8");
    assert_eq!(serde_json::from_slice::<Value>(&bytes).expect("json"), body);
    let at = |needle: &str| text.find(needle).expect(needle);
    assert!(
        text.starts_with(r#"{"state":{"task":{"objective":"#),
        "{text}"
    );
    assert!(
        at(r#""questions":"#) < at(r#""model":"jev-latest"}"#),
        "{text}"
    );
    assert!(
        at(r#""done_when_verifiable":"#) < at(r#""intent":"#),
        "{text}"
    );
    assert!(
        at(r#""intent":"#) < at(r#""weakest_sufficient_tier":"#),
        "{text}"
    );
    assert!(at(r#""true":"#) < at(r#""false":"#), "{text}");
    assert!(text.ends_with(r#","model":"jev-latest"}"#), "{text}");
    for leaked in ["secret label", "/tmp/x", "run-1", "leaked-tier"] {
        assert!(!text.contains(leaked), "{leaked} leaked into the request");
    }
}

#[test]
fn state_has_only_the_four_task_fields() {
    let state = serde_json::to_value(State::Task(TaskState::from(&launch_task()))).expect("value");
    let keys: Vec<&String> = state["task"].as_object().expect("task").keys().collect();
    assert_eq!(keys, ["constraints", "doneWhen", "objective", "scope"]);
}

/// `noul: 0.34` → `{"yes": 0.34}`, verdict `no` at the 0.5 bound;
/// `noul: 0.85` → `yes`.
#[test]
fn noul_maps_to_yes_probability() {
    let raw = parse(&fixture("jev-raw-response.json"))["raw_body"].clone();
    let asked = [
        noul("done_when_verifiable", None),
        WireQuestion {
            name: "sized".to_owned(),
            kind: Kind::Choice,
            instructions: String::new(),
            criteria: Vec::new(),
        },
    ];
    let decoded = decode(raw.to_string().as_bytes(), &asked).expect("decode");
    assert_eq!(decoded.answers[0].probabilities["yes"], Probability(0.34));
    assert_eq!(decoded.answers[0].answer, "no");
    let launch_text = fixture("jev-launch-evaluation.json");
    let launch = parse(&launch_text)["response"].clone();
    let launch_asked = fixture_questions(&parse(&launch_text)["request"]["body"], &LAUNCH_ORDER);
    let launch_decoded = decode(launch.to_string().as_bytes(), &launch_asked).expect("decode");
    assert_eq!(
        launch_decoded.answers[0].probabilities["yes"],
        Probability(0.85)
    );
    assert_eq!(launch_decoded.answers[0].answer, "yes");
}

/// A choice keeps its distribution verbatim and its label; `confidence`
/// never appears in the decoded answer.
#[test]
fn choice_probabilities_verbatim_and_confidence_dropped() {
    let raw = parse(&fixture("jev-raw-response.json"))["raw_body"].clone();
    let asked = [noul("done_when_verifiable", None), choice("sized")];
    let decoded = decode(raw.to_string().as_bytes(), &asked).expect("decode");
    let sized = &decoded.answers[1];
    assert_eq!(sized.answer, "small");
    assert_eq!(sized.probabilities.len(), 2);
    assert_eq!(sized.probabilities["small"], Probability(0.98));
    assert_eq!(sized.probabilities["large"], Probability(0.02));
    assert!(!sized.probabilities.contains_key("confidence"));
    assert_eq!(decoded.model, "jev-1.13.0");
}

/// The seven-question review decodes; `reason` sums to 0.99 and is
/// accepted — no exact-1 enforcement.
#[test]
fn supervision_review_shape() {
    let review = parse(&fixture("jev-supervision-review.json"));
    let asked = fixture_questions(&review["request"]["body"], &SUPERVISION_ORDER);
    let decoded = decode(review["response"].to_string().as_bytes(), &asked).expect("decode");
    assert_eq!(decoded.answers.len(), 7);
    assert_eq!(decoded.answers[1].answer, "yes");
    assert_eq!(decoded.answers[2].answer, "no");
    let reason = &decoded.answers[6];
    assert_eq!(reason.answer, "verification_passed");
    let sum: f64 = reason.probabilities.values().map(|p| p.0).sum();
    assert!((sum - 0.99).abs() < 1e-9, "{sum}");
}

/// A Task carrying every routing-adjacent field — `tier`, `recovery_of`,
/// `label`, `cwd`, `retention` — so the golden and key-set tests prove
/// the evidence projections drop them (F12, F23/F24, F30, H#41).
fn evidence_task() -> Task {
    Task {
        objective: "Implement the Jev evidence request states".to_owned(),
        scope: "the Jev wire adapter".to_owned(),
        done_when: vec![
            "review, blocked and acceptance states carry the spec evidence fields".to_owned(),
            "the request-side goldens pass byte for byte".to_owned(),
        ],
        constraints: vec!["never send routing fields to Jev".to_owned()],
        tier: Some(governor_core::config::Tier("leaked-tier".to_owned())),
        recovery_of: Some(governor_core::identity::RunId("run-1".to_owned())),
        label: Some("secret label".to_owned()),
        cwd: Some("/tmp/x".to_owned()),
        retention: Some(governor_core::task::Retention::Keep),
    }
}

fn transcript_lines() -> Vec<TranscriptLine> {
    vec![
        TranscriptLine {
            timestamp: Some("2026-10-03T04:00:00Z".to_owned()),
            role: Some("assistant".to_owned()),
            kind: "tool_call".to_owned(),
            text: Some("just test".to_owned()),
        },
        TranscriptLine {
            timestamp: Some("2026-10-03T04:00:11Z".to_owned()),
            role: None,
            kind: "tool_result".to_owned(),
            text: Some("655 passed".to_owned()),
        },
    ]
}

fn terminal_text() -> String {
    "$ just test\n655 passed".to_owned()
}

fn git_state() -> GitState {
    GitState {
        head: "e17f8fa3b9c2d1a0f5e6b7c8d9a0b1c2d3e4f5a6".to_owned(),
        dirty: vec!["herdr-governor/src/adapters/jev/wire.rs".to_owned()],
    }
}

fn limit_record() -> LimitRecordState {
    LimitRecordState {
        source: "claude_session_quota".to_owned(),
        observed_at: "2026-10-03T03:41:02Z".to_owned(),
        reset_at: Some("2026-10-03T19:20:00Z".to_owned()),
    }
}

fn review_state() -> ReviewState {
    ReviewState {
        task: TaskDigest::from(&evidence_task()),
        scope: evidence_task().scope,
        transcript: transcript_lines(),
        terminal: Some(terminal_text()),
        git: Some(git_state()),
    }
}

fn blocked_state(limit_record: Option<LimitRecordState>) -> BlockedState {
    BlockedState {
        task: TaskDigest::from(&evidence_task()),
        transcript: transcript_lines(),
        terminal: Some(terminal_text()),
        git: Some(git_state()),
        limit_record,
    }
}

fn acceptance_state() -> AcceptanceState {
    AcceptanceState {
        task: TaskDigest::from(&evidence_task()),
        handoff: "## Handoff\n\nImplemented the three Jev evidence request states and their request-side goldens.\n\n## Verification\n- just test: green\n- just lint: green\n\n<!-- herdr-governor handoff run=01994c4e-7a3b-7f2e-9d1c-2b3a4f5e6d7c -->".to_owned(),
        transcript: transcript_lines(),
        terminal: Some(terminal_text()),
        git: Some(git_state()),
    }
}

/// The serialized key set of a JSON object value.
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

/// The encoded request equals the hand-authored golden file byte for
/// byte — the fixture is the contract, the serializer must reproduce it.
fn assert_matches_golden(name: &str, state: &State, order: &[&str]) {
    let fixture_text = fixture(name);
    let questions = fixture_questions(&parse(&fixture_text), order);
    let bytes = encode(&Request {
        state,
        questions: Questions(&questions),
        model: "jev-latest",
    })
    .expect("encode");
    assert_eq!(
        String::from_utf8(bytes).expect("utf8"),
        fixture_text.trim_end()
    );
}

/// F23 review ask: the Task digest, `scope` for `outside_scope`, the
/// transcript tail, the terminal fallback and the git evidence.
#[test]
fn review_request_matches_golden_bytes() {
    assert_matches_golden(
        "jev-review-request.json",
        &State::Review(review_state()),
        &REVIEW_ORDER,
    );
}

/// F23/F21 blocked ask without a limit record: no `limitRecord` key.
#[test]
fn blocked_request_matches_golden_bytes() {
    assert_matches_golden(
        "jev-blocked-request.json",
        &State::Blocked(blocked_state(None)),
        &BLOCKED_ORDER,
    );
}

/// F31 blocked ask carrying the typed limit record as evidence:
/// `limitRecord` renders `{source, observedAt, resetAt}`.
#[test]
fn blocked_request_with_limit_record_matches_golden_bytes() {
    assert_matches_golden(
        "jev-blocked-limit-request.json",
        &State::Blocked(blocked_state(Some(limit_record()))),
        &BLOCKED_ORDER,
    );
}

/// F24 acceptance ask: the frozen handoff rides the same evidence
/// bundle, judged per `handoff_meets_item_k`.
#[test]
fn acceptance_request_matches_golden_bytes() {
    assert_matches_golden(
        "jev-acceptance-request.json",
        &State::Acceptance(acceptance_state()),
        &ACCEPTANCE_ORDER,
    );
}

/// The contract check (F12/F23/F24): every evidence state serializes to
/// exactly the spec field set — no operating point, provider, tier,
/// label, caller or Run identity can reach Jev through a `State`.
#[test]
fn evidence_states_have_no_routing_fields() {
    let review = serde_json::to_value(State::Review(review_state())).expect("value");
    let blocked = serde_json::to_value(State::Blocked(blocked_state(None))).expect("value");
    let limited =
        serde_json::to_value(State::Blocked(blocked_state(Some(limit_record())))).expect("value");
    let acceptance = serde_json::to_value(State::Acceptance(acceptance_state())).expect("value");
    assert_eq!(
        keys(&review["review"]),
        BTreeSet::from(["git", "scope", "task", "terminal", "transcript"])
    );
    assert_eq!(
        keys(&blocked["blocked"]),
        BTreeSet::from(["git", "task", "terminal", "transcript"])
    );
    assert_eq!(
        keys(&limited["blocked"]),
        BTreeSet::from(["git", "limitRecord", "task", "terminal", "transcript"])
    );
    assert_eq!(
        keys(&limited["blocked"]["limitRecord"]),
        BTreeSet::from(["observedAt", "resetAt", "source"])
    );
    assert_eq!(
        keys(&acceptance["acceptance"]),
        BTreeSet::from(["git", "handoff", "task", "terminal", "transcript"])
    );
    let no_reset = serde_json::to_value(LimitRecordState {
        reset_at: None,
        ..limit_record()
    })
    .expect("value");
    assert_eq!(keys(&no_reset), BTreeSet::from(["observedAt", "source"]));
    for state in [
        &review["review"],
        &blocked["blocked"],
        &limited["blocked"],
        &acceptance["acceptance"],
    ] {
        assert_eq!(
            keys(&state["task"]),
            BTreeSet::from(["constraints", "doneWhen", "objective"])
        );
        assert_eq!(keys(&state["git"]), BTreeSet::from(["dirty", "head"]));
        assert_eq!(
            keys(&state["transcript"][0]),
            BTreeSet::from(["kind", "role", "text", "timestamp"])
        );
    }
    let text = [&review, &blocked, &limited, &acceptance]
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    for leaked in ["secret label", "/tmp/x", "run-1", "leaked-tier"] {
        assert!(!text.contains(leaked), "{leaked} leaked into the state");
    }
}

fn noul(name: &str, threshold: Option<f64>) -> WireQuestion {
    WireQuestion {
        name: name.to_owned(),
        kind: Kind::Noul { threshold },
        instructions: String::new(),
        criteria: Vec::new(),
    }
}

fn choice(name: &str) -> WireQuestion {
    WireQuestion {
        name: name.to_owned(),
        kind: Kind::Choice,
        instructions: String::new(),
        criteria: Vec::new(),
    }
}
