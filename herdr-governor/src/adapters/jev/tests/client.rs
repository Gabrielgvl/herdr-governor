//! The fixture-free client legs: the credential seam (missing, wrong-mode,
//! empty or header-unsafe key files are refused before any request exists),
//! the redacted `ApiKey` debug form, and the error → `JudgmentOutcome` map.
//! Every fixture-reading fake-server test lives in the `tests/jev_fixtures/`
//! integration crate now.

use governor_core::routing::JudgmentOutcome;

use super::super::client::{ApiKey, Client};
use super::super::error::JevError;
use super::{FAKE_KEY, credential_file, fake_key};

/// `unresolvable_key_abstain`: missing, wrong-mode, empty or non-header
/// credential files are `CredentialUnavailable` → `AuthFailed`/`api_key`
/// before any request exists.
#[tokio::test]
async fn error_unresolvable_key() {
    let (dir, path) = credential_file("", 0o600);
    let cases: [(&str, &str, u32); 4] = [
        ("missing", "", 0o600),
        ("mode_not_0600", FAKE_KEY, 0o644),
        ("empty", "\n", 0o600),
        ("not_header_safe", "bad\u{1}key", 0o600),
    ];
    for (reason, contents, mode) in cases {
        let (_d, p) = credential_file(contents, mode);
        let target = if reason == "missing" {
            dir.path().join("absent")
        } else {
            p
        };
        let err = ApiKey::read_0600(&target).await.expect_err(reason);
        assert!(
            matches!(err, JevError::CredentialUnavailable { reason: r } if r == reason),
            "{reason}: {err}"
        );
        assert_eq!(err.outcome(), JudgmentOutcome::AuthFailed);
        assert_eq!(err.component(), "api_key");
    }
    assert!(ApiKey::read_0600(&path).await.is_err(), "empty file");
}

#[tokio::test]
async fn api_key_debug_is_redacted() {
    let (_dir, key) = fake_key().await;
    let shown = format!("{key:?}");
    assert_eq!(shown, "ApiKey(<redacted>)");
    assert!(!shown.contains(FAKE_KEY));
}

#[tokio::test]
async fn every_error_class_maps() {
    let table: Vec<(JevError, JudgmentOutcome)> = vec![
        (
            JevError::CredentialUnavailable { reason: "missing" },
            JudgmentOutcome::AuthFailed,
        ),
        (JevError::TooLarge { bytes: 1 }, JudgmentOutcome::TooLarge),
        (JevError::Timeout, JudgmentOutcome::TransportFailed),
        (
            JevError::InvalidResponse { detail: "x" },
            JudgmentOutcome::InvalidResponse,
        ),
    ];
    for (err, outcome) in table {
        assert_eq!(err.outcome(), outcome, "{err}");
    }
    for (status, outcome) in [
        (400, JudgmentOutcome::TransportFailed),
        (401, JudgmentOutcome::AuthFailed),
        (403, JudgmentOutcome::AuthFailed),
        (404, JudgmentOutcome::TransportFailed),
        (422, JudgmentOutcome::TransportFailed),
        (429, JudgmentOutcome::TransportFailed),
        (500, JudgmentOutcome::TransportFailed),
    ] {
        let err = JevError::Http {
            status,
            component: String::new(),
            retry_after_ms: None,
        };
        assert_eq!(err.outcome(), outcome, "{status}");
        assert_ne!(err.outcome(), JudgmentOutcome::Answered);
        assert_ne!(err.outcome(), JudgmentOutcome::Stale);
    }
    Client::new("http://127.0.0.1:1/").expect("client builds");
}
