//! `wire` — Jev's `POST /v1/systemone` body as the fixtures pin it
//! (`jev-launch-evaluation.json`, `jev-supervision-review.json`,
//! `jev-raw-response.json`): the request `{state, questions, model}` in
//! that key order with questions and criteria in asked order, and the
//! bounded response decode `{model, answers{name → answer}, usage}` into
//! per-question distributions. Pure — no I/O, no clock, no credential.

use std::collections::BTreeMap;

use governor_core::routing::{Probability, Question};
use governor_core::task::Task;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, Serializer};

use super::error::JevError;

/// The governor-side bound on a Jev response body. The largest recorded
/// answer set is well under 2 KiB; a body past this is not a response we
/// trust in memory, so it is `InvalidResponse`, never read further.
pub const JEV_RESPONSE_MAX_BYTES: usize = 256 * 1024;

/// The verdict bound a noul resolves against when no policy threshold
/// applies — the same 0.5 the core's `noul_yes` uses (it is `pub(crate)`
/// there; the core re-checks the verdict in `validate_evaluation`).
pub const NOUL_VERDICT_BOUND: f64 = 0.5;

/// The semantic state Jev judges. By construction there is no field for an
/// operating point, provider, tier request or label (F12, H#41): a
/// `State` cannot carry them, so the request cannot either.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// F12 launch evaluation — `{"task": {objective, scope, doneWhen,
    /// constraints}}`.
    Task(TaskState),
}

/// The four semantic Task fields and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskState {
    /// `task.objective`.
    pub objective: String,
    /// `task.scope`.
    pub scope: String,
    /// `task.doneWhen`.
    #[serde(rename = "doneWhen")]
    pub done_when: Vec<String>,
    /// `task.constraints`.
    pub constraints: Vec<String>,
}

impl From<&Task> for TaskState {
    /// Projects the Task: `tier`, `recovery_of`, `label` and `cwd` never
    /// reach Jev (F12, H#41).
    fn from(task: &Task) -> Self {
        Self {
            objective: task.objective.clone(),
            scope: task.scope.clone(),
            done_when: task.done_when.clone(),
            constraints: task.constraints.clone(),
        }
    }
}

/// The answer shape a question asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// `{type:"noul", noul: p}` — P(yes); the verdict is `yes` at or above
    /// `threshold` (the policy threshold where one applies, F21) or the
    /// verdict bound.
    Noul {
        /// The policy threshold stamped onto the judgment, if any.
        threshold: Option<f64>,
    },
    /// `{type:"choice", choice, probabilities, confidence}` — a label
    /// distribution; `confidence` is dropped (a concentration measure,
    /// not part of the distribution).
    Choice,
}

impl Kind {
    fn wire_type(self) -> &'static str {
        match self {
            Self::Noul { threshold: _ } => "noul",
            Self::Choice => "choice",
        }
    }
}

/// One question as it goes on the wire: the name key, its `type`, the
/// `instructions` text and the ordered `criteria` map.
#[derive(Debug, Clone, PartialEq)]
pub struct WireQuestion {
    /// The `questions` key (`Question` spelling via [`question_name`]).
    pub name: String,
    /// The answer shape.
    pub kind: Kind,
    /// The `instructions` text, verbatim.
    pub instructions: String,
    /// The `criteria` entries in the order they are sent.
    pub criteria: Vec<(String, String)>,
}

/// The stored/wire spelling of a core question: `as_str()`, with
/// `handoff_meets_item_k` rendered as `handoff_meets_item_<item>` (F24).
#[must_use]
pub fn question_name(question: &Question) -> String {
    match question {
        Question::HandoffMeetsItem { item } => format!("{}_{item}", question.as_str()),
        Question::DoneWhenVerifiable
        | Question::WeakestSufficientTier
        | Question::ChangesFiles
        | Question::SecurityBoundary
        | Question::NeedsExternal
        | Question::LongRunning
        | Question::RelatedTab
        | Question::BlockedOnInput
        | Question::NoRecentProgress
        | Question::OutsideScope
        | Question::ProviderLimited => question.as_str().to_owned(),
    }
}

/// The request body. Field order is the wire order the fixtures record:
/// `state`, `questions`, `model`.
#[derive(Debug, Serialize)]
pub struct Request<'a> {
    /// The semantic state judged.
    pub state: &'a State,
    /// The questions, in asked order.
    pub questions: Questions<'a>,
    /// The requested model name (`jev-latest`; catalog data, never a
    /// literal here).
    pub model: &'a str,
}

/// The ordered `questions` map.
#[derive(Debug)]
pub struct Questions<'a>(pub &'a [WireQuestion]);

impl Serialize for Questions<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for question in self.0 {
            map.serialize_entry(
                &question.name,
                &QuestionBody {
                    kind: question.kind.wire_type(),
                    instructions: &question.instructions,
                    criteria: Criteria(&question.criteria),
                },
            )?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct QuestionBody<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: &'a str,
    criteria: Criteria<'a>,
}

struct Criteria<'a>(&'a [(String, String)]);

impl Serialize for Criteria<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (label, text) in self.0 {
            map.serialize_entry(label, text)?;
        }
        map.end()
    }
}

/// Serialize the request body — the only bytes that ever go on the wire.
pub fn encode(request: &Request<'_>) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(request)
}

/// One decoded answer: the distribution and the resolved value.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Noul: `{"yes": p}`; choice: the recorded distribution verbatim.
    pub probabilities: BTreeMap<String, Probability>,
    /// Noul: `yes`/`no` per the verdict; choice: the chosen label.
    pub answer: String,
}

/// A decoded 2xx body: the resolved model and the answers aligned with
/// the asked questions. `usage` is dropped (bookkeeping, no column).
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// `model` — the resolved revision, opaque.
    pub model: String,
    /// One answer per asked question, in asked order.
    pub answers: Vec<Answer>,
}

#[derive(Deserialize)]
struct RawResponse {
    model: String,
    answers: BTreeMap<String, RawAnswer>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
    },
}

/// Decode a 2xx body against the asked questions: the answer set must be
/// exactly the asked set (no partial, no extra), every answer must carry
/// the asked `type`, and every probability must be finite in `[0, 1]`. No
/// sum-to-one rule (the recorded `reason` distribution sums to 0.99).
pub fn decode(body: &[u8], asked: &[WireQuestion]) -> Result<Decoded, JevError> {
    let raw: RawResponse =
        serde_json::from_slice(body).map_err(|_shape| JevError::InvalidResponse {
            detail: "body_shape",
        })?;
    if raw.answers.len() != asked.len() {
        return Err(JevError::InvalidResponse {
            detail: "answer_set_mismatch",
        });
    }
    let mut answers = Vec::with_capacity(asked.len());
    for question in asked {
        let answer = raw
            .answers
            .get(&question.name)
            .ok_or(JevError::InvalidResponse {
                detail: "answer_set_mismatch",
            })?;
        answers.push(decode_answer(question.kind, answer)?);
    }
    Ok(Decoded {
        model: raw.model,
        answers,
    })
}

fn decode_answer(kind: Kind, raw: &RawAnswer) -> Result<Answer, JevError> {
    match (kind, raw) {
        (Kind::Noul { threshold }, RawAnswer::Noul { noul }) => {
            let p = probability(*noul)?;
            let verdict = if p.0 >= threshold.unwrap_or(NOUL_VERDICT_BOUND) {
                "yes"
            } else {
                "no"
            };
            Ok(Answer {
                probabilities: BTreeMap::from([("yes".to_owned(), p)]),
                answer: verdict.to_owned(),
            })
        }
        (
            Kind::Choice,
            RawAnswer::Choice {
                choice,
                probabilities,
            },
        ) => {
            if !probabilities.contains_key(choice) {
                return Err(JevError::InvalidResponse {
                    detail: "choice_not_in_distribution",
                });
            }
            let distribution = probabilities
                .iter()
                .map(|(label, p)| Ok((label.clone(), probability(*p)?)))
                .collect::<Result<BTreeMap<_, _>, JevError>>()?;
            Ok(Answer {
                probabilities: distribution,
                answer: choice.clone(),
            })
        }
        (Kind::Noul { threshold: _ }, RawAnswer::Choice { .. })
        | (Kind::Choice, RawAnswer::Noul { noul: _ }) => Err(JevError::InvalidResponse {
            detail: "answer_type_mismatch",
        }),
    }
}

fn probability(value: f64) -> Result<Probability, JevError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(Probability(value))
    } else {
        Err(JevError::InvalidResponse {
            detail: "probability_out_of_range",
        })
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    detail: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    error_type: Option<String>,
}

/// The `detail.error_type` of an error body, if the body is the typed
/// `{detail:{error_type, message}}` shape; anything else is `None` (the
/// component falls back to `http_<status>`).
#[must_use]
pub fn error_type(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .and_then(|b| b.detail.error_type)
}
