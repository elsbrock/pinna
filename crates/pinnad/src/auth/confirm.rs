//! Presence confirmation and its grants (ADR 0029): someone pressing Action at
//! the satellite proves they are there. The state machine here knows nothing
//! of the Brain or the clock; [`super::Auth`] connects them.

use std::time::{Duration, Instant};

use super::credentials::same;

/// How long the ring waits for a press.
pub const WINDOW: Duration = Duration::from_secs(60);
/// After a confirmation ended, before the next may start: nobody on the LAN
/// can keep the ring busy.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// How long a grant stays good.
pub const GRANT_TTL: Duration = Duration::from_secs(300);
/// Grants held at once. Each needs a press, so this is never reached by one
/// owner.
const MAX_GRANTS: usize = 8;

/// Where a confirmation stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Waiting,
    /// Action was pressed; the grant is the dashboard's to present once.
    Confirmed(String),
    Expired,
}

/// The latest confirmation.
#[derive(Debug)]
pub struct Current {
    pub id: String,
    pub started: Instant,
    /// When it was confirmed or expired.
    pub ended: Option<Instant>,
    pub progress: tokio::sync::watch::Sender<Progress>,
}

#[derive(Debug, Default)]
pub struct Confirmations {
    pub current: Option<Current>,
}

impl Confirmations {
    /// Seconds until a new confirmation may start; zero when it may. While one
    /// waits, its window and the cooldown after it.
    pub fn cooldown(&self, now: Instant) -> u64 {
        let Some(current) = &self.current else {
            return 0;
        };
        let end = current.ended.unwrap_or(current.started + WINDOW);
        super::limit::ceil_secs((end + COOLDOWN).saturating_duration_since(now))
    }
}

/// The grants handed out and not yet used.
#[derive(Debug, Default)]
pub struct Grants {
    held: Vec<(String, Instant)>,
}

impl Grants {
    pub fn add(&mut self, grant: String, now: Instant) {
        self.held
            .retain(|(_, at)| now.duration_since(*at) < GRANT_TTL);
        if self.held.len() == MAX_GRANTS {
            self.held.remove(0);
        }
        self.held.push((grant, now));
    }

    /// Use `presented`: true once for a grant that is still good. Compares
    /// against every grant held, so the time taken doesn't tell which matched.
    pub fn consume(&mut self, presented: &str, now: Instant) -> bool {
        let mut found = None;
        for (i, (grant, at)) in self.held.iter().enumerate() {
            if same(grant.as_bytes(), presented.as_bytes()) && now.duration_since(*at) < GRANT_TTL {
                found = Some(i);
            }
        }
        match found {
            Some(i) => {
                self.held.remove(i);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_grant_works_once_and_for_five_minutes() {
        let t = Instant::now();
        let mut g = Grants::default();
        g.add("one".into(), t);
        g.add("two".into(), t);
        assert!(!g.consume("", t));
        assert!(!g.consume("on", t));
        assert!(g.consume("one", t + s(299)));
        assert!(!g.consume("one", t + s(299)), "single use");
        assert!(!g.consume("two", t + s(300)), "expired");
    }

    #[test]
    fn only_the_newest_grants_are_kept() {
        let t = Instant::now();
        let mut g = Grants::default();
        for i in 0..=MAX_GRANTS {
            g.add(format!("g{i}"), t);
        }
        assert!(!g.consume("g0", t));
        assert!(g.consume(&format!("g{MAX_GRANTS}"), t));
    }

    #[test]
    fn a_new_confirmation_waits_a_minute_after_the_last_ended() {
        let t = Instant::now();
        let mut c = Confirmations::default();
        assert_eq!(c.cooldown(t), 0);
        let (tx, _rx) = tokio::sync::watch::channel(Progress::Waiting);
        c.current = Some(Current {
            id: "a".into(),
            started: t,
            ended: None,
            progress: tx,
        });
        // While it waits: the rest of its window and the minute after.
        assert_eq!(c.cooldown(t + s(10)), 110);
        c.current.as_mut().unwrap().ended = Some(t + s(10));
        assert_eq!(c.cooldown(t + s(10)), 60);
        assert_eq!(c.cooldown(t + s(69)), 1);
        assert_eq!(c.cooldown(t + s(70)), 0);
    }
}
