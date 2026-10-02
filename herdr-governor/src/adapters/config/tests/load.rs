//! `load` — the golden decode, the OQ-7 content digest, the read/decode
//! taxonomy, `deny_unknown_fields`, and every `ConfigError` variant a TOML
//! file can reach — each through the file path, never past `tokio::fs`.

use std::io::ErrorKind;
use std::time::Duration;

use governor_core::config::ConfigError;
use tempfile::TempDir;

use super::{GOLDEN, write_catalog};
use crate::adapters::config::{ConfigLoadError, sha256_hex};

/// Load `doc` expecting an `Invalid` outcome; return the error list.
async fn load_invalid(dir: &TempDir, doc: &str) -> Vec<ConfigError> {
    let path = write_catalog(dir.path(), doc);
    match crate::adapters::config::load(&path).await {
        Err(ConfigLoadError::Invalid(errors)) => errors,
        other => panic!("doc must fail validation, got {other:?}"),
    }
}

#[tokio::test]
async fn golden_catalog_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let loaded = crate::adapters::config::load(&path)
        .await
        .expect("the golden catalog must load");
    let config = &loaded.config;
    assert_eq!(
        loaded.version, config.version,
        "LoadedConfig hoists config.version verbatim"
    );
    assert_eq!(loaded.version.0.len(), 64, "version is sha256 hex");
    assert!(
        loaded
            .version
            .0
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "version is lowercase hex: {}",
        loaded.version.0
    );
    let tiers: Vec<&str> = config.policy.tiers.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(
        tiers,
        ["fast", "standard", "frontier"],
        "tier order is catalog order"
    );
    assert_eq!(
        config.policy.cooldown,
        Duration::from_secs(3600),
        "cooldown_secs maps to a Duration"
    );
    assert_eq!(config.policy.max_age, Duration::from_hours(24));
    assert_eq!(config.policy.idle_window, Duration::from_mins(15));
    assert_eq!(
        config.catalog.operating_points.len(),
        2,
        "both points decode"
    );
    let point = &config.catalog.operating_points[0];
    assert_eq!(point.id.0, "forge-pro");
    assert_eq!(point.harness.0, "forge");
    assert_eq!(point.args, ["--model", "pro"]);
    assert_eq!(point.tier.0, "frontier");
    assert_eq!(point.cost_class.0, 3);
    assert_eq!(point.provider.0, "vendor-a");
    let caps: Vec<&str> = point.capabilities.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(caps, ["start", "prompt_ack"], "capabilities map verbatim");
}

#[test]
fn sha256_known_answers() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256 of the empty string — the borrowed provider is real SHA-256"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256('abc') — the FIPS-180 vector"
    );
}

#[tokio::test]
async fn version_is_sha256_of_file_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let loaded = crate::adapters::config::load(&path).await.expect("loads");
    let bytes = std::fs::read(&path).expect("read back the fixture");
    assert_eq!(
        loaded.version.0,
        sha256_hex(&bytes),
        "the version is the sha256 of the exact bytes read (OQ-7)"
    );
}

#[tokio::test]
async fn version_stable_and_differs_on_byte_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_catalog(dir.path(), GOLDEN);
    let first = crate::adapters::config::load(&path)
        .await
        .expect("first load");
    let second = crate::adapters::config::load(&path)
        .await
        .expect("second load");
    assert_eq!(first.version, second.version, "same bytes, same version");

    std::fs::write(&path, GOLDEN.replace("frontier", "edge")).expect("rewrite");
    let changed = crate::adapters::config::load(&path)
        .await
        .expect("the edited catalog still validates");
    assert_ne!(
        first.version, changed.version,
        "a byte change bumps the version"
    );
    assert_eq!(changed.config.policy.tiers[2].0, "edge", "the edit took");
}

#[tokio::test]
async fn unreadable_paths_are_read_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = crate::adapters::config::load(&dir.path().join("absent.toml"))
        .await
        .expect_err("a missing file must fail");
    assert!(
        matches!(missing, ConfigLoadError::Read(ref e) if e.kind() == ErrorKind::NotFound),
        "a missing file is a read failure: {missing:?}"
    );
    let directory = crate::adapters::config::load(dir.path())
        .await
        .expect_err("a directory cannot load");
    assert!(
        matches!(directory, ConfigLoadError::Read(_)),
        "a directory is a read failure: {directory:?}"
    );
}

#[tokio::test]
async fn undecodable_bytes_are_decode_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in [
        ("malformed toml", "this is = not toml\n".as_bytes().to_vec()),
        ("non-utf8 bytes", b"\xff\xfe[policy]".to_vec()),
        (
            "missing [policy] table",
            "[catalog]\noperating_points = []\n".into(),
        ),
    ] {
        let path = dir.path().join("catalog.toml");
        std::fs::write(&path, contents).expect("fixture write");
        let err = crate::adapters::config::load(&path)
            .await
            .expect_err("undecodable bytes must fail");
        assert!(
            matches!(err, ConfigLoadError::Decode(_)),
            "{name} is a decode failure: {err:?}"
        );
    }
}

#[tokio::test]
async fn unknown_field_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, doc) in [
        ("root", GOLDEN.replace("[policy]", "stray = 1\n\n[policy]")),
        (
            "policy",
            GOLDEN.replace(
                "provider_limit_threshold",
                "mystery = 1\nprovider_limit_threshold",
            ),
        ),
        (
            "operating point",
            GOLDEN.replace(
                "provider = \"vendor-a\"",
                "provider = \"vendor-a\"\nbogus = true",
            ),
        ),
    ] {
        let path = write_catalog(dir.path(), &doc);
        match crate::adapters::config::load(&path).await {
            Err(ConfigLoadError::Decode(_)) => {}
            other => panic!("{name}: an unknown field must fail the decode, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn f27_missing_tiers_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN.replace(
        "tiers = [\"fast\", \"standard\", \"frontier\"]",
        "tiers = []",
    );
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(
        errors,
        Vec::from([
            ConfigError::Missing {
                field: "policy.tiers".into(),
            },
            ConfigError::UnknownTier {
                field: "policy.no_change_cap".into(),
                tier: "standard".into(),
            },
            ConfigError::UnknownTier {
                field: "policy.security_floor".into(),
                tier: "frontier".into(),
            },
            ConfigError::UnknownTier {
                field: "policy.broad_change_floor".into(),
                tier: "standard".into(),
            },
            ConfigError::UnknownTier {
                field: "catalog.operating_points[forge-pro].tier".into(),
                tier: "frontier".into(),
            },
            ConfigError::UnknownTier {
                field: "catalog.operating_points[atlas-mini].tier".into(),
                tier: "fast".into(),
            },
        ]),
        "an empty tier list reports Missing AND cascades UnknownTier onto every reference"
    );
}

#[tokio::test]
async fn f27_duplicate_tier_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN.replace(
        "tiers = [\"fast\", \"standard\", \"frontier\"]",
        "tiers = [\"fast\", \"standard\", \"frontier\", \"standard\"]",
    );
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(
        errors,
        Vec::from([ConfigError::Duplicate {
            field: "policy.tiers[3]".into(),
            value: "standard".into(),
        }]),
        "a repeated tier reports Duplicate through the core"
    );
}

#[tokio::test]
async fn f27_unknown_tier_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN.replace("no_change_cap = \"standard\"", "no_change_cap = \"ghost\"");
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(
        errors,
        Vec::from([ConfigError::UnknownTier {
            field: "policy.no_change_cap".into(),
            tier: "ghost".into(),
        }]),
        "an undeclared cap reports UnknownTier through the core"
    );
}

#[tokio::test]
async fn f27_rates_out_of_range_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN
        .replace(
            "provider_limit_threshold = 0.6",
            "provider_limit_threshold = 1.5",
        )
        .replace("exploration_rate = 0.05", "exploration_rate = nan");
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(errors.len(), 2, "both rates fail");
    assert!(
        matches!(
            errors[0],
            ConfigError::RateOutOfRange { ref field, value }
            if field == "policy.provider_limit_threshold" && value.to_bits() == 1.5f64.to_bits()
        ),
        "1.5 reports RateOutOfRange on the threshold: {errors:?}"
    );
    assert!(
        matches!(
            errors[1],
            ConfigError::RateOutOfRange { ref field, value }
            if field == "policy.exploration_rate" && value.is_nan()
        ),
        "TOML's nan literal reports RateOutOfRange — the range check, not decode, owns it: {errors:?}"
    );
}

#[tokio::test]
async fn f27_zero_duration_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN.replace("cooldown_secs = 3600", "cooldown_secs = 0");
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(
        errors,
        Vec::from([ConfigError::ZeroDuration {
            field: "policy.cooldown".into(),
        }]),
        "a zero bound reports ZeroDuration through the core"
    );
}

#[tokio::test]
async fn f27_reports_every_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = GOLDEN
        .replace("exploration_rate = 0.05", "exploration_rate = 2.0")
        .replace("tier = \"frontier\"", "tier = \"ghost\"")
        .replace("provider = \"vendor-a\"", "provider = \"\"");
    let errors = load_invalid(&dir, &doc).await;
    assert_eq!(
        errors,
        Vec::from([
            ConfigError::RateOutOfRange {
                field: "policy.exploration_rate".into(),
                value: 2.0,
            },
            ConfigError::UnknownTier {
                field: "catalog.operating_points[forge-pro].tier".into(),
                tier: "ghost".into(),
            },
            ConfigError::Missing {
                field: "catalog.operating_points[forge-pro].provider".into(),
            },
        ]),
        "one Err carries every problem, in the core's validation order"
    );
}

#[tokio::test]
async fn empty_catalog_is_valid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = format!(
        "{}[catalog]\noperating_points = []\n",
        GOLDEN
            .split("[[catalog.operating_points]]")
            .next()
            .expect("GOLDEN splits"),
    );
    let path = write_catalog(dir.path(), &doc);
    let loaded = crate::adapters::config::load(&path)
        .await
        .expect("an empty catalog is the bring-up state");
    assert!(
        loaded.config.catalog.operating_points.is_empty(),
        "operating_points = [] decodes to the empty catalog F27 calls valid"
    );
}

#[tokio::test]
async fn policy_defaults_apply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n[catalog]\noperating_points = []\n";
    let path = write_catalog(dir.path(), doc);
    let loaded = crate::adapters::config::load(&path)
        .await
        .expect("minimal policy loads");
    let policy = &loaded.config.policy;
    assert_eq!(
        policy.exploration_rate.to_bits(),
        0.05f64.to_bits(),
        "exploration_rate defaults to 5%"
    );
    assert_eq!(policy.recovery_expiry, Duration::from_hours(24));
    assert_eq!(policy.max_age, Duration::from_hours(24));
    assert_eq!(policy.repair_window, Duration::from_mins(15));
    assert_eq!(policy.judgment_window, Duration::from_mins(30));
    assert_eq!(policy.idle_window, Duration::from_mins(15));
    assert_eq!(policy.no_change_cap, None, "absent caps are None");
}
