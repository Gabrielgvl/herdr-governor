//! `tests` — the daemon's in-process contract: `run`'s bring-up, serve and
//! teardown over real files; the bounded apply-retry; the §4.3 step-5
//! restart marking (including the [r2] settled-Run case); the §4.7
//! reconcile gates (the identity-less absence rule, the sessionless
//! foreign-incarnation settlement, `HerdrHealth`); the §4.5 `decided`
//! re-mint on `Conflict{Run}`; the `Msg::Tool` arm's
//! F1 resolve + bind + `herdr_status` page; `check_config`'s exit codes;
//! and the lexical tripwire that keeps `tracing::` call sites to ids,
//! sizes and digests (§4.18).

mod coordinator;
mod effects;
mod launch;
mod process;
mod reconcile;
mod tool;

/// A `RunnerEnv`'s Jev fields as real values: a client on an unroutable
/// base and a key read from a genuine `0600` file, never a literal.
async fn jev_env(
    dir: &std::path::Path,
) -> (crate::adapters::jev::Client, crate::adapters::jev::ApiKey) {
    let credentials = dir.join("credentials");
    std::fs::write(&credentials, "test-token").expect("credentials");
    std::fs::set_permissions(
        &credentials,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .expect("0600");
    let client = crate::adapters::jev::Client::new("http://127.0.0.1:9").expect("jev client");
    let key = crate::adapters::jev::ApiKey::read_0600(&credentials)
        .await
        .expect("test credential reads");
    (client, key)
}
