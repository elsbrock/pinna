//! The server's clock for the media lane (ADR 0027 §9): a [`ServerClock`] that converts local
//! time to the server's through a copy of the connection's time filter, refreshed whenever the
//! filter takes a measurement.
//!
//! Local time is `CLOCK_MONOTONIC` in µs, the clock the speaker's timestamps and every receive
//! stamp use. Rust's [`Instant`] reads the same clock on Linux but doesn't expose its value, so
//! an `Instant` is converted through a pair of readings taken together: the two clocks differ
//! by a fixed offset.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use pinna_audio::ServerClock;
use pinna_sendspin::TimeFilter;

/// `CLOCK_MONOTONIC` in µs.
// `time_t` and `c_long` are i64 here, but narrower on the 32-bit device.
#[allow(clippy::useless_conversion)]
pub fn monotonic_us() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    i64::from(ts.tv_sec) * 1_000_000 + i64::from(ts.tv_nsec) / 1_000
}

/// A connection's time filter, as the timed source in the media lane reads it.
pub struct FilterClock {
    /// A copy of the filter once it has taken a measurement. The lane reads it once per 20 ms
    /// chunk; the lock is held for a conversion, never across a wait.
    filter: Mutex<Option<TimeFilter>>,
    /// An `Instant` and the monotonic µs read right after it.
    base: (Instant, i64),
}

impl Default for FilterClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterClock {
    pub fn new() -> Self {
        let instant = Instant::now();
        let us = monotonic_us();
        Self {
            filter: Mutex::new(None),
            base: (instant, us),
        }
    }

    /// Takes the connection's filter after it changed. Until it has a measurement, the
    /// server's time is unknown.
    pub fn update(&self, filter: &TimeFilter) {
        *self.filter.lock().unwrap_or_else(PoisonError::into_inner) =
            (filter.count() >= 1).then(|| filter.clone());
    }

    /// `local` in monotonic µs.
    fn local_us(&self, local: Instant) -> i64 {
        let (instant, us) = self.base;
        match local.checked_duration_since(instant) {
            Some(later) => us.saturating_add(i64::try_from(later.as_micros()).unwrap_or(i64::MAX)),
            None => us.saturating_sub(
                i64::try_from(instant.duration_since(local).as_micros()).unwrap_or(i64::MAX),
            ),
        }
    }
}

impl ServerClock for FilterClock {
    fn server_us(&self, local: Instant) -> Option<i64> {
        let local_us = self.local_us(local);
        self.filter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|f| f.compute_server_time(local_us))
    }

    fn rate(&self) -> f64 {
        // The conversions apply the drift only once the filter finds it significant; the rate
        // follows them, or the lane would correct towards a drift they ignore.
        self.filter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|f| f.uses_drift())
            .map_or(1.0, |f| 1.0 + f.drift())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_sendspin::clock::Sample;
    use std::time::Duration;

    /// A filter fed `n` exchanges with a server whose clock reads `offset` µs ahead of the
    /// player's, `drift` faster, from `t0` on, 1 s apart, each taking 400 µs.
    fn filter(t0: i64, n: i64, offset: i64, drift: f64) -> TimeFilter {
        let mut f = TimeFilter::new();
        let server = |t: i64| t + offset + ((t - t0) as f64 * drift) as i64;
        for i in 0..n {
            let t1 = t0 + i * 1_000_000;
            let sample =
                Sample::from_timestamps(t1, server(t1 + 200), server(t1 + 210), t1 + 400).unwrap();
            assert!(f.update(sample));
        }
        f
    }

    #[test]
    fn unknown_until_the_filter_has_a_measurement() {
        let clock = FilterClock::new();
        assert_eq!(clock.server_us(Instant::now()), None);
        assert_eq!(clock.rate(), 1.0);
        clock.update(&TimeFilter::new());
        assert_eq!(clock.server_us(Instant::now()), None);
    }

    #[test]
    fn converts_an_instant_through_the_monotonic_clock() {
        let clock = FilterClock::new();
        let now = monotonic_us();
        clock.update(&filter(now, 1, 7_000_000, 0.0));
        let t = Instant::now();
        let us = monotonic_us();
        let server = clock.server_us(t).unwrap();
        // The two readings of the same clock are taken a few µs apart.
        assert!((server - (us + 7_000_000)).abs() < 200, "{}", server - us);
        // An instant before the pair, too.
        let earlier = t - Duration::from_millis(500);
        let back = clock.server_us(earlier).unwrap();
        assert!((server - back - 500_000).abs() < 2, "{}", server - back);
        assert_eq!(clock.rate(), 1.0);
    }

    #[test]
    fn the_rate_follows_a_drift_the_conversions_use() {
        let clock = FilterClock::new();
        let f = filter(monotonic_us(), 60, 1_000, 200e-6);
        assert!(f.uses_drift(), "{f:?}");
        clock.update(&f);
        let rate = clock.rate();
        assert!((rate - 1.0002).abs() < 20e-6, "{rate}");
        // A filter that hasn't found the drift significant yet runs at 1.
        clock.update(&filter(monotonic_us(), 2, 1_000, 200e-6));
        assert_eq!(clock.rate(), 1.0);
    }

    #[test]
    fn monotonic_us_is_the_clock_instant_reads() {
        let (a, i0) = (monotonic_us(), Instant::now());
        std::thread::sleep(Duration::from_millis(20));
        let (b, i1) = (monotonic_us(), Instant::now());
        let d = (b - a) - i64::try_from((i1 - i0).as_micros()).unwrap();
        assert!(d.abs() < 2_000, "{d}");
    }
}
