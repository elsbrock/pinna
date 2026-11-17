//! Timer state and persistence for Home Assistant mirrors and device timers (ADR 0017).

// Device-timer wiring is separate from this persistence codec.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

const CLOCK_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Ha,
    Local,
}

#[derive(Debug)]
pub(super) struct Timer {
    pub(super) name: String,
    pub(super) total: u32,
    pub(super) left: u32,
    pub(super) active: bool,
    pub(super) at: Instant,
    pub(super) origin: Origin,
}

impl Timer {
    pub(super) fn remaining(&self, now: Instant) -> u32 {
        if self.active {
            self.left
                .saturating_sub(now.saturating_duration_since(self.at).as_secs() as u32)
        } else {
            self.left
        }
    }

    fn remaining_ms(&self, now: Instant) -> u64 {
        let left = u64::from(self.left) * 1000;
        if !self.active {
            return left;
        }
        let elapsed = now.saturating_duration_since(self.at).as_millis();
        left.saturating_sub(elapsed.try_into().unwrap_or(u64::MAX))
    }
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub(super) struct SavedTimers {
    saved_unix_ms: u64,
    clock_synced: bool,
    timer: Vec<SavedTimer>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub(super) struct SavedTimer {
    id: String,
    name: String,
    total_s: u32,
    left_ms: u64,
    deadline_unix_ms: u64,
}

pub(super) fn due(timers: &HashMap<String, Timer>, now: Instant) -> Vec<String> {
    let mut due: Vec<_> = timers
        .iter()
        .filter(|(_, timer)| {
            timer.origin == Origin::Local && timer.active && timer.remaining(now) == 0
        })
        .map(|(id, _)| id.clone())
        .collect();
    due.sort();
    due
}

pub(super) fn save(
    timers: &HashMap<String, Timer>,
    now: Instant,
    wall: SystemTime,
    synced: bool,
) -> SavedTimers {
    let saved_unix_ms = unix_ms(wall);
    let mut saved: Vec<_> = timers
        .iter()
        .filter(|(_, timer)| timer.origin == Origin::Local)
        .map(|(id, timer)| {
            let left_ms = timer.remaining_ms(now);
            SavedTimer {
                id: id.clone(),
                name: timer.name.clone(),
                total_s: timer.total,
                left_ms,
                deadline_unix_ms: saved_unix_ms.saturating_add(left_ms),
            }
        })
        .collect();
    saved.sort_by(|a, b| a.id.cmp(&b.id));
    SavedTimers {
        saved_unix_ms,
        clock_synced: synced,
        timer: saved,
    }
}

pub(super) enum Restore {
    Wait,
    Ready {
        timers: Vec<(String, Timer)>,
        ring: Vec<String>,
        dropped: Vec<String>,
    },
}

pub(super) fn restore(
    saved: &SavedTimers,
    now: Instant,
    wall: SystemTime,
    synced: bool,
    waited: Duration,
) -> Restore {
    if !(synced && saved.clock_synced) {
        if !synced && waited < CLOCK_WAIT {
            return Restore::Wait;
        }
        tracing::info!("restoring device timers from their remaining time, without downtime");
        return Restore::Ready {
            timers: saved
                .timer
                .iter()
                .map(|timer| (timer.id.clone(), resume(timer, timer.left_ms, now)))
                .collect(),
            ring: Vec::new(),
            dropped: Vec::new(),
        };
    }

    let wall_ms = unix_ms(wall);
    let mut timers = Vec::new();
    let mut ring = Vec::new();
    let mut dropped = Vec::new();
    for timer in &saved.timer {
        if timer.deadline_unix_ms > wall_ms {
            let left_ms = timer.deadline_unix_ms - wall_ms;
            timers.push((timer.id.clone(), resume(timer, left_ms, now)));
        } else {
            let overdue_ms = wall_ms - timer.deadline_unix_ms;
            if overdue_ms < super::RING_TIMEOUT.as_millis() as u64 {
                ring.push(timer.id.clone());
            } else {
                tracing::info!(
                    "device timer {:?} expired over five minutes ago; dropping it",
                    timer.id
                );
                dropped.push(timer.id.clone());
            }
        }
    }
    Restore::Ready {
        timers,
        ring,
        dropped,
    }
}

fn resume(saved: &SavedTimer, left_ms: u64, now: Instant) -> Timer {
    Timer {
        name: saved.name.clone(),
        total: saved.total_s.max(1),
        left: left_ms.div_ceil(1000).min(u64::from(u32::MAX)) as u32,
        active: true,
        at: now,
        origin: Origin::Local,
    }
}

fn unix_ms(wall: SystemTime) -> u64 {
    wall.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(origin: Origin, left: u32, at: Instant) -> Timer {
        Timer {
            name: "Tea".into(),
            total: 90,
            left,
            active: true,
            at,
            origin,
        }
    }

    fn saved_timer(deadline_unix_ms: u64, left_ms: u64) -> SavedTimers {
        SavedTimers {
            saved_unix_ms: 1_000_000,
            clock_synced: true,
            timer: vec![SavedTimer {
                id: "tea".into(),
                name: "Tea".into(),
                total_s: 90,
                left_ms,
                deadline_unix_ms,
            }],
        }
    }

    fn ready(restore: Restore) -> (Vec<(String, Timer)>, Vec<String>, Vec<String>) {
        match restore {
            Restore::Ready {
                timers,
                ring,
                dropped,
            } => (timers, ring, dropped),
            Restore::Wait => panic!("timer restore is still waiting"),
        }
    }

    #[test]
    fn save_records_only_local_timers_and_their_deadlines() {
        let now = Instant::now();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000);
        let timers = HashMap::from([
            ("ha".into(), timer(Origin::Ha, 10, now)),
            ("local".into(), timer(Origin::Local, 90, now)),
        ]);
        let saved = save(&timers, now + Duration::from_millis(5_250), wall, true);
        assert_eq!(saved.saved_unix_ms, 1_000_000);
        assert!(saved.clock_synced);
        assert_eq!(saved.timer.len(), 1);
        assert_eq!(saved.timer[0].id, "local");
        assert_eq!(saved.timer[0].left_ms, 84_750);
        assert_eq!(saved.timer[0].deadline_unix_ms, 1_084_750);

        let text = toml::to_string(&saved).unwrap();
        let decoded: SavedTimers = toml::from_str(&text).unwrap();
        assert_eq!(decoded, saved);
    }

    #[test]
    fn synced_restore_keeps_a_running_timer() {
        let now = Instant::now();
        let saved = saved_timer(1_090_000, 85_000);
        let (timers, ring, dropped) = ready(restore(
            &saved,
            now,
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_010_000),
            true,
            Duration::ZERO,
        ));
        assert!(ring.is_empty() && dropped.is_empty());
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].0, "tea");
        let timer = &timers[0].1;
        assert_eq!(timer.name, "Tea");
        assert_eq!(timer.total, 90);
        assert_eq!(timer.left, 80);
        assert!(timer.active);
        assert_eq!(timer.at, now);
        assert_eq!(timer.origin, Origin::Local);
    }

    #[test]
    fn synced_restore_rings_a_recently_overdue_timer() {
        let saved = saved_timer(900_000, 0);
        let (timers, ring, dropped) = ready(restore(
            &saved,
            Instant::now(),
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000),
            true,
            Duration::ZERO,
        ));
        assert!(timers.is_empty() && dropped.is_empty());
        assert_eq!(ring, ["tea"]);
    }

    #[test]
    fn synced_restore_drops_a_timer_overdue_by_five_minutes() {
        let saved = saved_timer(700_000, 0);
        let (timers, ring, dropped) = ready(restore(
            &saved,
            Instant::now(),
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000),
            true,
            Duration::ZERO,
        ));
        assert!(timers.is_empty() && ring.is_empty());
        assert_eq!(dropped, ["tea"]);
    }

    #[test]
    fn unsynced_restore_waits_then_resumes_the_saved_time() {
        let now = Instant::now();
        let saved = saved_timer(1_090_000, 42_500);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(9_000_000);
        assert!(matches!(
            restore(&saved, now, wall, false, Duration::ZERO),
            Restore::Wait
        ));
        assert!(matches!(
            restore(&saved, now, wall, false, Duration::from_secs(59)),
            Restore::Wait
        ));
        let (timers, ring, dropped) =
            ready(restore(&saved, now, wall, false, Duration::from_secs(60)));
        assert!(ring.is_empty() && dropped.is_empty());
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].1.left, 43);
        assert_eq!(timers[0].1.at, now);
    }

    #[test]
    fn due_returns_only_device_timers_that_reached_zero() {
        let now = Instant::now();
        let mut paused = timer(Origin::Local, 0, now);
        paused.active = false;
        let timers = HashMap::from([
            ("ha".into(), timer(Origin::Ha, 1, now)),
            ("later".into(), timer(Origin::Local, 2, now)),
            ("paused".into(), paused),
            ("tea".into(), timer(Origin::Local, 1, now)),
        ]);
        assert_eq!(due(&timers, now + Duration::from_secs(1)), ["tea"]);
    }
}
