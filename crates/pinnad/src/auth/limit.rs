//! How fast logins may be tried (ADR 0029): for the whole satellite, not per
//! address, because addresses on a LAN cost nothing. Pure over the times it is
//! given, so the tests need no clock.

use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime};

/// Between two attempts.
const GAP: Duration = Duration::from_secs(1);
/// After this many failures in a row...
const STREAK: u32 = 10;
/// ...between two attempts, until a login succeeds or a press resets it.
const SLOW_GAP: Duration = Duration::from_secs(60);

/// Whole seconds, rounded up: "try again in 0 s" would be a lie.
pub fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// The last failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub address: IpAddr,
    pub at: SystemTime,
}

#[derive(Debug, Default)]
pub struct Limiter {
    /// The last attempt that was taken. A refused one doesn't count, so
    /// hammering doesn't push the next turn away.
    last: Option<Instant>,
    /// Failures in a row.
    streak: u32,
    /// Failures since the last success, for the Password and tokens page.
    since_login: u32,
    last_failure: Option<Failure>,
}

impl Limiter {
    fn gap(&self) -> Duration {
        if self.streak >= STREAK { SLOW_GAP } else { GAP }
    }

    /// How long until an attempt is taken; zero when one is.
    pub fn wait(&self, now: Instant) -> Duration {
        self.last.map_or(Duration::ZERO, |last| {
            self.gap()
                .saturating_sub(now.saturating_duration_since(last))
        })
    }

    /// Take an attempt, or say how long to wait. Whole seconds, at least one.
    pub fn admit(&mut self, now: Instant) -> Result<(), u64> {
        let wait = self.wait(now);
        if wait.is_zero() {
            self.last = Some(now);
            Ok(())
        } else {
            Err(ceil_secs(wait))
        }
    }

    pub fn failed(&mut self, address: IpAddr, at: SystemTime) {
        self.streak += 1;
        self.since_login += 1;
        self.last_failure = Some(Failure { address, at });
    }

    pub fn succeeded(&mut self) {
        self.streak = 0;
        self.since_login = 0;
    }

    /// A press at the satellite: nothing is held back any more.
    pub fn reset(&mut self) {
        self.succeeded();
        self.last = None;
    }

    pub fn since_login(&self) -> u32 {
        self.since_login
    }

    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHO: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 9));

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn one_attempt_a_second_for_the_whole_satellite() {
        let t = Instant::now();
        let mut l = Limiter::default();
        assert_eq!(l.admit(t), Ok(()));
        assert_eq!(l.admit(t + Duration::from_millis(400)), Err(1));
        // The refused attempt didn't move the turn.
        assert_eq!(l.admit(t + s(1)), Ok(()));
    }

    #[test]
    fn ten_failures_in_a_row_slow_it_to_one_a_minute() {
        let t = Instant::now();
        let mut l = Limiter::default();
        for i in 0..10 {
            assert_eq!(l.admit(t + s(i)), Ok(()), "attempt {i}");
            l.failed(WHO, SystemTime::now());
        }
        assert_eq!(l.admit(t + s(10)), Err(59));
        assert_eq!(l.admit(t + s(9 + 60)), Ok(()));
        l.failed(WHO, SystemTime::now());
        assert_eq!(l.since_login(), 11);
        assert_eq!(l.last_failure().unwrap().address, WHO);

        // A success ends the slow turn.
        l.succeeded();
        assert_eq!(l.since_login(), 0);
        assert_eq!(l.admit(t + s(70 + 1)), Ok(()));
    }

    #[test]
    fn a_press_lets_the_next_attempt_through_at_once() {
        let t = Instant::now();
        let mut l = Limiter::default();
        for i in 0..10 {
            l.admit(t + s(i)).unwrap();
            l.failed(WHO, SystemTime::now());
        }
        assert!(l.admit(t + s(10)).is_err());
        l.reset();
        assert_eq!(l.admit(t + s(10)), Ok(()));
    }
}
