//! `config` — `catalog.toml` → the core `Config` (F27): fail-closed async
//! load, the content-derived `ConfigVersion` (sha256 of the file bytes —
//! OQ-7), adopt-vs-retain-last-good `reload`, and the 0600 credential-file
//! read. `raw` holds the serde DTOs; `Config::validate` owns every value
//! rule, so a bad file reports every field error, never a partial
//! `Config`.

mod raw;

use std::path::Path;

use governor_core::config::{Config, ConfigError, ConfigVersion};
use thiserror::Error;

use crate::adapters::jev::{ApiKey, JevError};

/// F27 — one `catalog.toml` read, decoded and core-validated: the value a
/// caller swaps in atomically on reload. `version` is `config.version`
/// verbatim, hoisted so the reload fast path and `ReloadOutcome` arms name
/// the content digest without reaching into the config.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// The validated catalog and routing policy.
    pub config: Config,
    /// Lowercase-hex sha256 of the file bytes — `config.version` verbatim
    /// (OQ-7: the file declares no version of its own).
    pub version: ConfigVersion,
}

/// F27 — why a `catalog.toml` load failed: unreadable bytes, undecodable
/// TOML (`raw` is `deny_unknown_fields` — a stray key fails here), or the
/// core's full `Vec<ConfigError>` — every invalid field, never just the
/// first.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ConfigLoadError {
    /// The file could not be read at all — missing, permissions, not a
    /// regular file. Fail-closed: no config exists to fall back to.
    #[error("catalog unreadable: {0}")]
    Read(#[source] std::io::Error),
    /// The bytes are not a well-formed catalog — malformed TOML, non-UTF-8,
    /// an unknown field, a missing required key, or a mistyped value.
    #[error("catalog undecodable: {0}")]
    Decode(#[source] toml::de::Error),
    /// Decoded but F27-invalid — every error `Config::validate` reported,
    /// in validation order.
    #[error("catalog invalid: {}", render_config_errors(.0))]
    Invalid(Vec<ConfigError>),
}

/// F27 — what `reload` did with the file's new bytes.
#[non_exhaustive]
#[derive(Debug)]
pub enum ReloadOutcome<'a> {
    /// The new bytes decoded and validated — or were byte-identical to the
    /// live ones (the digest-equality cheap path): this is the live config.
    Adopted(LoadedConfig),
    /// The new file failed to read, decode or validate; the last-good
    /// config stays live (F27) and the typed error rides along.
    Retained {
        /// The config still in force — `reload`'s `current`, unchanged.
        last_good: &'a LoadedConfig,
        /// Why the new bytes were refused.
        error: ConfigLoadError,
    },
}

/// F27 — read `path` and decode it into a validated `Config` stamped with
/// the sha256 of the file bytes. Fail-closed: any read, decode or
/// validation failure is a typed `ConfigLoadError`, never a partial
/// config. A caller that gets `Err` at startup refuses (F27); a reload
/// goes through `reload`, which retains the last good.
pub async fn load(path: &Path) -> Result<LoadedConfig, ConfigLoadError> {
    let bytes = tokio::fs::read(path).await.map_err(ConfigLoadError::Read)?;
    assemble(&bytes, ConfigVersion(sha256_hex(&bytes)))
}

/// F27 — re-read `path` and adopt or retain. Identical bytes take the
/// cheap path: equal digest ⇒ equal config (`version` is a pure function
/// of the bytes), so the adopt is a no-op without a re-decode. Any
/// read/decode/validate failure retains `current` — the daemon keeps
/// serving the last-good catalog — and the outcome carries the typed
/// error. The SIGHUP→reload wiring is the daemon's (Phase 5); what a
/// changed version invalidates is the store's concern, not this adapter's.
pub async fn reload<'a>(current: &'a LoadedConfig, path: &Path) -> ReloadOutcome<'a> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return ReloadOutcome::Retained {
                last_good: current,
                error: ConfigLoadError::Read(error),
            };
        }
    };
    let version = ConfigVersion(sha256_hex(&bytes));
    if version == current.version {
        return ReloadOutcome::Adopted(current.clone());
    }
    match assemble(&bytes, version) {
        Ok(loaded) => ReloadOutcome::Adopted(loaded),
        Err(error) => ReloadOutcome::Retained {
            last_good: current,
            error,
        },
    }
}

/// F27/spec §19 — the governor's credential file (`credentials`, mode
/// 0600): today, the Jev bearer key. The P4.C1 contract's `Credential`/
/// `CredentialError` are `ApiKey`/`JevError` — `ApiKey` is already the
/// redacted secret wrapper and `read_0600` the one reader (reuse it,
/// never a second); only `JevError::CredentialUnavailable` is reachable
/// from it. File content passes through verbatim: a `!cmd`/`$ENV` line is
/// *stored*, never executed (the CT-JEV-AUTH-1 rule, reused for the
/// governor's own file).
pub async fn load_credentials(path: &Path) -> Result<ApiKey, JevError> {
    ApiKey::read_0600(path).await
}

/// Decode `bytes` into a validated `LoadedConfig` stamped with `version` —
/// the caller hashes so `reload` can compare before paying for a decode.
fn assemble(bytes: &[u8], version: ConfigVersion) -> Result<LoadedConfig, ConfigLoadError> {
    let raw: raw::RawConfig = toml::from_slice(bytes).map_err(ConfigLoadError::Decode)?;
    let config = raw.into_config(version.clone());
    config.validate().map_err(ConfigLoadError::Invalid)?;
    Ok(LoadedConfig { config, version })
}

/// OQ-7 — `sha256(file bytes)` as lowercase hex.
///
/// The catalog version: lowercase hex SHA-256 of the file bytes (`sha2`,
/// the digest governor-core already uses; `sha256_known_answers` pins it).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase hex without a lookup table (`indexing_slicing` is denied).
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

/// `0..=15` → `'0'..='f'`; the bound holds by construction, so saturating
/// arithmetic is only to keep `arithmetic_side_effects` quiet.
fn hex_digit(nibble: u8) -> char {
    char::from(if nibble < 10 {
        b'0'.saturating_add(nibble)
    } else {
        b'a'.saturating_add(nibble.saturating_sub(10))
    })
}

/// `; `-joined `ConfigError` renders — F27's one sanitized stderr line is
/// the caller's job (it caps at 500 chars); this feeds it.
fn render_config_errors(errors: &[ConfigError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests;
