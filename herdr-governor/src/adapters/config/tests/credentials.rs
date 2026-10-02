//! `load_credentials` — the 0600 reader, delegated to J1's
//! `ApiKey::read_0600`: typed refusals, and the CT-JEV-AUTH-1 rule that a
//! `!cmd`/`$ENV` line is stored verbatim, never executed.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::adapters::config::load_credentials;
use crate::adapters::jev::JevError;

/// Write `contents` to a 0600 file inside `dir` and return its path.
fn write_credential(dir: &Path, name: &str, contents: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("fixture write succeeds");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

#[tokio::test]
async fn credentials_failures_are_typed_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");

    let missing = load_credentials(&dir.path().join("credentials"))
        .await
        .expect_err("a missing credential must fail");
    assert!(
        matches!(
            missing,
            JevError::CredentialUnavailable { reason: "missing" }
        ),
        "missing file is typed 'missing': {missing:?}"
    );

    let loose = write_credential(dir.path(), "loose", "test-token\n", 0o640);
    let err = load_credentials(&loose)
        .await
        .expect_err("0640 must be refused");
    assert!(
        matches!(
            err,
            JevError::CredentialUnavailable {
                reason: "mode_not_0600"
            }
        ),
        "a loose mode is refused: {err:?}"
    );

    let empty = write_credential(dir.path(), "empty", "  \n", 0o600);
    let empty_err = load_credentials(&empty)
        .await
        .expect_err("an empty credential must fail");
    assert!(
        matches!(
            empty_err,
            JevError::CredentialUnavailable { reason: "empty" }
        ),
        "a whitespace-only file is typed 'empty': {empty_err:?}"
    );
}

#[tokio::test]
async fn credentials_pass_through_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in [
        ("cmd-indirected", "!cmd printenv SECRET\n"),
        ("env-indirected", "$GOVERNOR_KEY\n"),
    ] {
        let path = write_credential(dir.path(), name, contents, 0o600);
        match load_credentials(&path).await {
            Ok(_key) => {}
            Err(error) => panic!(
                "{name}: an indirection is stored verbatim, never executed (CT-JEV-AUTH-1): {error}"
            ),
        }
    }
}
