//! `error` — the typed failure taxonomy of the Jev client and its one map
//! onto governor-core's `JudgmentOutcome`. Every class the probe recorded
//! (`jev-wire-evidence.json` `errors[]` + `code_cited_gates`) has a row
//! here; the component strings follow the recorded
//! `error_component_rule` (`http_<status>_<error_type>`, fallback
//! `http_<status>`, `transport` for non-API failures, `api_key` for an
//! unresolvable credential). No variant ever carries key material.

use std::borrow::Cow;

use governor_core::routing::JudgmentOutcome;
use thiserror::Error;

/// Every way a Jev call can fail, typed. Failure is a value: nothing in the
/// adapter panics or retries (`router_retry_max_retries: 0`).
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum JevError {
    /// The credential could not be resolved — absent, unreadable, wrong
    /// file mode, empty, or not a valid header value. No request is sent
    /// (`unresolvable_key_abstain`: `authentication_unavailable` /
    /// `api_key`).
    #[error("jev credential unavailable: {reason}")]
    CredentialUnavailable {
        /// `missing` | `unreadable` | `mode_not_0600` | `empty` |
        /// `not_header_safe`.
        reason: &'static str,
    },
    /// The HTTP client could not be constructed (TLS backend init).
    #[error("jev client build failed: {0}")]
    ClientBuild(#[source] reqwest::Error),
    /// The serialized request exceeds `JEV_REQUEST_MAX_BYTES` — nothing
    /// was written to any socket (`oversize-client-side`).
    #[error("jev request too large: {bytes} bytes")]
    TooLarge {
        /// The serialized body size.
        bytes: usize,
    },
    /// The caller's per-request timeout elapsed (`timeout-1ms`,
    /// `APITimeoutError`).
    #[error("jev request timed out")]
    Timeout,
    /// Connect, write, read or peer-abort failure with no HTTP status
    /// (`pre-abort`, DNS, refused, reset mid-body). The source never
    /// contains the request body or headers.
    #[error("jev transport failed: {0}")]
    Transport(#[source] reqwest::Error),
    /// A non-2xx status. `component` is the recorded rule applied to the
    /// body's `detail.error_type`; 401/403 are the authentication class,
    /// every other status (400-class detail, 404, 422, 429, 3xx, ≥500) is
    /// transport-failed (F12: HTTP errors abstain, never retry).
    #[error("jev http {status}: {component}")]
    Http {
        /// The HTTP status code.
        status: u16,
        /// `http_<status>_<error_type>` or `http_<status>`.
        component: String,
        /// `retry-after-ms` / `retry-after` from a 429 — recorded only,
        /// never acted on.
        retry_after_ms: Option<u64>,
    },
    /// A 2xx whose body does not satisfy the contract: not JSON, missing
    /// or mistyped `model`/`answers`, an answer set that is not exactly the
    /// asked set, an unknown answer `type`, a type that mismatches the
    /// question, a probability outside `[0, 1]`, a choice outside its own
    /// distribution, or a body over the read bound.
    #[error("jev response invalid: {detail}")]
    InvalidResponse {
        /// Which rule the body broke.
        detail: &'static str,
    },
}

impl JevError {
    /// The Appendix-B `judgment_sets.outcome` this failure journals as.
    /// `Answered` and `Stale` are never produced here: the adapter never
    /// answers through an error, and staleness is the core's verdict (F20).
    #[must_use]
    pub fn outcome(&self) -> JudgmentOutcome {
        match self {
            Self::CredentialUnavailable { reason: _ }
            | Self::Http {
                status: 401 | 403,
                component: _,
                retry_after_ms: _,
            } => JudgmentOutcome::AuthFailed,
            Self::ClientBuild(_)
            | Self::Timeout
            | Self::Transport(_)
            | Self::Http {
                status: _,
                component: _,
                retry_after_ms: _,
            } => JudgmentOutcome::TransportFailed,
            Self::TooLarge { bytes: _ } => JudgmentOutcome::TooLarge,
            Self::InvalidResponse { detail: _ } => JudgmentOutcome::InvalidResponse,
        }
    }

    /// The recorded abstain component string for this failure.
    #[must_use]
    pub fn component(&self) -> Cow<'_, str> {
        match self {
            Self::CredentialUnavailable { reason: _ } => Cow::Borrowed("api_key"),
            Self::ClientBuild(_) | Self::Timeout | Self::Transport(_) => Cow::Borrowed("transport"),
            Self::Http {
                status: _,
                component,
                retry_after_ms: _,
            } => Cow::Borrowed(component),
            Self::TooLarge { bytes: _ } => Cow::Borrowed("request_too_large"),
            Self::InvalidResponse { detail } => Cow::Borrowed(detail),
        }
    }
}

/// The recorded `error_component_rule`: `http_<status>_<error_type>` when
/// `error_type` matches `^[a-z][a-z0-9_]{0,63}$`, else `http_<status>`.
#[must_use]
pub fn http_component(status: u16, error_type: Option<&str>) -> String {
    match error_type {
        Some(kind) if conforming_error_type(kind) => format!("http_{status}_{kind}"),
        Some(_) | None => format!("http_{status}"),
    }
}

fn conforming_error_type(kind: &str) -> bool {
    let mut chars = kind.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && kind.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
