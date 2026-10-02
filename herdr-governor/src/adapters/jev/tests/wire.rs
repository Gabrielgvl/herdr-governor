//! Pure wire tests: request key order against the launch fixture, every
//! answer shape in the three recorded responses, and each malformed-200
//! rule — no socket.

use governor_core::routing::{JudgmentOutcome, Probability, Question};
use governor_core::task::Task;
use serde_json::{Value, json};

use super::super::error::{JevError, http_component};
use super::super::wire::{
    Kind, Questions, Request, State, TaskState, WireQuestion, decode, encode, error_type,
    question_name,
};
use super::{LAUNCH, RAW, SUPERVISION, fixture_questions, json as parse};

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
    let t = &parse(LAUNCH)["request"]["body"]["state"]["task"];
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
    let fixture = parse(LAUNCH)["request"]["body"].clone();
    let questions = fixture_questions(&fixture, &LAUNCH_ORDER);
    let state = State::Task(TaskState::from(&launch_task()));
    let bytes = encode(&Request {
        state: &state,
        questions: Questions(&questions),
        model: "jev-latest",
    })
    .expect("encode");
    let text = String::from_utf8(bytes.clone()).expect("utf8");
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).expect("json"),
        fixture
    );
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

#[test]
fn question_names_render_spec_spelling() {
    assert_eq!(
        question_name(&Question::DoneWhenVerifiable),
        "done_when_verifiable"
    );
    assert_eq!(
        question_name(&Question::HandoffMeetsItem { item: 3 }),
        "handoff_meets_item_3"
    );
}

/// `noul: 0.34` → `{"yes": 0.34}`, verdict `no` at the 0.5 bound;
/// `noul: 0.85` → `yes`.
#[test]
fn noul_maps_to_yes_probability() {
    let raw = parse(RAW)["raw_body"].clone();
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
    let launch = parse(LAUNCH)["response"].clone();
    let launch_asked = fixture_questions(&parse(LAUNCH)["request"]["body"], &LAUNCH_ORDER);
    let launch_decoded = decode(launch.to_string().as_bytes(), &launch_asked).expect("decode");
    assert_eq!(
        launch_decoded.answers[0].probabilities["yes"],
        Probability(0.85)
    );
    assert_eq!(launch_decoded.answers[0].answer, "yes");
}

/// p ≥ bound is `yes`; a custom threshold moves the bound.
#[test]
fn noul_verdict_threshold() {
    let body = json!({"model": "m", "answers": {"q": {"type": "noul", "noul": 0.5}}});
    let verdict = |threshold| {
        decode(body.to_string().as_bytes(), &[noul("q", threshold)])
            .expect("decode")
            .answers[0]
            .answer
            .clone()
    };
    assert_eq!(verdict(None), "yes");
    assert_eq!(verdict(Some(0.5)), "yes");
    assert_eq!(verdict(Some(0.51)), "no");
    assert_eq!(verdict(Some(0.2)), "yes");
}

/// A choice keeps its distribution verbatim and its label; `confidence`
/// never appears in the decoded answer.
#[test]
fn choice_probabilities_verbatim_and_confidence_dropped() {
    let raw = parse(RAW)["raw_body"].clone();
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
    let fixture = parse(SUPERVISION);
    let asked = fixture_questions(&fixture["request"]["body"], &SUPERVISION_ORDER);
    let decoded = decode(fixture["response"].to_string().as_bytes(), &asked).expect("decode");
    assert_eq!(decoded.answers.len(), 7);
    assert_eq!(decoded.answers[1].answer, "yes");
    assert_eq!(decoded.answers[2].answer, "no");
    let reason = &decoded.answers[6];
    assert_eq!(reason.answer, "verification_passed");
    let sum: f64 = reason.probabilities.values().map(|p| p.0).sum();
    assert!((sum - 0.99).abs() < 1e-9, "{sum}");
}

/// `model` is recorded verbatim; `usage` and unknown keys are ignored.
#[test]
fn response_metadata_opaque() {
    let body = json!({
        "model": "jev-9.9.9-rc1",
        "answers": {"q": {"type": "noul", "noul": 0.7}},
        "usage": {"input_tokens": 1, "output_tokens": 2},
        "requestId": "req_x"
    });
    let decoded = decode(body.to_string().as_bytes(), &[noul("q", None)]).expect("decode");
    assert_eq!(decoded.model, "jev-9.9.9-rc1");
    assert_eq!(decoded.answers.len(), 1);
}

#[test]
fn malformed_200_rules_are_invalid_response() {
    let cases: [(&str, Value, Vec<WireQuestion>); 9] = [
        ("body_shape", json!("not an object"), vec![noul("q", None)]),
        ("body_shape", json!({"answers": {}}), vec![]),
        ("body_shape", json!({"model": 1, "answers": {}}), vec![]),
        (
            "answer_set_mismatch",
            json!({"model": "m", "answers": {}}),
            vec![noul("q", None)],
        ),
        (
            "answer_set_mismatch",
            json!({"model": "m", "answers": {"q": {"type": "noul", "noul": 0.5}, "extra": {"type": "noul", "noul": 0.5}}}),
            vec![noul("q", None)],
        ),
        (
            "body_shape",
            json!({"model": "m", "answers": {"q": {"type": "mystery", "noul": 0.5}}}),
            vec![noul("q", None)],
        ),
        (
            "answer_type_mismatch",
            json!({"model": "m", "answers": {"q": {"type": "choice", "choice": "a", "probabilities": {"a": 1.0}}}}),
            vec![noul("q", None)],
        ),
        (
            "probability_out_of_range",
            json!({"model": "m", "answers": {"q": {"type": "noul", "noul": 1.5}}}),
            vec![noul("q", None)],
        ),
        (
            "choice_not_in_distribution",
            json!({"model": "m", "answers": {"q": {"type": "choice", "choice": "zz", "probabilities": {"a": 1.0}}}}),
            vec![choice("q")],
        ),
    ];
    for (expected, body, asked) in cases {
        let err = decode(body.to_string().as_bytes(), &asked).expect_err(expected);
        assert_eq!(
            err.outcome(),
            JudgmentOutcome::InvalidResponse,
            "{expected}"
        );
        assert_eq!(err.component(), expected, "{err}");
    }
    let mistyped = json!({"model": "m", "answers": {"q": {"type": "noul", "noul": "0.5"}}});
    assert!(matches!(
        decode(mistyped.to_string().as_bytes(), &[noul("q", None)]),
        Err(JevError::InvalidResponse {
            detail: "body_shape"
        })
    ));
}

#[test]
fn error_component_rule() {
    assert_eq!(
        http_component(401, Some("authentication_error")),
        "http_401_authentication_error"
    );
    assert_eq!(http_component(400, Some("Bad-Type!")), "http_400");
    assert_eq!(http_component(400, Some("")), "http_400");
    assert_eq!(http_component(400, Some(&"a".repeat(65))), "http_400");
    assert_eq!(
        http_component(400, Some(&"a".repeat(64))),
        format!("http_400_{}", "a".repeat(64))
    );
    assert_eq!(http_component(500, None), "http_500");
    assert_eq!(
        error_type(br#"{"detail":{"error_type":"max_tokens_exceeded"}}"#).as_deref(),
        Some("max_tokens_exceeded")
    );
    assert_eq!(error_type(b"<html>"), None);
    assert_eq!(error_type(br#"{"detail":{"message":"x"}}"#), None);
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
