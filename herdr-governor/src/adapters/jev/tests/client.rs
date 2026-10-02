//! The fixture-free client legs: the credential seam (missing, wrong-mode,
//! empty or header-unsafe key files are refused before any request exists),
//! the redacted `ApiKey` debug form, and the error → `JudgmentOutcome` map.
//! Every fixture-reading fake-server test lives in the `tests/jev_fixtures/`
//! integration crate now.

use std::os::unix::fs::PermissionsExt as _;

use governor_core::routing::JudgmentOutcome;

use super::super::client::{ApiKey, Client, vouch};
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

/// A symlinked credential is refused at open: `O_NOFOLLOW` turns the
/// final link component into `ELOOP` even when the target is itself a
/// perfectly vouched 0600 file, so the refusal is about the path, never
/// about what the target would have been.
#[tokio::test]
async fn symlinked_credential_is_refused() {
    let (dir, real) = credential_file(FAKE_KEY, 0o600);
    let link = dir.path().join("linked");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let err = ApiKey::read_0600(&link).await.expect_err("symlink");
    assert!(
        matches!(err, JevError::CredentialUnavailable { reason: "symlink" }),
        "{err}"
    );
}

/// A credential owned by a different uid is refused on the same open
/// handle that vouches the mode. Building one needs privilege (a
/// `chown`), so the leg skips with a reason when the test runs
/// unprivileged.
#[tokio::test]
async fn foreign_owned_credential_is_refused() {
    let (_dir, path) = credential_file(FAKE_KEY, 0o600);
    let euid = rustix::process::geteuid().as_raw();
    let foreign = if euid == 0 { 65534 } else { 0 };
    if rustix::fs::chown(&path, Some(rustix::process::Uid::from_raw(foreign)), None).is_err() {
        eprintln!("foreign-owner leg skipped: cannot chown unprivileged (euid {euid})");
        return;
    }
    let err = ApiKey::read_0600(&path).await.expect_err("foreign owner");
    assert!(
        matches!(
            err,
            JevError::CredentialUnavailable {
                reason: "foreign_owner"
            }
        ),
        "{err}"
    );
}

/// The pure verdict: `0600` owned by the effective uid passes, and every
/// other state names the requirement it failed. File-type bits in
/// `st_mode` are ignored (the `S_IFREG` check is a stated follow-up).
#[test]
fn vouch_requires_mode_and_owner() {
    assert_eq!(vouch(0o100_600, 1000, 1000), Ok(()));
    assert_eq!(vouch(0o040_600, 1000, 1000), Ok(()));
    for mode in [0o100_644, 0o100_640, 0o100_700] {
        assert_eq!(vouch(mode, 1000, 1000), Err("mode_not_0600"), "{mode:o}");
    }
    assert_eq!(vouch(0o100_600, 0, 1000), Err("foreign_owner"));
    assert_eq!(vouch(0o100_600, 1000, 0), Err("foreign_owner"));
    assert_eq!(vouch(0o100_644, 0, 1000), Err("mode_not_0600"));
}

/// The mode verdict and the bytes come from the opened handle, not from
/// the path: once a file is open, swapping what the path points at changes
/// nothing. A handle on a 0644 file is refused even after a 0600 file is
/// swapped in at its path; a handle on a 0600 file is accepted even after
/// a 0644 file replaces it there. The old metadata-then-read shape passed
/// the first case's swapped-in bytes.
#[tokio::test]
async fn credential_verdict_comes_from_the_open_handle() {
    let (dir, loose) = credential_file(FAKE_KEY, 0o644);
    let strict = dir.path().join("strict");
    std::fs::write(&strict, FAKE_KEY).expect("write");
    std::fs::set_permissions(&strict, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let opened_loose = tokio::fs::File::open(&loose).await.expect("open");
    std::fs::rename(&strict, &loose).expect("swap a 0600 file in at the path");
    let err = ApiKey::from_file(opened_loose)
        .await
        .expect_err("0644 handle");
    assert!(
        matches!(
            err,
            JevError::CredentialUnavailable {
                reason: "mode_not_0600"
            }
        ),
        "{err}"
    );

    let opened_strict = tokio::fs::File::open(&loose)
        .await
        .expect("open the swapped-in 0600");
    let other = credential_file(FAKE_KEY, 0o644);
    std::fs::rename(&other.1, &loose).expect("swap a 0644 file in at the path");
    ApiKey::from_file(opened_strict)
        .await
        .expect("the 0600 handle is unaffected by the path swap");
}

/// A credential file past the read bound is refused before it is buffered
/// whole, and the bound is read from the same handle as the verdict.
#[tokio::test]
async fn oversized_credential_file_is_refused() {
    let (_dir, path) = credential_file(&"k".repeat(8 * 1024 + 1), 0o600);
    let err = ApiKey::read_0600(&path).await.expect_err("too large");
    assert!(
        matches!(
            err,
            JevError::CredentialUnavailable {
                reason: "not_header_safe"
            }
        ),
        "{err}"
    );
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
