//! The virtual clock: one atomic nanosecond counter serving every time API.
//!
//! Model:
//! - Virtual-monotonic time starts at 0 and only moves forward.
//! - Every *read* advances it by [`TICK_NS`] so that programs polling the
//!   clock in a loop ("spin until deadline") make progress, and so that two
//!   reads never return the same instant (timestamps are strictly increasing
//!   even across threads — `fetch_add` hands out disjoint intervals).
//! - Sleeps don't sleep: they advance the counter by the requested duration
//!   and return immediately. Deterministic and fast.
//! - Virtual-realtime = fixed epoch (2000-01-01T00:00:00Z) + a seed-derived
//!   offset in [0, 1 year) + virtual-monotonic. Different seeds therefore see
//!   different wall-clock dates, which deliberately exercises date-dependent
//!   target logic.

use core::sync::atomic::{AtomicU64, Ordering};

/// Amount each clock *observation* advances virtual time.
pub const TICK_NS: u64 = 1_000; // 1 µs

/// 2000-01-01T00:00:00Z as a Unix timestamp, in nanoseconds.
pub const EPOCH_BASE_NS: u64 = 946_684_800 * NANOS_PER_SEC;

pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Seed-derived realtime offsets span up to ~1 year (in seconds) so different
/// seeds land on different dates without overflowing 32-bit `time_t` math in
/// sloppy targets before ~2040.
pub const MAX_SEED_OFFSET_SECS: u64 = 365 * 24 * 3600;

#[derive(Debug)]
pub struct VClock {
    /// Virtual-monotonic nanoseconds since process start.
    mono_ns: AtomicU64,
    /// Realtime base: `EPOCH_BASE_NS` + seed-derived offset.
    real_base_ns: u64,
}

impl VClock {
    #[must_use]
    pub fn new(seed_offset_secs: u64) -> Self {
        Self {
            mono_ns: AtomicU64::new(0),
            real_base_ns: EPOCH_BASE_NS
                + (seed_offset_secs % MAX_SEED_OFFSET_SECS) * NANOS_PER_SEC,
        }
    }

    /// Observe the monotonic clock, advancing it one tick.
    pub fn now_mono_ns(&self) -> u64 {
        self.mono_ns.fetch_add(TICK_NS, Ordering::Relaxed) + TICK_NS
    }

    /// Observe the realtime clock, advancing the underlying counter one tick.
    pub fn now_real_ns(&self) -> u64 {
        self.real_base_ns + self.now_mono_ns()
    }

    /// Advance virtual time by `ns` without producing an observation (sleeps).
    pub fn advance_ns(&self, ns: u64) {
        self.mono_ns.fetch_add(ns, Ordering::Relaxed);
    }

    /// Advance the monotonic clock to at least `target_ns` (absolute-deadline
    /// sleeps against `CLOCK_MONOTONIC`). Past deadlines are a no-op.
    pub fn advance_mono_to(&self, target_ns: u64) {
        self.mono_ns.fetch_max(target_ns, Ordering::Relaxed);
    }

    /// Advance the clock so virtual-realtime reaches at least `target_ns`
    /// (absolute-deadline sleeps, `TIMER_ABSTIME`). Past deadlines are a no-op.
    pub fn advance_real_to(&self, target_ns: u64) {
        let target_mono = target_ns.saturating_sub(self.real_base_ns);
        // fetch_max keeps concurrent absolute sleeps monotonic: whichever
        // deadline is furthest wins, and time still never goes backwards.
        self.mono_ns.fetch_max(target_mono, Ordering::Relaxed);
    }

    /// Current realtime base (for converting absolute deadlines).
    #[must_use]
    pub fn real_base_ns(&self) -> u64 {
        self.real_base_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strictly_increasing_and_ticking() {
        let c = VClock::new(0);
        let a = c.now_mono_ns();
        let b = c.now_mono_ns();
        assert_eq!(a, TICK_NS);
        assert_eq!(b, 2 * TICK_NS);
    }

    #[test]
    fn sleep_advances_without_observation() {
        let c = VClock::new(0);
        c.advance_ns(5 * NANOS_PER_SEC);
        assert_eq!(c.now_mono_ns(), 5 * NANOS_PER_SEC + TICK_NS);
    }

    #[test]
    fn realtime_has_seed_offset_and_epoch_base() {
        let c = VClock::new(1234);
        assert_eq!(
            c.now_real_ns(),
            EPOCH_BASE_NS + 1234 * NANOS_PER_SEC + TICK_NS
        );
    }

    #[test]
    fn absolute_advance_never_goes_backwards() {
        let c = VClock::new(0);
        c.advance_ns(10 * NANOS_PER_SEC);
        c.advance_real_to(EPOCH_BASE_NS + 3 * NANOS_PER_SEC); // in the past
        assert!(c.now_mono_ns() > 10 * NANOS_PER_SEC);
    }

    #[test]
    fn concurrent_observations_are_unique() {
        use std::collections::HashSet;
        use std::sync::Arc;
        let c = Arc::new(VClock::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c = Arc::clone(&c);
            handles.push(std::thread::spawn(move || {
                (0..10_000).map(|_| c.now_mono_ns()).collect::<Vec<_>>()
            }));
        }
        let mut seen = HashSet::new();
        for h in handles {
            for v in h.join().unwrap() {
                assert!(seen.insert(v), "duplicate timestamp {v}");
            }
        }
    }
}
