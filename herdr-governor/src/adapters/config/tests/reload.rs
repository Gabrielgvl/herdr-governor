//! `reload` — adopt-vs-retain-last-good: the daemon keeps serving the
//! last-good catalog when the new file fails at any stage, and a
//! byte-identical file takes the digest-equality cheap path.

use std::time::Duration;

use governor_core::config::ConfigError;

use super::{GOLDEN, write_catalog};
use crate::adapters::config::{ConfigLoadError, LoadedConfig, ReloadOutcome, load, reload};

#[tokio::test]
async fn reload_adopts_valid_new_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let current = load(&path).await.expect("load");
    std::fs::write(
        &path,
        GOLDEN.replace("cooldown_secs = 3600", "cooldown_secs = 120"),
    )
    .expect("rewrite");
    match reload(&current, &path).await {
        ReloadOutcome::Adopted(loaded) => {
            assert_ne!(
                loaded.version, current.version,
                "new bytes bump the version"
            );
            assert_eq!(
                loaded.config.policy.cooldown,
                Duration::from_secs(120),
                "the new policy value is live"
            );
        }
        ReloadOutcome::Retained { last_good, error } => {
            panic!("valid new config must adopt (kept {last_good:?}): {error}");
        }
    }
}

#[tokio::test]
async fn reload_retains_last_good_on_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let current = load(&path).await.expect("load");
    std::fs::write(
        &path,
        GOLDEN.replace(
            "tiers = [\"fast\", \"standard\", \"frontier\"]",
            "tiers = []",
        ),
    )
    .expect("rewrite");
    match reload(&current, &path).await {
        ReloadOutcome::Retained { last_good, error } => {
            assert!(
                std::ptr::eq(last_good, &raw const current),
                "Retained carries the same last-good config the caller holds"
            );
            let ConfigLoadError::Invalid(errors) = &error else {
                panic!("an F27-invalid file retains with Invalid, got {error:?}");
            };
            assert_eq!(
                errors.len(),
                6,
                "empty tiers cascades: Missing + an UnknownTier per reference: {errors:?}"
            );
            assert_eq!(
                errors.first(),
                Some(&ConfigError::Missing {
                    field: "policy.tiers".into()
                }),
                "the primary error reports Missing{{policy.tiers}} first"
            );
        }
        ReloadOutcome::Adopted(loaded) => {
            panic!("an invalid file must never adopt, got {loaded:?}");
        }
    }
}

#[tokio::test]
async fn reload_retains_on_decode_or_read_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let current = load(&path).await.expect("load");
    std::fs::write(&path, "= broken").expect("rewrite");
    match reload(&current, &path).await {
        ReloadOutcome::Retained { error, .. } => assert!(
            matches!(error, ConfigLoadError::Decode(_)),
            "malformed bytes retain with Decode: {error:?}"
        ),
        ReloadOutcome::Adopted(_) => panic!("malformed bytes must retain last-good"),
    }
    std::fs::remove_file(&path).expect("remove");
    match reload(&current, &path).await {
        ReloadOutcome::Retained { error, .. } => assert!(
            matches!(error, ConfigLoadError::Read(_)),
            "a vanished file retains with Read: {error:?}"
        ),
        ReloadOutcome::Adopted(_) => panic!("an unreadable file must retain last-good"),
    }
}

#[tokio::test]
async fn reload_identical_bytes_is_noop_adopt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let current: LoadedConfig = load(&path).await.expect("load");
    match reload(&current, &path).await {
        ReloadOutcome::Adopted(loaded) => {
            assert_eq!(
                loaded.version, current.version,
                "identical bytes keep the version — digest-equality took the cheap path"
            );
            assert_eq!(
                loaded.config, current.config,
                "identical bytes keep the config"
            );
        }
        ReloadOutcome::Retained { error, .. } => {
            panic!("identical bytes cannot fail: {error}");
        }
    }
}
