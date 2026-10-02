//! `client` — the async Jev call (OQ-9: tokio + `reqwest`): the
//! credential as a redacted value, the no-socket-write size gate, one
//! `POST {base}/v1/systemone` with the caller's timeout and no retry, the
//! bounded body read, and the status → `JevError` map. Base URL, model
//! and timeout are caller parameters (catalog data), never literals here.

use std::fmt;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use governor_core::routing::{Judgment, Question, request_size_outcome};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::redirect;
use tokio::fs::File;
use tokio::io::AsyncReadExt as _;

use super::error::{JevError, http_component};
use super::wire::{
    self, JEV_RESPONSE_MAX_BYTES, Kind, Request, State, WireQuestion, question_name,
};

/// The credential-file read bound: a bearer token is a few hundred bytes;
/// anything past this is not a key file and is never buffered further.
const KEY_MAX_BYTES: u64 = 8 * 1024;

/// The Jev bearer credential. Never printed, logged, serialized or
/// compared: `Debug` is redacted and there is no `Display`. Built only
/// from the governor's own 0600 credential file (spec §19).
#[derive(Clone)]
pub struct ApiKey(HeaderValue);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl ApiKey {
    /// Read the credential file: it must exist, be mode `0600` exactly,
    /// and hold one non-empty header-safe token (surrounding whitespace
    /// and a trailing newline are trimmed). Any other state is
    /// `CredentialUnavailable` — no request is ever sent with a key the
    /// adapter could not vouch for.
    pub async fn read_0600(path: &Path) -> Result<Self, JevError> {
        let file = File::open(path)
            .await
            .map_err(|e| JevError::CredentialUnavailable {
                reason: if e.kind() == std::io::ErrorKind::NotFound {
                    "missing"
                } else {
                    "unreadable"
                },
            })?;
        Self::from_file(file).await
    }

    /// The verdict and the bytes come from one open handle: the mode is
    /// `fstat`ed on `file` and the token read from the same descriptor,
    /// so no path swap between the two can substitute a looser file.
    pub(super) async fn from_file(mut file: File) -> Result<Self, JevError> {
        let meta = file
            .metadata()
            .await
            .map_err(|_io| JevError::CredentialUnavailable {
                reason: "unreadable",
            })?;
        if meta.permissions().mode() & 0o777 != 0o600 {
            return Err(JevError::CredentialUnavailable {
                reason: "mode_not_0600",
            });
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(KEY_MAX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|_io| JevError::CredentialUnavailable {
                reason: "unreadable",
            })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > KEY_MAX_BYTES {
            return Err(JevError::CredentialUnavailable {
                reason: "not_header_safe",
            });
        }
        let token = std::str::from_utf8(&bytes)
            .map_err(|_utf8| JevError::CredentialUnavailable {
                reason: "not_header_safe",
            })?
            .trim();
        if token.is_empty() {
            return Err(JevError::CredentialUnavailable { reason: "empty" });
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_value| {
            JevError::CredentialUnavailable {
                reason: "not_header_safe",
            }
        })?;
        value.set_sensitive(true);
        Ok(Self(value))
    }
}

/// One question to ask: the core `Question` plus the wire text the
/// question catalog supplies.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionSpec {
    /// The question answered (its wire name is `question_name`).
    pub question: Question,
    /// Noul (with an optional policy threshold) or choice.
    pub kind: Kind,
    /// The `instructions` text.
    pub instructions: String,
    /// The ordered `criteria` entries.
    pub criteria: Vec<(String, String)>,
}

/// Everything one Jev call needs beyond the credential — all caller
/// data, none of it literals in this module.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeParams {
    /// The requested model name (`jev-latest` in the catalog).
    pub model: String,
    /// The semantic state judged (never operating points — by type).
    pub state: State,
    /// The questions, in asked order.
    pub questions: Vec<QuestionSpec>,
    /// The per-request deadline (`sdk_default_timeout_ms` is the
    /// catalog's business).
    pub timeout: Duration,
}

/// An answered call: the resolved model and one `Judgment` per asked
/// question. The caller stamps these into its `JudgmentSet` with outcome
/// `Answered`; a failed call is a `JevError`, whose `outcome()` is the
/// set's outcome instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    /// `model` from the response — the resolved revision, opaque.
    pub model: String,
    /// The answers in asked order.
    pub judgments: Vec<Judgment>,
}

/// The Jev client: one `reqwest::Client` (connection pool) plus the
/// endpoint base. Redirects are disabled — a 3xx is an HTTP failure, so
/// the bearer header can never follow a redirect off the configured host.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
}

impl Client {
    /// `base` is the scheme+host(+port) the catalog names; the path is
    /// the fixed `/v1/systemone`.
    pub fn new(base: &str) -> Result<Self, JevError> {
        install_crypto_provider();
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .build()
            .map_err(JevError::ClientBuild)?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/systemone", base.trim_end_matches('/')),
        })
    }

    /// One judgment call, no retry. Order is fixed: encode → size gate
    /// (nothing written past it) → send with `timeout` → bounded read →
    /// decode. Every failure is a typed `JevError`.
    pub async fn judge(&self, key: &ApiKey, params: &JudgeParams) -> Result<Judged, JevError> {
        let asked: Vec<WireQuestion> = params
            .questions
            .iter()
            .map(|q| WireQuestion {
                name: question_name(&q.question),
                kind: q.kind,
                instructions: q.instructions.clone(),
                criteria: q.criteria.clone(),
            })
            .collect();
        let body = wire::encode(&Request {
            state: &params.state,
            questions: wire::Questions(&asked),
            model: &params.model,
        })
        .map_err(|_encode| JevError::InvalidResponse {
            detail: "request_encode",
        })?;
        if request_size_outcome(body.len()).is_some() {
            return Err(JevError::TooLarge { bytes: body.len() });
        }
        let response = self
            .http
            .post(&self.endpoint)
            .header(AUTHORIZATION, key.0.clone())
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .timeout(params.timeout)
            .body(body)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        let retry_after_ms = retry_after_ms(response.headers());
        let bytes = read_bounded(response).await?;
        if !status.is_success() {
            return Err(JevError::Http {
                status: status.as_u16(),
                component: http_component(status.as_u16(), wire::error_type(&bytes).as_deref()),
                retry_after_ms,
            });
        }
        let decoded = wire::decode(&bytes, &asked)?;
        let judgments = params
            .questions
            .iter()
            .zip(decoded.answers)
            .map(|(spec, answer)| Judgment {
                question: spec.question,
                probabilities: answer.probabilities,
                answer: answer.answer,
                threshold: match spec.kind {
                    Kind::Noul { threshold } => threshold,
                    Kind::Choice => None,
                },
            })
            .collect();
        Ok(Judged {
            model: decoded.model,
            judgments,
        })
    }
}

/// The `no-provider` rustls feature (D0) leaves the process-level crypto
/// provider to the binary: install aws-lc-rs once. A concurrent first
/// install reports the already-set provider — the same one — so the
/// result is deliberately ignored.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _already_installed = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

fn transport(error: reqwest::Error) -> JevError {
    if error.is_timeout() {
        JevError::Timeout
    } else {
        JevError::Transport(error)
    }
}

/// `retry-after-ms` (milliseconds) else `retry-after` (delay-seconds),
/// recorded on a 429 for the journal; never acted on.
fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    number("retry-after-ms").or_else(|| number("retry-after").and_then(|s| s.checked_mul(1000)))
}

/// Read the body up to `JEV_RESPONSE_MAX_BYTES`; one byte over is an
/// invalid response and the connection is dropped unread.
async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, JevError> {
    let over = JevError::InvalidResponse {
        detail: "response_too_large",
    };
    if response
        .content_length()
        .is_some_and(|len| u64::try_from(JEV_RESPONSE_MAX_BYTES).is_ok_and(|max| len > max))
    {
        return Err(over);
    }
    let mut buf = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if buf.len().saturating_add(chunk.len()) > JEV_RESPONSE_MAX_BYTES {
            return Err(over);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}
