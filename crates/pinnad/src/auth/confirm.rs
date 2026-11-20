//! Presence confirmation and its grants (ADR 0029, ADR 0030): someone pressing
//! Action at the satellite proves they are there. The state machine here knows
//! nothing of the Brain or the clock; [`super::Auth`] connects them.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::credentials::same;

/// How long the ring waits for a press.
pub const WINDOW: Duration = Duration::from_secs(60);
/// After a confirmation ended, before the next may start: nobody on the LAN
/// can keep the ring busy.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// After a confirmation was canceled: a stray click shouldn't lock the owner
/// out for a minute, and ten seconds still keep anyone from holding the ring.
pub const COOLDOWN_AFTER_CANCEL: Duration = Duration::from_secs(10);
/// How long a password grant stays good.
pub const GRANT_TTL: Duration = Duration::from_secs(300);
/// How long a recording grant stays good: the recording follows the press
/// (ADR 0030).
pub const RECORDING_GRANT_TTL: Duration = Duration::from_secs(10);
/// Grants held at once. Each needs a press, so this is never reached by one
/// owner.
const MAX_GRANTS: usize = 8;

/// What a press of Action is asked for. A grant is good only for its purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Setting or removing the dashboard password (ADR 0029).
    Password,
    /// A recording from the microphones (ADR 0030).
    Recording,
}

impl Purpose {
    fn grant_ttl(self) -> Duration {
        match self {
            Purpose::Password => GRANT_TTL,
            Purpose::Recording => RECORDING_GRANT_TTL,
        }
    }
}

/// Where a confirmation stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Waiting,
    /// Action was pressed; the grant is the dashboard's to present once.
    Confirmed(String),
    Expired,
    /// The browser that asked, or a signed-in owner, stopped it.
    Canceled,
}

/// The latest confirmation.
#[derive(Debug)]
pub struct Current {
    pub id: String,
    /// Who asked, for the Overview's warning.
    pub from: IpAddr,
    pub purpose: Purpose,
    /// The cookie value of the session that asked, for a recording: its grant
    /// belongs to that session.
    pub session: Option<String>,
    pub started: Instant,
    /// When it was confirmed, expired or canceled.
    pub ended: Option<Instant>,
    /// After it ended, before the next may start.
    pub cooldown: Duration,
    /// Wakes the task that waits for the press, when it is canceled.
    pub cancel: Arc<tokio::sync::Notify>,
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
        super::limit::ceil_secs((end + current.cooldown).saturating_duration_since(now))
    }
}

/// One grant and what it is good for.
#[derive(Debug)]
struct Held {
    grant: String,
    at: Instant,
    purpose: Purpose,
    /// The session a recording grant belongs to.
    session: Option<String>,
}

/// The grants handed out and not yet used.
#[derive(Debug, Default)]
pub struct Grants {
    held: Vec<Held>,
}

impl Grants {
    pub fn add(&mut self, grant: String, now: Instant, purpose: Purpose, session: Option<String>) {
        self.held
            .retain(|h| now.duration_since(h.at) < h.purpose.grant_ttl());
        if self.held.len() == MAX_GRANTS {
            self.held.remove(0);
        }
        self.held.push(Held {
            grant,
            at: now,
            purpose,
            session,
        });
    }

    /// Use `presented` for `purpose`: true once for a grant that is still
    /// good, was made for that purpose and, when it belongs to a session, is
    /// presented from it. A refused grant stays good for what it is for.
    /// Compares against every grant held, so the time taken doesn't tell which
    /// matched.
    pub fn consume(
        &mut self,
        presented: &str,
        purpose: Purpose,
        session: Option<&str>,
        now: Instant,
    ) -> bool {
        let mut found = None;
        for (i, h) in self.held.iter().enumerate() {
            let grant = same(h.grant.as_bytes(), presented.as_bytes());
            let mine = match (&h.session, session) {
                (Some(a), Some(b)) => same(a.as_bytes(), b.as_bytes()),
                (Some(_), None) => false,
                (None, _) => true,
            };
            if grant
                && mine
                && h.purpose == purpose
                && now.duration_since(h.at) < h.purpose.grant_ttl()
            {
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
        g.add("one".into(), t, Purpose::Password, None);
        g.add("two".into(), t, Purpose::Password, None);
        let password =
            |g: &mut Grants, grant: &str, at| g.consume(grant, Purpose::Password, None, at);
        assert!(!password(&mut g, "", t));
        assert!(!password(&mut g, "on", t));
        assert!(password(&mut g, "one", t + s(299)));
        assert!(!password(&mut g, "one", t + s(299)), "single use");
        assert!(!password(&mut g, "two", t + s(300)), "expired");
    }

    #[test]
    fn a_grant_is_good_only_for_its_purpose() {
        let t = Instant::now();
        let mut g = Grants::default();
        g.add("p".into(), t, Purpose::Password, None);
        g.add("r".into(), t, Purpose::Recording, Some("mine".into()));
        assert!(!g.consume("p", Purpose::Recording, Some("mine"), t));
        assert!(!g.consume("r", Purpose::Password, Some("mine"), t));
        // A refusal doesn't use a grant up.
        assert!(g.consume("p", Purpose::Password, None, t));
        assert!(g.consume("r", Purpose::Recording, Some("mine"), t));
    }

    #[test]
    fn a_recording_grant_belongs_to_its_session_and_lasts_ten_seconds() {
        let t = Instant::now();
        let mut g = Grants::default();
        g.add("r".into(), t, Purpose::Recording, Some("mine".into()));
        assert!(!g.consume("r", Purpose::Recording, None, t));
        assert!(!g.consume("r", Purpose::Recording, Some("theirs"), t));
        assert!(g.consume("r", Purpose::Recording, Some("mine"), t + s(9)));
        let again = g.consume("r", Purpose::Recording, Some("mine"), t + s(9));
        assert!(!again, "single use");
        g.add("late".into(), t, Purpose::Recording, Some("mine".into()));
        assert!(!g.consume("late", Purpose::Recording, Some("mine"), t + s(10)));
    }

    #[test]
    fn only_the_newest_grants_are_kept() {
        let t = Instant::now();
        let mut g = Grants::default();
        for i in 0..=MAX_GRANTS {
            g.add(format!("g{i}"), t, Purpose::Password, None);
        }
        assert!(!g.consume("g0", Purpose::Password, None, t));
        let newest = format!("g{MAX_GRANTS}");
        assert!(g.consume(&newest, Purpose::Password, None, t));
    }

    #[test]
    fn a_new_confirmation_waits_a_minute_after_the_last_ended() {
        let t = Instant::now();
        let mut c = Confirmations::default();
        assert_eq!(c.cooldown(t), 0);
        let (tx, _rx) = tokio::sync::watch::channel(Progress::Waiting);
        c.current = Some(Current {
            id: "a".into(),
            from: "192.168.1.9".parse().unwrap(),
            purpose: Purpose::Password,
            session: None,
            started: t,
            ended: None,
            cooldown: COOLDOWN,
            cancel: Arc::default(),
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
