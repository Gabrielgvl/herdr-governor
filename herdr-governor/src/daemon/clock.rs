//! `clock` — the daemon's one clock value (spec §9: time is an input).
//! The wall-clock epoch is read exactly once, at construction
//! (`rustix::time::clock_gettime(Realtime)`, OQ-C decided), and every later
//! `now()` is `wall_at_start + elapsed` over a monotonic
//! `tokio::time::Instant` — so no lifecycle call ever re-reads the wall
//! clock, and a paused test runtime drives `now()` deterministically.

use governor_core::identity::Timestamp;
use rustix::time::{ClockId, Timespec, clock_gettime};
use tokio::time::Instant;

/// The daemon clock: one realtime epoch read plus monotonic elapsed time.
#[derive(Debug)]
pub(super) struct Clock {
    wall_at_start: i64,
    started: Instant,
}

/// `Timespec` → epoch milliseconds, saturating: a negative or overflowing
/// read pins to `i64::MIN`/`MAX` rather than wrapping — `Timestamp` is a
/// value type and never sees a wrapped time.
fn epoch_ms(spec: Timespec) -> i64 {
    spec.tv_sec
        .saturating_mul(1_000)
        .saturating_add(spec.tv_nsec.saturating_div(1_000_000))
}

impl Clock {
    /// The one epoch read: realtime now, plus the monotonic origin.
    pub(super) fn new() -> Self {
        Self {
            wall_at_start: epoch_ms(clock_gettime(ClockId::Realtime)),
            started: Instant::now(),
        }
    }

    /// `wall_at_start + elapsed` — monotonic within this process; equal or
    /// later on every call.
    pub(super) fn now(&self) -> Timestamp {
        let elapsed = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        Timestamp(self.wall_at_start.saturating_add(elapsed))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use governor_core::identity::Timestamp;

    use super::Clock;

    /// `now()` must derive from the single epoch read at construction plus
    /// monotonic elapsed — never re-read the wall clock. Built with a fixed
    /// epoch so a paused `Instant` makes the derivation exact: a clock that
    /// re-read realtime would return the host's current epoch, not the
    /// pinned `wall_at_start + elapsed`.
    #[tokio::test(start_paused = true)]
    async fn clock_is_monotonic_from_one_epoch_read() {
        let clock = Clock {
            wall_at_start: 1_000_000,
            started: tokio::time::Instant::now(),
        };
        assert_eq!(clock.now(), Timestamp(1_000_000), "zero elapsed");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            clock.now(),
            Timestamp(1_005_000),
            "now() is the one stored epoch plus monotonic elapsed"
        );
        // Monotonic by construction: repeated reads never go backwards.
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first, "later reads never regress");
        // The wall-clock epoch was captured once: the host clock advancing
        // is invisible to now() while Instant stays paused.
        tokio::time::advance(Duration::from_secs(55)).await;
        assert_eq!(
            clock.now(),
            Timestamp(1_060_000),
            "still derived from the single epoch read"
        );
    }

    /// `new()` reads a sane realtime epoch (after 2024) and is callable
    /// twice — the second origin must not precede the first.
    #[test]
    fn clock_new_reads_one_sane_epoch() {
        let first = Clock::new();
        let second = Clock::new();
        assert!(
            first.wall_at_start > 1_700_000_000_000,
            "a post-2024 epoch: {}",
            first.wall_at_start
        );
        assert!(
            second.wall_at_start >= first.wall_at_start,
            "realtime origins never regress"
        );
    }
}
