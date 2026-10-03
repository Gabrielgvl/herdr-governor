//! `ids` — the one id source (F1, §4.2): `LaunchId` = UUID v7
//! (time-ordered, `now` in the top 48 bits + random); **`RunId` = UUID v4**
//! — its first eight hex characters are 32 random bits, which is what
//! `mint_agent_name` hashes (`run_id[0..8]`) for `gov-<8hex>` names whose
//! rare collision the `ReserveRun` re-mint retry repairs; and
//! `RelayInstanceId` = 128-bit lowercase hex. All entropy is 16 bytes from
//! `/dev/urandom` via `std::fs` (OQ-O decided) — the same source as the
//! existing store helpers.

use std::io;
use std::io::Read as _;

use governor_core::identity::{EventId, LaunchId, RelayInstanceId, RunId, Timestamp};

/// One 128-bit read from `/dev/urandom` — the process's one entropy source.
fn entropy() -> io::Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// `bytes` as lowercase hex (no `hex` crate — the existing core/store
/// code hex-encodes by hand the same way).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            // A 4-bit nibble always has a digit; the fallback is unreachable.
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    out
}

/// `bytes` rendered as `xxxxxxxx-xxxx-4xxx-…` canonical UUID text:
/// 8-4-4-4-12 groups. Version/variant bits are set by the caller first.
fn uuid_text(bytes: [u8; 16]) -> String {
    let hex = hex_encode(&bytes);
    let mut out = String::with_capacity(36);
    for (at, ch) in hex.chars().enumerate() {
        if matches!(at, 8 | 12 | 16 | 20) {
            out.push('-');
        }
        out.push(ch);
    }
    out
}

/// A fresh **`RunId` — UUID v4**: fully random except the version
/// (byte 6 high nibble `4`) and variant (byte 8 high bits `10`) bits.
/// Its first eight hex characters are `bytes[0..4]` — 32 random bits —
/// which is what `mint_agent_name` takes; a `Conflict{Run}` on
/// `ReserveRun` re-mints (→ F1).
pub fn mint_run_id() -> io::Result<RunId> {
    let mut bytes = entropy()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(RunId(uuid_text(bytes)))
}

/// A fresh **`LaunchId` — UUID v7**: `now` (epoch ms) in the top 48 bits,
/// version `7`, variant `10`, the remaining bits random. Time-ordered:
/// two ids minted at different ms sort by text. A negative timestamp
/// (before-epoch clock) pins the time field to zero.
pub fn mint_launch_id(now: Timestamp) -> io::Result<LaunchId> {
    let mut bytes = entropy()?;
    let millis = u64::try_from(now.0).unwrap_or(0);
    bytes[0..6].copy_from_slice(&millis.to_be_bytes()[2..8]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(LaunchId(uuid_text(bytes)))
}

/// A fresh **`EventId`** — the daemon's event ids are v4 like `RunId`:
/// sort order comes from `run_version`, not the id text.
pub fn mint_event_id() -> io::Result<EventId> {
    Ok(EventId(mint_run_id()?.0))
}

/// A fresh **`RelayInstanceId`** — 128 bits as 32 lowercase hex chars
/// (the `validate_caller_envelope` shape).
pub fn mint_relay_instance_id() -> io::Result<RelayInstanceId> {
    Ok(RelayInstanceId(hex_encode(&entropy()?)))
}

#[cfg(test)]
mod tests {
    use governor_core::identity::Timestamp;

    use super::{mint_event_id, mint_launch_id, mint_relay_instance_id, mint_run_id};

    /// A minted RunId is canonical v4 UUID text and its first eight hex
    /// characters are random — `mint_agent_name` uses exactly those
    /// (`run_id[0..8]`, `identity/child.rs:29-33`), so 256 mints yield
    /// ≥ 200 distinct leading octets (the birthday bound says ~255 are
    /// unique; a time-derived leading field would repeat within a 65s
    /// bucket, which is precisely the F1 failure v4 prevents).
    #[test]
    fn ids_run_id_v4_first_eight_hex_are_random() {
        let mut leading = std::collections::HashSet::new();
        for _ in 0..256 {
            let id = mint_run_id().expect("/dev/urandom read");
            let text = &id.0;
            assert_eq!(text.len(), 36, "canonical uuid text: {text}");
            assert_eq!(text.get(14..15), Some("4"), "v4 version nibble: {text}");
            let variant = text.chars().nth(19).unwrap();
            assert!(
                matches!(variant, '8' | '9' | 'a' | 'b'),
                "v4 variant bits: {text}"
            );
            leading.insert(text.get(0..8).expect("8 hex").to_string());
        }
        assert!(
            leading.len() >= 200,
            "first-eight-hex random: {} distinct of 256",
            leading.len()
        );
    }

    /// LaunchIds are v7: version nibble `7`, variant `10`, and the top 48
    /// bits are the millisecond timestamp — so ids minted at increasing
    /// `now`s sort by text while same-ms ids differ in the random tail.
    #[test]
    fn ids_launch_id_v7_is_monotonic_and_random() {
        let earlier = mint_launch_id(Timestamp(1_700_000_000_000)).expect("urandom");
        let later = mint_launch_id(Timestamp(1_700_000_001_000)).expect("urandom");
        assert_eq!(earlier.0.get(14..15), Some("7"), "v7 version nibble");
        assert!(
            earlier.0 < later.0,
            "v7 text sorts by mint time: {} !< {}",
            earlier.0,
            later.0
        );
        // Same-ms mints differ (random tail) — a timestamp-only id would
        // collide inside one millisecond.
        let a = mint_launch_id(Timestamp(1_700_000_002_000)).expect("urandom");
        let b = mint_launch_id(Timestamp(1_700_000_002_000)).expect("urandom");
        assert_ne!(a, b, "same-ms mints are distinct");
    }

    /// Relay instance ids are 32 lowercase hex chars (the
    /// `validate_caller_envelope` shape: `[0-9a-f]` × 32).
    #[test]
    fn ids_relay_instance_id_is_32_lowercase_hex() {
        let id = mint_relay_instance_id().expect("urandom");
        assert_eq!(id.0.len(), 32, "128 bits as hex");
        assert!(
            id.0.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex: {}",
            id.0
        );
        // EventIds are v4 UUID text too.
        let event = mint_event_id().expect("urandom");
        assert_eq!(event.0.get(14..15), Some("4"), "event ids are v4");
    }
}
