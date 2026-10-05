//! `devin_log::time` — the self-contained RFC3339 codec the limit
//! records carry (no clock, no dependency): Hinnant's civil-day
//! algorithms in the store codec's shape (`store/rows.rs`) so the same
//! named saturating/Euclidean arithmetic the lint set requires is the
//! only spelling here.

/// Days after 1970-01-01 for the civil date — Hinnant's algorithm, in the
/// store codec's shape (`store/rows.rs`): the same named arithmetic the
/// lint set requires.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let shifted = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = shifted.div_euclid(400);
    let yoe = shifted.rem_euclid(400);
    let mp = month.saturating_add(9).rem_euclid(12);
    let doy = mp
        .saturating_mul(153)
        .saturating_add(2)
        .div_euclid(5)
        .saturating_add(day)
        .saturating_sub(1);
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe.div_euclid(4))
        .saturating_sub(yoe.div_euclid(100))
        .saturating_add(doy);
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

/// The inverse of `days_from_civil` — `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days.saturating_add(719_468);
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe.div_euclid(1_460))
        .saturating_add(doe.div_euclid(36_524))
        .saturating_sub(doe.div_euclid(146_096))
        .div_euclid(365);
    let year_base = era.saturating_mul(400).saturating_add(yoe);
    let doy = doe
        .saturating_sub(yoe.saturating_mul(365))
        .saturating_sub(yoe.div_euclid(4))
        .saturating_add(yoe.div_euclid(100));
    let mp = doy.saturating_mul(5).saturating_add(2).div_euclid(153);
    let day = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2).div_euclid(5))
        .saturating_add(1);
    let month = mp.saturating_add(if mp < 10 { 3 } else { -9 });
    let year = year_base.saturating_add(i64::from(month <= 2));
    (year, month, day)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM|±HHMM]` to epoch ms — the
/// `Date.parse` shape the records carry; a sub-ms fraction truncates.
/// `None` for anything else (a non-conforming timestamp is no evidence).
pub(in crate::adapters::transcript) fn rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let field = |range: core::ops::Range<usize>| -> Option<i64> {
        text.get(range)?.bytes().try_fold(0_i64, |acc, b| {
            b.is_ascii_digit().then(|| {
                acc.saturating_mul(10)
                    .saturating_add(i64::from(b.wrapping_sub(b'0')))
            })
        })
    };
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    let mut ms = days_from_civil(year, month, day)
        .saturating_mul(86_400_000)
        .saturating_add(hour.saturating_mul(3_600_000))
        .saturating_add(minute.saturating_mul(60_000))
        .saturating_add(second.saturating_mul(1_000));
    let mut rest = text.get(19..)?;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let millis = frac
            .get(..digits.min(3))
            .and_then(|slice| slice.parse::<i64>().ok())?;
        let scaled = match 3_usize.saturating_sub(digits.min(3)) {
            0 => millis,
            1 => millis.saturating_mul(10),
            _ => millis.saturating_mul(100),
        };
        ms = ms.saturating_add(scaled);
        rest = frac.get(digits..)?;
    }
    let offset_minutes: i64 = match rest.as_bytes().first() {
        Some(b'Z' | b'z') if rest.len() == 1 => 0,
        Some(b'+' | b'-') => {
            let sign: i64 = if rest.starts_with('-') { -1 } else { 1 };
            let body = rest.get(1..)?;
            let (hours, minutes) = match body.split_once(':') {
                Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
                None if body.len() == 4 && body.bytes().all(|b| b.is_ascii_digit()) => (
                    body.get(..2).and_then(|s| s.parse().ok())?,
                    body.get(2..).and_then(|s| s.parse().ok())?,
                ),
                None => return None,
            };
            sign.saturating_mul(hours.saturating_mul(60).saturating_add(minutes))
        }
        Some(_) | None => return None,
    };
    Some(ms.saturating_sub(offset_minutes.saturating_mul(60_000)))
}

/// Render epoch ms as `YYYY-MM-DDTHH:MM:SS.mmmZ` — the wire spelling the
/// store codec produces (`store/rows.rs`'s `ts_encode` shape).
pub(in crate::adapters::transcript) fn ms_rfc3339(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let within = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = within.div_euclid(3_600_000);
    let minute = within.rem_euclid(3_600_000).div_euclid(60_000);
    let second = within.rem_euclid(60_000).div_euclid(1_000);
    let millis = within.rem_euclid(1_000);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_ms_parses_z_frac_and_offsets() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0), "the epoch");
        assert_eq!(
            rfc3339_ms("1970-01-01T00:00:00.421266Z"),
            Some(421),
            "micros truncate to ms"
        );
        assert_eq!(
            rfc3339_ms("1970-01-01T01:30:00+01:30"),
            Some(0),
            "offset applies"
        );
        assert_eq!(
            rfc3339_ms("1970-01-01T01:30:00.5+01:30"),
            Some(500),
            "frac + offset"
        );
        for bad in [
            "",
            "2026-13-40T99:99:99Z",
            "not a timestamp",
            "2026-10-02T01:33:04",
        ] {
            assert_eq!(rfc3339_ms(bad), None, "{bad:?} refuses");
        }
        // the last has no zone — Date.parse would read local; the parser
        // refuses rather than guess a zone
    }

    #[test]
    fn ms_rfc3339_round_trips() {
        assert_eq!(ms_rfc3339(0), "1970-01-01T00:00:00.000Z");
        let ts = rfc3339_ms("2026-10-02T01:33:04.421266Z").expect("parses");
        assert_eq!(ms_rfc3339(ts), "2026-10-02T01:33:04.421Z");
    }
}
