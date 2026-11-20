//! How late audio arrives, for `min_buffer_ms` (ADR 0027 §11): the 99th percentile over the
//! last minute of each chunk's delay, from the moment the server sent it to the moment it
//! arrived, reported only when it moves by more than 20 ms.
//!
//! A chunk's delay is `arrival - compute_client_time(timestamp - send_ahead)`, as the
//! specification measures it; chunks without a measured `send_ahead` don't count.

use std::collections::VecDeque;

/// The window the percentile covers.
const WINDOW_US: i64 = 60_000_000;
/// Delays kept at most. Chunks last 15 ms at the shortest, so a minute holds 4000; a server
/// that sends smaller ones can't make the window grow without bound.
const SAMPLES_MAX: usize = 8192;
/// How often the percentile is computed again, at most.
const EVERY_US: i64 = 1_000_000;
/// A new value is reported only when it is this far from the last one.
const STEP_MS: u32 = 20;
/// The protocol's range.
const MS_MAX: u32 = 5_000;

#[derive(Debug, Default)]
pub struct Lateness {
    /// Arrival and delay, in µs, oldest first.
    samples: VecDeque<(i64, i64)>,
    computed_at: Option<i64>,
    reported_ms: u32,
}

impl Lateness {
    /// The value reported now, in ms.
    pub fn reported_ms(&self) -> u32 {
        self.reported_ms
    }

    /// Adds one chunk's delay. Returns the new value to report, when it moved by more than
    /// 20 ms.
    pub fn add(&mut self, arrival_us: i64, delay_us: i64) -> Option<u32> {
        self.samples.push_back((arrival_us, delay_us));
        while self.samples.len() > SAMPLES_MAX
            || self
                .samples
                .front()
                .is_some_and(|(at, _)| arrival_us.saturating_sub(*at) > WINDOW_US)
        {
            self.samples.pop_front();
        }
        // The first value waits for a second of chunks: one chunk says little, and the first
        // after a start come in a burst.
        let Some(at) = self.computed_at else {
            self.computed_at = Some(arrival_us);
            return None;
        };
        if arrival_us.saturating_sub(at) < EVERY_US && arrival_us >= at {
            return None;
        }
        self.computed_at = Some(arrival_us);
        let ms = self.p99_ms();
        if ms.abs_diff(self.reported_ms) <= STEP_MS {
            return None;
        }
        self.reported_ms = ms;
        Some(ms)
    }

    /// The 99th percentile of the delays, in whole ms rounded up, within the protocol's range.
    fn p99_ms(&self) -> u32 {
        let mut delays: Vec<i64> = self.samples.iter().map(|(_, d)| *d).collect();
        if delays.is_empty() {
            return 0;
        }
        let rank = (delays.len() * 99).div_ceil(100) - 1;
        let (_, p99, _) = delays.select_nth_unstable(rank);
        let ms = p99.saturating_add(999).div_euclid(1_000).max(0);
        u32::try_from(ms).unwrap_or(MS_MAX).min(MS_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000;

    /// 100 chunks per second for `secs` seconds from `t0`, each delayed `delay(i)` µs.
    fn feed(l: &mut Lateness, t0: i64, secs: i64, delay: impl Fn(i64) -> i64) -> Vec<u32> {
        (0..secs * 100)
            .filter_map(|i| l.add(t0 + i * 10_000, delay(i)))
            .collect()
    }

    #[test]
    fn reports_the_99th_percentile_rounded_up() {
        let mut l = Lateness::default();
        // One chunk in 200 is 80 ms late, the rest 5 ms: the 99th percentile is 5 ms, inside
        // the step from 0.
        assert!(feed(&mut l, 0, 5, |i| if i % 200 == 7 { 80_000 } else { 5_000 }).is_empty());
        assert_eq!(l.reported_ms(), 0);
        // Two in 100 at 80 ms put the percentile there.
        let reports = feed(
            &mut l,
            5 * S,
            60,
            |i| {
                if i % 50 == 0 { 80_000 } else { 5_000 }
            },
        );
        assert_eq!(reports, [80]);
        // 30.2 ms rounds up to 31.
        let mut l = Lateness::default();
        assert_eq!(feed(&mut l, 0, 2, |_| 30_200), [31]);
    }

    #[test]
    fn changes_of_20_ms_or_less_are_not_reported() {
        let mut l = Lateness::default();
        assert_eq!(feed(&mut l, 0, 2, |_| 50_000), [50]);
        // 65 ms is within 20 of 50 for a whole minute: nothing new.
        assert!(feed(&mut l, 2 * S, 70, |_| 65_000).is_empty());
        assert_eq!(l.reported_ms(), 50);
        // 75 ms moves it, once the minute holds enough of it.
        assert_eq!(feed(&mut l, 72 * S, 70, |_| 75_000), [75]);
    }

    #[test]
    fn the_window_is_a_minute() {
        let mut l = Lateness::default();
        assert_eq!(feed(&mut l, 0, 10, |_| 200_000), [200]);
        // Once the late minute has passed, the low delays bring it down.
        let reports = feed(&mut l, 10 * S, 61, |_| 10_000);
        assert_eq!(reports, [10]);
    }

    #[test]
    fn stays_within_the_protocols_range() {
        let mut l = Lateness::default();
        assert_eq!(feed(&mut l, 0, 2, |_| 60 * S), [5_000]);
        let mut l = Lateness::default();
        // A server clock that runs ahead makes chunks look early: 0.
        assert_eq!(feed(&mut l, 0, 2, |_| -50_000), Vec::<u32>::new());
        assert_eq!(l.reported_ms(), 0);
    }

    #[test]
    fn keeps_a_bounded_number_of_samples() {
        let mut l = Lateness::default();
        for i in 0..20_000 {
            l.add(i, 1_000);
        }
        assert_eq!(l.samples.len(), SAMPLES_MAX);
    }
}
