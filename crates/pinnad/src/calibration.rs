//! Wake word calibration. While it runs, the volume buttons say what went
//! wrong: + "you missed me", − "that wasn't me". Each press moves the
//! sensitivity just far enough to fix the last near miss or wake it refers to,
//! instead of by a fixed step. Pure state; the brain applies the results.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use pinna_wakeword::WakeConfig;

/// Calibration ends this long after it starts or after the last press.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// A press refers to a wake or near miss at most this long before it: long
/// enough to walk over to the satellite after speaking from across the room.
pub const FEEDBACK_WINDOW: Duration = Duration::from_secs(15);
/// Points above the lowest setting that would have caught a near miss.
const RAISE_MARGIN: u8 = 5;
/// Points below the highest setting that would have rejected a false wake.
const LOWER_MARGIN: u8 = 3;
/// Events kept for the dashboard.
const MAX_EVENTS: usize = 10;

/// The highest sensitivity at which `score` stays below the accept threshold:
/// the setting that ignored a wake of that score.
fn highest_rejecting(score: f32) -> Option<u8> {
    match WakeConfig::min_sensitivity_for(score) {
        // Not even 100 catches it: every setting rejects it.
        None => Some(100),
        Some(0) => None,
        Some(s) => Some(s - 1),
    }
}

/// Why a press changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unchanged {
    /// No near miss (+) or wake (−) in the last few seconds.
    NothingHeard,
    /// No setting changes the outcome: a near miss below what 100 catches, or
    /// one that reached the threshold and was missed for another reason (+);
    /// a wake above what 0 rejects (−).
    NoSettingHelps,
    /// The change would undo a press the other way: a real wake and a false one
    /// score about the same here, so no setting settles both.
    WouldUndo,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    /// The wake word was detected (and acknowledged; no request started).
    Woke,
    /// + raised the sensitivity for a near miss.
    Raised,
    /// − lowered the sensitivity for a false wake.
    Lowered,
    /// A press that changed nothing.
    Unchanged(Unchanged),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Counts up from 1 within a calibration.
    pub id: u32,
    pub kind: EventKind,
    /// The + (raise) or the − button; false for a wake.
    pub raise: bool,
    /// The wake's or near miss's score, when the press referred to one.
    pub score: Option<f32>,
    pub from: u8,
    pub to: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    /// Sensitivity when calibration started, for undo.
    pub started_from: u8,
    deadline: Instant,
    last_wake: Option<(f32, Instant)>,
    near_miss: Option<(f32, Instant)>,
    /// The highest sensitivity a + needed to catch a near miss.
    needed: Option<u8>,
    /// The lowest sensitivity a − needed to reject a false wake.
    rejected: Option<u8>,
    next_id: u32,
    pub events: VecDeque<Event>,
}

impl Calibration {
    pub fn start(sensitivity: u8, now: Instant) -> Self {
        Self {
            started_from: sensitivity,
            deadline: now + IDLE_TIMEOUT,
            last_wake: None,
            near_miss: None,
            needed: None,
            rejected: None,
            next_id: 1,
            events: VecDeque::new(),
        }
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// The detector accepted a wake word.
    pub fn woke(&mut self, score: f32, sensitivity: u8, now: Instant) {
        self.last_wake = Some((score, now));
        // Blocks of this utterance that scored before the detection fired.
        self.near_miss = None;
        self.push(
            EventKind::Woke,
            false,
            Some(score),
            sensitivity,
            sensitivity,
        );
    }

    /// A block scored at least the detector's support level without a
    /// detection. Keeps the highest recent one.
    pub fn near_miss(&mut self, score: f32, now: Instant) {
        let stale = self
            .near_miss
            .is_none_or(|(s, at)| now.duration_since(at) > FEEDBACK_WINDOW || score > s);
        if stale {
            self.near_miss = Some((score, now));
        }
    }

    /// + : catch the last near miss. Returns the new sensitivity.
    pub fn raise(&mut self, current: u8, now: Instant) -> Result<u8, Unchanged> {
        self.deadline = now + IDLE_TIMEOUT;
        let Some(score) = recent(self.near_miss.take(), now) else {
            return self.unchanged(true, Unchanged::NothingHeard, None, current);
        };
        let need = match WakeConfig::min_sensitivity_for(score) {
            Some(need) if need > current => need,
            _ => return self.unchanged(true, Unchanged::NoSettingHelps, Some(score), current),
        };
        let to = need.saturating_add(RAISE_MARGIN).min(100);
        if self.rejected.is_some_and(|r| to > r) {
            return self.unchanged(true, Unchanged::WouldUndo, Some(score), current);
        }
        self.needed = Some(self.needed.map_or(need, |n| n.max(need)));
        self.push(EventKind::Raised, true, Some(score), current, to);
        Ok(to)
    }

    /// − : reject the last wake. Returns the new sensitivity.
    pub fn lower(&mut self, current: u8, now: Instant) -> Result<u8, Unchanged> {
        self.deadline = now + IDLE_TIMEOUT;
        let Some(score) = recent(self.last_wake.take(), now) else {
            return self.unchanged(false, Unchanged::NothingHeard, None, current);
        };
        let reject = match highest_rejecting(score) {
            Some(reject) if reject < current => reject,
            _ => return self.unchanged(false, Unchanged::NoSettingHelps, Some(score), current),
        };
        let to = reject.saturating_sub(LOWER_MARGIN);
        if self.needed.is_some_and(|n| to < n) {
            return self.unchanged(false, Unchanged::WouldUndo, Some(score), current);
        }
        self.rejected = Some(self.rejected.map_or(reject, |r| r.min(reject)));
        self.push(EventKind::Lowered, false, Some(score), current, to);
        Ok(to)
    }

    fn unchanged(
        &mut self,
        raise: bool,
        why: Unchanged,
        score: Option<f32>,
        current: u8,
    ) -> Result<u8, Unchanged> {
        self.push(EventKind::Unchanged(why), raise, score, current, current);
        Err(why)
    }

    fn push(&mut self, kind: EventKind, raise: bool, score: Option<f32>, from: u8, to: u8) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            id: self.next_id,
            kind,
            raise,
            score,
            from,
            to,
        });
        self.next_id += 1;
    }
}

fn recent(event: Option<(f32, Instant)>, now: Instant) -> Option<f32> {
    event
        .filter(|(_, at)| now.duration_since(*at) <= FEEDBACK_WINDOW)
        .map(|(score, _)| score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn settings_for_a_score_match_the_detector() {
        // 0.85 - 0.0044 * s: 0.55 is reached from 69 (68 gives 0.5508).
        assert_eq!(WakeConfig::min_sensitivity_for(0.55), Some(69));
        assert_eq!(highest_rejecting(0.55), Some(68));
        assert_eq!(WakeConfig::min_sensitivity_for(0.9), Some(0));
        assert_eq!(highest_rejecting(0.9), None);
        assert_eq!(WakeConfig::min_sensitivity_for(0.40), None);
        assert_eq!(highest_rejecting(0.40), Some(100));
        for score in [0.42, 0.5, 0.6, 0.7, 0.8] {
            let s = WakeConfig::min_sensitivity_for(score).unwrap();
            assert!(score >= WakeConfig::threshold_for_sensitivity(s));
            assert!(s == 0 || score < WakeConfig::threshold_for_sensitivity(s - 1));
            assert_eq!(highest_rejecting(score), Some(s - 1));
        }
    }

    #[test]
    fn plus_catches_the_last_near_miss() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        c.near_miss(0.30, t);
        c.near_miss(0.50, t + Duration::from_millis(80));
        c.near_miss(0.45, t + Duration::from_millis(160));
        // 0.50 needs 80; plus the margin.
        assert_eq!(c.raise(68, t + secs(2)), Ok(85));
        let e = c.events.back().unwrap();
        assert_eq!(
            (e.kind, e.from, e.to, e.score),
            (EventKind::Raised, 68, 85, Some(0.50))
        );
        // The near miss is used up.
        assert_eq!(c.raise(85, t + secs(3)), Err(Unchanged::NothingHeard));
    }

    #[test]
    fn plus_without_a_recent_near_miss_changes_nothing() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        assert_eq!(c.raise(68, t), Err(Unchanged::NothingHeard));
        c.near_miss(0.50, t);
        assert_eq!(c.raise(68, t + secs(16)), Err(Unchanged::NothingHeard));
        // A detection consumes the near misses that led up to it.
        c.near_miss(0.50, t + secs(20));
        c.woke(0.7, 68, t + secs(20));
        assert_eq!(c.raise(68, t + secs(21)), Err(Unchanged::NothingHeard));
    }

    #[test]
    fn plus_when_no_setting_helps() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        // Below what 100 (0.41) catches.
        c.near_miss(0.35, t);
        assert_eq!(c.raise(68, t), Err(Unchanged::NoSettingHelps));
        // Already above the threshold: missed for another reason.
        c.near_miss(0.6, t);
        assert_eq!(c.raise(68, t), Err(Unchanged::NoSettingHelps));
        assert_eq!(
            c.events.back().unwrap().kind,
            EventKind::Unchanged(Unchanged::NoSettingHelps)
        );
    }

    #[test]
    fn plus_stops_at_100() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        c.near_miss(0.42, t);
        assert_eq!(c.raise(68, t), Ok(100));
    }

    #[test]
    fn minus_rejects_the_last_wake() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        c.woke(0.62, 68, t);
        // 0.62 is rejected up to 52; minus the margin.
        assert_eq!(c.lower(68, t + secs(1)), Ok(49));
        assert_eq!(c.lower(49, t + secs(2)), Err(Unchanged::NothingHeard));
        // Too old.
        c.woke(0.62, 68, t + secs(20));
        assert_eq!(c.lower(68, t + secs(36)), Err(Unchanged::NothingHeard));
    }

    #[test]
    fn minus_when_no_setting_helps() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        c.woke(0.95, 68, t);
        assert_eq!(c.lower(68, t), Err(Unchanged::NoSettingHelps));
    }

    #[test]
    fn minus_stops_at_0() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        c.woke(0.845, 68, t);
        assert_eq!(c.lower(68, t), Ok(0));
    }

    #[test]
    fn a_press_that_would_undo_the_other_is_refused() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        // A real attempt at 0.50 needs 80.
        c.near_miss(0.50, t);
        assert_eq!(c.raise(68, t), Ok(85));
        // A false wake at 0.53 can only be rejected at 72 or below, which would
        // lose the 0.50 attempt: refused.
        c.woke(0.53, 85, t + secs(1));
        assert_eq!(c.lower(85, t + secs(1)), Err(Unchanged::WouldUndo));
        assert_eq!(
            c.events.back().unwrap().kind,
            EventKind::Unchanged(Unchanged::WouldUndo)
        );
        // And the other way: after a − for a false wake, a + below its bound is
        // refused too.
        let mut c = Calibration::start(68, t);
        c.woke(0.62, 68, t);
        assert_eq!(c.lower(68, t), Ok(49));
        c.near_miss(0.63, t + secs(1));
        assert_eq!(c.raise(49, t + secs(1)), Err(Unchanged::WouldUndo));
    }

    #[test]
    fn presses_keep_it_running() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        assert!(!c.expired(t + IDLE_TIMEOUT - secs(1)));
        assert!(c.expired(t + IDLE_TIMEOUT));
        let _ = c.raise(68, t + secs(200));
        assert!(!c.expired(t + secs(200) + IDLE_TIMEOUT - secs(1)));
        assert!(c.expired(t + secs(200) + IDLE_TIMEOUT));
        // Wakes alone do not.
        c.woke(0.7, 68, t + secs(400));
        assert!(c.expired(t + secs(200) + IDLE_TIMEOUT));
    }

    #[test]
    fn keeps_the_last_events() {
        let t = Instant::now();
        let mut c = Calibration::start(68, t);
        for _ in 0..12 {
            c.woke(0.7, 68, t);
        }
        assert_eq!(c.events.len(), MAX_EVENTS);
        assert_eq!(c.events.front().unwrap().id, 3);
        assert_eq!(c.events.back().unwrap().id, 12);
    }
}
