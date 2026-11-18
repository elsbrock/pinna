//! Timer state and persistence for Home Assistant mirrors and device timers (ADR 0017).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::settings::FILE_MODE;
use crate::store::Store;

const CLOCK_WAIT: Duration = Duration::from_secs(60);
const CLOCK_RECHECK: Duration = Duration::from_secs(5);
const SAVE_INTERVAL: Duration = Duration::from_secs(60);

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

/// Saved deadlines that will correct the resumed timers once the wall
/// clock can be trusted.
struct Correction {
    deadlines: Vec<(String, u64)>,
    started: Instant,
    next_check: Instant,
}

/// The device-timer file, the saved timers not yet handed to the Brain, and
/// the deadlines waiting for the wall clock.
pub(super) struct TimerStore {
    store: Store<SavedTimers>,
    saved: Option<SavedTimers>,
    correction: Option<Correction>,
    last_save: Instant,
}

impl TimerStore {
    pub(super) fn load(timers_file: &Path, restore_saved: bool) -> Self {
        let now = Instant::now();
        let (store, saved, _) = Store::load(timers_file, FILE_MODE, SavedTimers::default());
        Self {
            store,
            saved: (restore_saved && !saved.timer.is_empty()).then_some(saved),
            correction: None,
            last_save: now,
        }
    }

    pub(super) fn save(
        &mut self,
        timers: &HashMap<String, Timer>,
        now: Instant,
        wall: SystemTime,
        synced: bool,
    ) {
        self.store.save(&save(timers, now, wall, synced));
        self.last_save = now;
    }

    /// Forget every device timer, saved ones not yet restored and pending
    /// corrections included, and leave an empty file.
    pub(super) fn clear(&mut self, now: Instant, wall: SystemTime, synced: bool) {
        self.saved = None;
        self.correction = None;
        self.save(&HashMap::new(), now, wall, synced);
    }

    pub(super) fn save_needed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_save) >= SAVE_INTERVAL
    }

    /// What the Brain should do with the saved timers now: take them (at
    /// once, so they can be listed and cancelled), and later correct them
    /// from their saved deadlines once the clock has synced. `synced` is only
    /// asked when the answer is needed.
    pub(super) fn restore(
        &mut self,
        now: Instant,
        wall: SystemTime,
        synced: fn() -> bool,
    ) -> Option<Restore> {
        if let Some(saved) = self.saved.take() {
            let synced = synced();
            if saved.clock_synced && !synced {
                tracing::info!(
                    "device timers resume from their remaining time until the clock syncs"
                );
                // Resume from the saved remaining time now; the deadlines say
                // what really happened once the clock is right.
                self.correction = Some(Correction {
                    deadlines: saved
                        .timer
                        .iter()
                        .map(|timer| (timer.id.clone(), timer.deadline_unix_ms))
                        .collect(),
                    started: now,
                    next_check: now + CLOCK_RECHECK,
                });
            }
            return Some(restore(&saved, now, wall, synced));
        }
        let correction = self.correction.as_mut()?;
        if now < correction.next_check {
            return None;
        }
        if now.saturating_duration_since(correction.started) >= CLOCK_WAIT {
            tracing::info!("the clock didn't sync; device timers keep their saved remaining time");
            self.correction = None;
            return None;
        }
        if !synced() {
            correction.next_check = now + CLOCK_RECHECK;
            return None;
        }
        self.correction
            .take()
            .map(|c| Restore::Correct(c.deadlines))
    }
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
    /// The saved timers, to take: running ones, ones to ring now, and ones
    /// left out because they expired long ago.
    Ready {
        timers: Vec<(String, Timer)>,
        ring: Vec<String>,
        dropped: Vec<String>,
    },
    /// The saved deadlines, now that the clock is right: [`correct`] applies
    /// them to the timers still running.
    Correct(Vec<(String, u64)>),
}

/// The saved timers, resumed. With a trusted clock at save time and now,
/// each continues from its saved deadline; otherwise from its saved remaining
/// time, without the time the satellite was off.
pub(super) fn restore(
    saved: &SavedTimers,
    now: Instant,
    wall: SystemTime,
    synced: bool,
) -> Restore {
    if !(synced && saved.clock_synced) {
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

/// Apply saved deadlines to the resumed device timers once the clock is
/// right: a running one gets its true remaining time, one that came due rings,
/// one overdue past the ring timeout goes. A timer no longer there (it rang or
/// was cancelled meanwhile) is skipped. Returns the ones to ring and the ones
/// dropped.
pub(super) fn correct(
    timers: &mut HashMap<String, Timer>,
    deadlines: &[(String, u64)],
    now: Instant,
    wall: SystemTime,
) -> (Vec<String>, Vec<String>) {
    let wall_ms = unix_ms(wall);
    let (mut ring, mut dropped) = (Vec::new(), Vec::new());
    for (id, deadline) in deadlines {
        let Some(timer) = timers.get_mut(id).filter(|t| t.origin == Origin::Local) else {
            continue;
        };
        if *deadline > wall_ms {
            timer.left = (deadline - wall_ms).div_ceil(1000).min(u64::from(u32::MAX)) as u32;
            timer.at = now;
        } else if wall_ms - deadline < super::RING_TIMEOUT.as_millis() as u64 {
            ring.push(id.clone());
        } else {
            tracing::info!("device timer {id:?} expired over five minutes ago; dropping it");
            timers.remove(id);
            dropped.push(id.clone());
        }
    }
    (ring, dropped)
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
            Restore::Correct(_) => panic!("a restore hands over timers first"),
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
        ));
        assert!(timers.is_empty() && ring.is_empty());
        assert_eq!(dropped, ["tea"]);
    }

    #[test]
    fn unsynced_restore_resumes_the_saved_time() {
        let now = Instant::now();
        let saved = saved_timer(1_090_000, 42_500);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(9_000_000);
        let (timers, ring, dropped) = ready(restore(&saved, now, wall, false));
        assert!(ring.is_empty() && dropped.is_empty());
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].1.left, 43);
        assert_eq!(timers[0].1.at, now);
    }

    #[test]
    fn a_correction_moves_rings_or_drops_a_resumed_timer() {
        let now = Instant::now();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000);
        let mut timers = HashMap::from([
            ("later".into(), timer(Origin::Local, 90, now)),
            ("due".into(), timer(Origin::Local, 90, now)),
            ("old".into(), timer(Origin::Local, 90, now)),
        ]);
        let deadlines = [
            ("later".to_string(), 1_030_000),
            ("due".to_string(), 990_000),
            ("old".to_string(), 600_000),
            ("cancelled".to_string(), 1_050_000),
        ];
        let (ring, dropped) = correct(&mut timers, &deadlines, now, wall);
        assert_eq!(ring, ["due"]);
        assert_eq!(dropped, ["old"]);
        assert_eq!(timers["later"].left, 30);
        assert!(!timers.contains_key("old") && !timers.contains_key("cancelled"));
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

#[cfg(test)]
mod brain_tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use pinna_esphome::{DeviceEvent, EsphomeHandle};
    use tokio::sync::{mpsc, oneshot, watch};

    use super::super::{ApiCommand, Brain, Defaults, Input, Options, Shared, Snapshot};
    use super::Origin;
    use crate::leds::LedState;
    use crate::settings::{Mode, Settings};
    use crate::wakewords;

    fn brain_at(dir: &Path, mode: Mode) -> (Brain, mpsc::Receiver<DeviceEvent>) {
        let (esp, events) = EsphomeHandle::detached(64);
        let brain = Brain::new(
            esp,
            None,
            Arc::<Shared>::default(),
            watch::channel(LedState::default()).0,
            watch::channel(Snapshot::default()).0,
            mpsc::channel(64).0,
            Options {
                key_file: dir.join("key"),
                settings_file: dir.join("settings.toml"),
                wake_words: wakewords::test_catalogue(),
                active_wake_words: vec!["alexa_v0.1".into()],
                keywords: None,
                defaults: Defaults {
                    node_name: "pinna-test".into(),
                    friendly_name: "Test".into(),
                    default_name: true,
                    wake_sound: false,
                },
                mdns: None,
                encrypted: false,
                has_hw_privacy: false,
                airplay: false,
                bluetooth: false,
                settings: Settings {
                    mode,
                    ..Settings::default()
                },
            },
        );
        (brain, events)
    }

    async fn set_timer(
        brain: &mut Brain,
        name: Option<&str>,
        seconds: u32,
    ) -> Result<String, String> {
        let (reply, result) = oneshot::channel();
        brain
            .handle(Input::Api(ApiCommand::SetTimer {
                name: name.map(str::to_owned),
                seconds,
                reply,
            }))
            .await;
        result.await.unwrap()
    }

    async fn cancel_timer(brain: &mut Brain, id_or_name: &str) -> Result<Snapshot, String> {
        let (reply, result) = oneshot::channel();
        brain
            .handle(Input::Api(ApiCommand::CancelTimer(
                id_or_name.into(),
                reply,
            )))
            .await;
        result.await.unwrap()
    }

    #[tokio::test]
    async fn a_one_second_device_timer_rings_after_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let (mut brain, mut events) = brain_at(dir.path(), Mode::Standalone);
        let started = Instant::now();
        let id = set_timer(&mut brain, Some("Tea"), 1).await.unwrap();
        assert_eq!(brain.snapshot().timers.len(), 1);
        assert_eq!(
            events.try_recv().unwrap(),
            DeviceEvent::Event {
                object_id: "timer".into(),
                event_type: "started".into(),
            }
        );

        // Ticked a second on, not slept: the test can't be late.
        brain
            .tick_timers(
                started + Duration::from_millis(1_100),
                SystemTime::now(),
                || false,
            )
            .await;

        assert_eq!(brain.ringing.as_ref().map(|(id, _)| id), Some(&id));
        assert!(brain.timers.is_empty());
        assert_eq!(
            events.try_recv().unwrap(),
            DeviceEvent::Event {
                object_id: "timer".into(),
                event_type: "finished".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_device_timer_can_be_cancelled_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (mut brain, _events) = brain_at(dir.path(), Mode::Standalone);
        set_timer(&mut brain, Some("Tea"), 60).await.unwrap();

        let snapshot = cancel_timer(&mut brain, "Tea").await.unwrap();

        assert!(snapshot.timers.is_empty());
        assert!(brain.timers.is_empty());
        let saved = std::fs::read_to_string(dir.path().join("timers.toml")).unwrap();
        assert!(!saved.contains("[[timer]]"));
    }

    #[tokio::test]
    async fn home_assistant_mode_refuses_device_timer_commands() {
        let dir = tempfile::tempdir().unwrap();
        let (mut brain, _events) = brain_at(dir.path(), Mode::HomeAssistant);

        assert!(set_timer(&mut brain, None, 1).await.is_err());
        assert!(cancel_timer(&mut brain, "Tea").await.is_err());
        assert!(brain.timers.is_empty());
        assert!(!dir.path().join("timers.toml").exists());
    }

    #[tokio::test]
    async fn a_new_brain_restores_the_saved_device_timer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut first, _events) = brain_at(dir.path(), Mode::Standalone);
        let id = set_timer(&mut first, Some("Tea"), 120).await.unwrap();
        let saved_at = Instant::now();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        first.timer_store.save(&first.timers, saved_at, wall, true);
        drop(first);

        // Booted without a synced clock: the timer is there at once, from
        // its saved remaining time, and can be listed.
        let (mut restored, _events) = brain_at(dir.path(), Mode::Standalone);
        let booted = Instant::now();
        restored.restore_timers(booted, wall, || false).await;
        assert_eq!(restored.snapshot().timers.len(), 1);
        assert_eq!(restored.timers[&id].left, 120);

        // The clock syncs and says 5 s more went by than the countdown knows.
        let synced_at = booted + Duration::from_secs(5);
        restored
            .restore_timers(synced_at, wall + Duration::from_secs(10), || true)
            .await;
        let timer = &restored.timers[&id];
        assert_eq!(timer.name, "Tea");
        assert_eq!(timer.origin, Origin::Local);
        assert_eq!(timer.left, 110);
        assert_eq!(timer.at, synced_at);
    }

    #[tokio::test]
    async fn a_timer_cancelled_while_the_clock_syncs_stays_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (mut first, _events) = brain_at(dir.path(), Mode::Standalone);
        set_timer(&mut first, Some("Tea"), 120).await.unwrap();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        first
            .timer_store
            .save(&first.timers, Instant::now(), wall, true);
        drop(first);

        let (mut restored, _events) = brain_at(dir.path(), Mode::Standalone);
        let booted = Instant::now();
        restored.restore_timers(booted, wall, || false).await;
        cancel_timer(&mut restored, "Tea").await.unwrap();
        restored
            .restore_timers(booted + Duration::from_secs(5), wall, || true)
            .await;
        assert!(restored.timers.is_empty());
        assert!(restored.ringing.is_none());
    }
}
