//! Pure wire tests needing no fixture: the noul verdict bound, the
//! malformed-200 rules, the response-metadata passthrough, the question
//! name spellings and the error-component rule. The fixture-driven wire
//! tests moved to the `tests/jev_fixtures/` integration crate.

use governor_core::routing::{JudgmentOutcome, Question};
use serde_json::{Value, json};

use super::super::error::{JevError, http_component};
use super::super::wire::{Kind, WireQuestion, decode, error_type, question_name};

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
