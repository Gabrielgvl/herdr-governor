//! Pure wire tests against the committed fixtures: request key order
//! against the launch fixture and the recorded response shapes — no
//! socket. Fixture-free wire tests stay in `src/adapters/jev/tests/wire.rs`.

use governor_core::routing::Probability;
use governor_core::task::Task;
use serde_json::Value;

use herdr_governor::adapters::jev::wire::{
    Kind, Questions, Request, State, TaskState, WireQuestion, decode, encode,
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
