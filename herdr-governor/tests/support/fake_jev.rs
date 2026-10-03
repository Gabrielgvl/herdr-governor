//! `fake_jev` — the generalized fake Jev server (P5.T1): the
//! `smoke_phase4.rs` one-shot `TcpListener` fake grown into a scripted
//! `POST /v1/systemone` double. A test queues `Answers` sets — each one
//! resolved against the asked question names, and the last one repeated
//! when the queue runs dry so tick-driven re-asks stay answered — plus
//! one-shot `Fault`s: `Silent` never replies, `Abort` drops the
//! connection mid-call, and `Status` serves a non-2xx with an optional
//! typed `error_type` body and `retry-after-ms` header. Every request —
//! answered or faulted — is captured decoded, so tests assert on the
//! asks the daemon sent; `CapturedRequest::state` exposes the semantic
//! state for the F31/§13 assertions.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// The request-head bound: request-line plus headers are small; a head
/// past this is not a request the fake reads further.
const HEAD_MAX_BYTES: usize = 64 * 1024;
/// The body bound — the mirror of the client's own size gate.
const BODY_MAX_BYTES: usize = 1024 * 1024;

/// One scripted answer for an asked question name — the `answers` map's
/// wire spellings verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// `{"type":"noul","noul":<p>}`.
    Noul(f64),
    /// `{"type":"choice","choice":<label>,"probabilities":{…}}`.
    Choice {
        /// The chosen label — must name a `probabilities` key.
        choice: String,
        /// The label distribution.
        probabilities: BTreeMap<String, f64>,
    },
}

impl Answer {
    /// P(yes) = `p`.
    #[must_use]
    pub fn noul(p: f64) -> Self {
        Self::Noul(p)
    }

    /// A choice with its distribution (`probabilities` must cover
    /// `label`, or the client decodes `choice_not_in_distribution`).
    #[must_use]
    pub fn choice(label: &str, probabilities: &[(&str, f64)]) -> Self {
        Self::Choice {
            choice: label.to_owned(),
            probabilities: probabilities
                .iter()
                .map(|(name, p)| ((*name).to_owned(), *p))
                .collect(),
        }
    }
}

/// A one-shot fault consuming the next request.
#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    /// Read and capture the request, then never reply — the client's own
    /// deadline is what fires (the abstain/`Silent` scenarios).
    Silent,
    /// Read and capture the request, then drop the connection — a
    /// mid-call transport death, no HTTP status.
    Abort,
    /// A non-2xx reply: `status`, an optional typed `error_type` (the
    /// `{"detail":{"error_type":…}}` body the client's component rule
    /// reads) and an optional `retry-after-ms` header.
    Status {
        /// The HTTP status code.
        status: u16,
        /// `detail.error_type` when the body carries one.
        error_type: Option<String>,
        /// The `retry-after-ms` header value.
        retry_after_ms: Option<u64>,
    },
}

/// What the next request gets.
#[derive(Debug)]
enum Step {
    /// A 200 answers map, resolved per asked question.
    Answers(BTreeMap<String, Answer>),
    /// A one-shot fault.
    Fault(Fault),
}

/// One captured request — recorded before any fault acts, so a silenced
/// or aborted ask is still assertable.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    /// The decoded request body, verbatim — `{state, questions, model}`.
    pub body: Value,
}

impl CapturedRequest {
    /// The `state` member when present — the decoded semantic state
    /// (F31/§13 assertions).
    #[must_use]
    pub fn state(&self) -> Option<&Value> {
        self.body.get("state")
    }
}

/// The shared script + capture (never locked across an await).
#[derive(Debug, Default)]
struct Shared {
    /// The queued steps, consumed per request.
    script: VecDeque<Step>,
    /// The last served answers set — repeated once the queue runs dry.
    sticky: Option<BTreeMap<String, Answer>>,
    /// Every decoded request, in arrival order.
    requests: Vec<CapturedRequest>,
}

/// The fake Jev server: the bound port, the accept task, the live
/// connection tasks. `Drop` stops them all.
#[derive(Debug)]
pub struct FakeJev {
    base: String,
    shared: Arc<Mutex<Shared>>,
    accept: Option<JoinHandle<()>>,
    conns: Arc<Mutex<JoinSet<()>>>,
}

impl FakeJev {
    /// Bind `127.0.0.1:0` and serve — call inside a Tokio runtime.
    #[must_use]
    pub fn start() -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake jev");
        std_listener.set_nonblocking(true).expect("nonblocking");
        let port = std_listener.local_addr().expect("local addr").port();
        let listener = TcpListener::from_std(std_listener).expect("tokio listener");
        let shared = Arc::new(Mutex::new(Shared::default()));
        let conns = Arc::new(Mutex::new(JoinSet::new()));
        let accept = {
            let accept_shared = Arc::clone(&shared);
            let accept_conns = Arc::clone(&conns);
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let task_state = Arc::clone(&accept_shared);
                    accept_conns
                        .lock()
                        .expect("conns poisoned")
                        .spawn(async move { handle_conn(stream, &task_state).await });
                }
            })
        };
        Self {
            base: format!("http://127.0.0.1:{port}"),
            shared,
            accept: Some(accept),
            conns,
        }
    }

    /// `http://127.0.0.1:<port>` — the catalog's `jev_base_url`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Queue a 200 answers set. Each asked question name resolves against
    /// `answers`; a name the script misses is a 500 `unscripted_question`
    /// (a scripting bug, reported — never a silently wrong answer). The
    /// set repeats once the queue runs dry.
    pub fn push_answers<S: Into<String>>(&self, answers: impl IntoIterator<Item = (S, Answer)>) {
        let map: BTreeMap<String, Answer> = answers
            .into_iter()
            .map(|(name, answer)| (name.into(), answer))
            .collect();
        self.lock().script.push_back(Step::Answers(map));
    }

    /// Queue a one-shot fault for the next request.
    pub fn push_fault(&self, fault: Fault) {
        self.lock().script.push_back(Step::Fault(fault));
    }

    /// The decoded requests so far — faulted asks included.
    #[must_use]
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.lock().requests.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().expect("fake jev state poisoned")
    }
}

impl Drop for FakeJev {
    fn drop(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
        self.conns.lock().expect("conns poisoned").abort_all();
    }
}

fn guard(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().expect("fake jev state poisoned")
}

/// One accepted connection: read the head + bounded body, capture the
/// decoded request, then answer or fault per the script. A malformed or
/// oversized request closes quietly — the client's own request is always
/// well-formed, and a transport error is the honest report.
async fn handle_conn(stream: TcpStream, shared: &Mutex<Shared>) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let Some(body) = read_request(&mut reader).await else {
        return;
    };
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return;
    };
    let step = {
        let mut s = guard(shared);
        s.requests.push(CapturedRequest {
            body: request.clone(),
        });
        match s.script.pop_front() {
            Some(step) => {
                if let Step::Answers(map) = &step {
                    s.sticky = Some(map.clone());
                }
                Some(step)
            }
            None => s.sticky.clone().map(Step::Answers),
        }
    };
    match step {
        Some(Step::Answers(answers)) => serve_answers(&mut writer, &request, &answers).await,
        Some(Step::Fault(Fault::Silent)) => {
            // Hold the connection open — `Drop`'s `abort_all` ends it.
            std::future::pending::<()>().await;
        }
        Some(Step::Fault(Fault::Abort)) => {
            writer.shutdown().await.ok();
        }
        Some(Step::Fault(Fault::Status {
            status,
            error_type,
            retry_after_ms,
        })) => {
            serve_status(&mut writer, status, error_type.as_deref(), retry_after_ms).await;
        }
        None => serve_status(&mut writer, 500, Some("unscripted_request"), None).await,
    }
}

/// Read one HTTP/1.1 request head + `content-length` body. `None` on any
/// deviation — the fake serves only its own client's shape.
async fn read_request(reader: &mut BufReader<OwnedReadHalf>) -> Option<Vec<u8>> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 || head.len().saturating_add(line.len()) > HEAD_MAX_BYTES {
            return None;
        }
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_owned)
        })
        .and_then(|value| value.trim().parse().ok())?;
    if length > BODY_MAX_BYTES {
        return None;
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await.ok()?;
    Some(body)
}

/// Resolve each asked question name against `answers` and serve the 200.
/// The response `model` echoes the request's — the resolved revision is
/// opaque to the client.
async fn serve_answers(
    writer: &mut OwnedWriteHalf,
    request: &Value,
    answers: &BTreeMap<String, Answer>,
) {
    let Some(asked) = request.get("questions").and_then(Value::as_object) else {
        return serve_status(writer, 500, Some("unscripted_request"), None).await;
    };
    let mut body = serde_json::Map::new();
    for (name, spec) in asked {
        let asked_type = spec.get("type").and_then(Value::as_str);
        let Some(answer) = answers.get(name) else {
            return serve_status(writer, 500, Some("unscripted_question"), None).await;
        };
        let wire = match (answer, asked_type) {
            (Answer::Noul(p), Some("noul")) => json!({"type": "noul", "noul": p}),
            (
                Answer::Choice {
                    choice,
                    probabilities,
                },
                Some("choice"),
            ) => json!({"type": "choice", "choice": choice, "probabilities": probabilities}),
            (Answer::Noul(_) | Answer::Choice { .. }, _) => {
                return serve_status(writer, 500, Some("answer_type_mismatch"), None).await;
            }
        };
        body.insert(name.clone(), wire);
    }
    let reply = json!({
        "model": request.get("model").cloned().unwrap_or_else(|| json!("jev-fake")),
        "answers": Value::Object(body),
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
    .to_string();
    write_response(writer, 200, "OK", &[], reply.as_bytes()).await;
}

/// A non-2xx reply — the typed `{"detail":{"error_type":…}}` body when
/// `error_type` is given, plus the optional `retry-after-ms` header.
async fn serve_status(
    writer: &mut OwnedWriteHalf,
    status: u16,
    error_type: Option<&str>,
    retry_after_ms: Option<u64>,
) {
    let body = match error_type {
        Some(kind) => json!({"detail": {"error_type": kind, "message": kind}}).to_string(),
        None => String::new(),
    };
    let mut headers = vec![("content-type", "application/json".to_owned())];
    if let Some(ms) = retry_after_ms {
        headers.push(("retry-after-ms", ms.to_string()));
    }
    write_response(writer, status, "Error", &headers, body.as_bytes()).await;
}

/// `HTTP/1.1 <status>\r\n<headers>\r\n<body>`, `connection: close`.
async fn write_response(
    writer: &mut OwnedWriteHalf,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        write!(response, "{name}: {value}\r\n").expect("write to String");
    }
    write!(
        response,
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .expect("write to String");
    writer.write_all(response.as_bytes()).await.ok();
    writer.write_all(body).await.ok();
    writer.shutdown().await.ok();
}
