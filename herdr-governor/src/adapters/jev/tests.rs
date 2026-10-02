//! Unit tests for the Jev client: the fixture-free unit surface stays
//! here — pure wire rules in `tests/wire.rs`, the credential seam and the
//! outcome map in `tests/client.rs`. Every test that reads a committed
//! contract fixture moved to the `tests/jev_fixtures/` integration crate,
//! which reads them at run time: a member's `src/` must compile against
//! its own tree alone (the guard-selftest mirrors `src/` plus the
//! lockfile). The fake-server harness used only by the moved tests moved
//! with them; the credential-file helpers below still serve the seam
//! tests.

mod client;
mod wire;

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use super::client::ApiKey;

pub(super) const FAKE_KEY: &str = "tsk_fake_0123456789abcdef";

/// A fake credential file with the given mode. The `TempDir` must outlive
/// the test.
pub(super) fn credential_file(contents: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("credentials");
    std::fs::write(&path, contents).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    (dir, path)
}

pub(super) async fn fake_key() -> (tempfile::TempDir, ApiKey) {
    let (dir, path) = credential_file(&format!("{FAKE_KEY}\n"), 0o600);
    let key = ApiKey::read_0600(&path).await.expect("fake key");
    (dir, key)
}
