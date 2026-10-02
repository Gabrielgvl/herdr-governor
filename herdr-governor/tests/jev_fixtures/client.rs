//! Fake-server tests driven by the recorded contract fixtures: the
//! captured request (`wire_shape`), the evidence-recorded error classes →
//! `JudgmentOutcome`, the no-socket-write size gate, and the bounded body
//! read. No test-side sleeps: the only waits are the client's own deadline
//! and the paused-clock accept probe in the size-gate test. Fixture-free
//! client tests stay in `src/adapters/jev/tests/client.rs`.

use std::time::Duration;

use governor_core::routing::{JEV_REQUEST_MAX_BYTES, JudgmentOutcome, Probability, Question};
use serde_json::{Value, json};

use herdr_governor::adapters::jev::client::{JudgeParams, Judged, QuestionSpec};
use herdr_governor::adapters::jev::error::JevError;
use herdr_governor::adapters::jev::wire::{JEV_RESPONSE_MAX_BYTES, Kind, State, TaskState};

use crate::support::{
    Captured, FAKE_KEY, Reply, fake_key, fixture, json as parse, respond, serve_once, server,
};

fn spec(question: Question, kind: Kind, body: &Value, name: &str) -> QuestionSpec {
    let q = &body["request"]["body"]["questions"][name];
    let mut criteria: Vec<(String, String)> = q["criteria"]
        .as_object()
        .expect("criteria")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("text").to_owned()))
        .collect();
    criteria.sort_by_key(|(k, _)| k != "true");
    QuestionSpec {
        question,
        kind,
        instructions: q["instructions"].as_str().expect("instructions").to_owned(),
        criteria,
    }
}

fn params() -> JudgeParams {
    let body = parse(&fixture("jev-launch-evaluation.json"));
    let t = &body["request"]["body"]["state"]["task"];
    let strings = |v: &Value| {
        v.as_array()
            .expect("array")
            .iter()
            .map(|s| s.as_str().expect("str").to_owned())
            .collect()
    };
    JudgeParams {
        model: "jev-latest".to_owned(),
        state: State::Task(TaskState {
            objective: t["objective"].as_str().expect("objective").to_owned(),
            scope: t["scope"].as_str().expect("scope").to_owned(),
            done_when: strings(&t["doneWhen"]),
            constraints: strings(&t["constraints"]),
        }),
        questions: vec![
            spec(
                Question::DoneWhenVerifiable,
                Kind::Noul { threshold: None },
                &body,
                "done_when_verifiable",
            ),
            spec(
                Question::WeakestSufficientTier,
                Kind::Choice,
                &body,
                "weakest_sufficient_tier",
            ),
        ],
        timeout: Duration::from_secs(5),
    }
}

/// The fixture response minus the `intent` answer the governor never asks.
fn answered_body() -> String {
    let mut response = parse(&fixture("jev-launch-evaluation.json"))["response"].clone();
    response["answers"]
        .as_object_mut()
        .expect("answers")
        .remove("intent");
    response.to_string()
}

/// Run one call against the fake. A `Silent` fake never finishes, so it
/// is aborted once the client has decided and no capture is returned.
async fn call(reply: Reply, params: &JudgeParams) -> (Option<Captured>, Result<Judged, JevError>) {
    let (listener, client) = server().await;
    let (_dir, key) = fake_key().await;
    let silent = matches!(reply, Reply::Silent);
    let fake = tokio::spawn(async move { serve_once(&listener, reply).await });
    let result = client.judge(&key, params).await;
    if silent {
        fake.abort();
        (None, result)
    } else {
        (Some(fake.await.expect("fake server")), result)
    }
}

async fn fail(reply: Reply) -> JevError {
    call(reply, &params()).await.1.expect_err("must fail")
}

fn evidence_error(probe: &str) -> (u16, String) {
    let errors = parse(&fixture("jev-wire-evidence.json"))["errors"].clone();
    let e = errors
        .as_array()
        .expect("errors")
        .iter()
        .find(|e| e["probe"] == probe)
        .expect(probe);
    let status = u16::try_from(e["status"].as_u64().expect("status")).expect("u16");
    let body = e.get("body").map_or_else(
        || e["body_prefix"].as_str().expect("prefix").to_owned(),
        Value::to_string,
    );
    (status, body)
}

/// Method, path, headers and body as recorded: `{state,questions,model}`
/// with no operating point anywhere in `state`.
#[tokio::test]
async fn wire_shape() {
    let (request, result) = call(respond(200, &answered_body()), &params()).await;
    let captured = request.expect("captured");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/v1/systemone");
    assert_eq!(
        captured.header("authorization"),
        Some(format!("Bearer {FAKE_KEY}").as_str())
    );
    assert_eq!(captured.header("content-type"), Some("application/json"));
    assert_eq!(captured.header("accept"), Some("application/json"));
    let body: Value = serde_json::from_slice(&captured.body).expect("json body");
    let mut expected = parse(&fixture("jev-launch-evaluation.json"))["request"]["body"].clone();
    expected["questions"]
        .as_object_mut()
        .expect("questions")
        .remove("intent");
    assert_eq!(body, expected);
    let text = String::from_utf8(captured.body).expect("utf8");
    assert!(text.starts_with(r#"{"state":{"task":{"#), "{text}");
    assert!(text.ends_with(r#","model":"jev-latest"}"#), "{text}");
    let judged = result.expect("answered");
    assert_eq!(judged.model, "jev-1.13.0");
    assert_eq!(judged.judgments.len(), 2);
    assert_eq!(judged.judgments[0].question, Question::DoneWhenVerifiable);
    assert_eq!(judged.judgments[0].probabilities["yes"], Probability(0.85));
    assert_eq!(judged.judgments[0].answer, "yes");
    assert_eq!(judged.judgments[0].threshold, None);
    assert_eq!(
        judged.judgments[1].question,
        Question::WeakestSufficientTier
    );
    assert_eq!(judged.judgments[1].answer, "economy");
    assert_eq!(judged.judgments[1].probabilities.len(), 6);
    assert_eq!(judged.judgments[1].threshold, None);
}

#[tokio::test]
async fn noul_threshold_is_stamped_on_the_judgment() {
    let mut p = params();
    p.questions[0].kind = Kind::Noul {
        threshold: Some(0.9),
    };
    let (_, result) = call(respond(200, &answered_body()), &p).await;
    let judged = result.expect("answered");
    assert_eq!(judged.judgments[0].threshold, Some(0.9));
    assert_eq!(judged.judgments[0].answer, "no");
}

/// `bad-key`: 401 `authentication_error` → `AuthFailed`,
/// `http_401_authentication_error`.
#[tokio::test]
async fn error_401_maps_auth_failed() {
    let (status, body) = evidence_error("bad-key");
    let err = fail(respond(status, &body)).await;
    assert_eq!(err.outcome(), JudgmentOutcome::AuthFailed);
    assert_eq!(err.component(), "http_401_authentication_error");
}

#[tokio::test]
async fn error_403_maps_auth_failed() {
    let err = fail(respond(403, "forbidden")).await;
    assert_eq!(err.outcome(), JudgmentOutcome::AuthFailed);
    assert_eq!(err.component(), "http_403");
}

/// `malformed-question`: 400 `api_usage_error` → `TransportFailed`.
#[tokio::test]
async fn error_400_api_usage_error() {
    let (status, body) = evidence_error("malformed-question");
    let err = fail(respond(status, &body)).await;
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "http_400_api_usage_error");
}

/// `oversize-256KB-raw`: 400 `max_tokens_exceeded` (body prefix only).
#[tokio::test]
async fn error_400_max_tokens_exceeded() {
    let (status, body) = evidence_error("oversize-256KB-raw");
    let err = fail(respond(status, &body)).await;
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "http_400_max_tokens_exceeded");
}

/// A non-conforming `error_type` falls back to `http_<status>`.
#[tokio::test]
async fn error_nonconforming_error_type_falls_back() {
    let body = json!({"detail": {"error_type": "Not-Conforming!", "message": "x"}}).to_string();
    let err = fail(respond(422, &body)).await;
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "http_422");
    assert_eq!(fail(respond(404, "")).await.component(), "http_404");
}

/// `timeout-1ms`: the deadline elapses on a silent server → `Timeout`,
/// `transport`. Real clock: reqwest's connector resolves on the blocking
/// pool, which holds the paused clock's auto-advance; the wait is the
/// client's own 50 ms deadline, no test-side sleep.
#[tokio::test]
async fn error_timeout() {
    let mut p = params();
    p.timeout = Duration::from_millis(50);
    let err = call(Reply::Silent, &p).await.1.expect_err("silent");
    assert!(matches!(err, JevError::Timeout), "{err}");
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "transport");
}

/// `pre-abort`: the peer closes without a status line → `Transport`.
#[tokio::test]
async fn error_abort() {
    let err = fail(Reply::Abort).await;
    assert!(matches!(err, JevError::Transport(_)), "{err}");
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "transport");
}

#[tokio::test]
async fn error_connect_refused() {
    let (listener, client) = server().await;
    drop(listener);
    let (_dir, key) = fake_key().await;
    let err = client.judge(&key, &params()).await.expect_err("refused");
    assert!(matches!(err, JevError::Transport(_)), "{err}");
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
}

/// 429 with `retry-after-ms` recorded; the seconds form is converted.
#[tokio::test]
async fn error_429_retry_after_recorded() {
    let body = json!({"detail": {"error_type": "rate_limit_error"}}).to_string();
    let reply = Reply::Respond {
        status: 429,
        headers: vec![("retry-after-ms", "1500".to_owned())],
        body: body.clone().into_bytes(),
    };
    let err = fail(reply).await;
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert!(
        matches!(&err, JevError::Http { status: 429, component, retry_after_ms: Some(1500) }
            if component == "http_429_rate_limit_error"),
        "{err:?}"
    );
    let seconds = Reply::Respond {
        status: 429,
        headers: vec![("retry-after", "2".to_owned())],
        body: body.into_bytes(),
    };
    assert!(matches!(
        fail(seconds).await,
        JevError::Http {
            status: 429,
            component: _,
            retry_after_ms: Some(2000)
        }
    ));
}

#[tokio::test]
async fn error_5xx() {
    let err = fail(respond(503, "<html>upstream</html>")).await;
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
    assert_eq!(err.component(), "http_503");
}

/// Redirects are never followed: the bearer stays on the configured host.
#[tokio::test]
async fn error_redirect_not_followed() {
    let reply = Reply::Respond {
        status: 302,
        headers: vec![("location", "http://127.0.0.1:1/v1/systemone".to_owned())],
        body: Vec::new(),
    };
    let err = fail(reply).await;
    assert!(matches!(err, JevError::Http { status: 302, .. }), "{err}");
    assert_eq!(err.outcome(), JudgmentOutcome::TransportFailed);
}

/// A 200 that breaks the contract (partial answer set) → `InvalidResponse`.
#[tokio::test]
async fn error_malformed_200() {
    let body = json!({"model": "jev-1.13.0", "answers": {"done_when_verifiable": {"type": "noul", "noul": 0.9}}});
    let err = fail(respond(200, &body.to_string())).await;
    assert_eq!(err.outcome(), JudgmentOutcome::InvalidResponse);
    assert_eq!(err.component(), "answer_set_mismatch");
    let not_json = fail(respond(200, "not json")).await;
    assert_eq!(not_json.outcome(), JudgmentOutcome::InvalidResponse);
}

#[tokio::test]
async fn error_response_over_bound() {
    let padding = "x".repeat(JEV_RESPONSE_MAX_BYTES);
    let body = json!({"model": "m", "answers": {}, "pad": padding}).to_string();
    let err = fail(respond(200, &body)).await;
    assert_eq!(err.outcome(), JudgmentOutcome::InvalidResponse);
    assert_eq!(err.component(), "response_too_large");
}

/// `oversize-client-side`: a body over `JEV_REQUEST_MAX_BYTES` is
/// `TooLarge` and nothing ever connects to the server.
#[tokio::test(start_paused = true)]
async fn jev_request_too_large_client_gate() {
    let (listener, client) = server().await;
    let (_dir, key) = fake_key().await;
    let mut p = params();
    p.questions[0].instructions = "x".repeat(JEV_REQUEST_MAX_BYTES);
    let err = client.judge(&key, &p).await.expect_err("too large");
    assert!(
        matches!(err, JevError::TooLarge { bytes } if bytes > JEV_REQUEST_MAX_BYTES),
        "{err}"
    );
    assert_eq!(err.outcome(), JudgmentOutcome::TooLarge);
    assert_eq!(err.component(), "request_too_large");
    let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
    assert!(
        accepted.is_err(),
        "a socket was written for an oversize request"
    );
}
