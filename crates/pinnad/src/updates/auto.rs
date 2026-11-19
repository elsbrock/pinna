//! Automatic installs (ADR 0022): the install window, the random minute inside
//! it, and whether a release may install by itself. Pure functions; the
//! `Updater` keeps the schedule and acts on it.
//!
//! The window is local wall-clock time in the satellite's time zone (ADR
//! 0007), so the math here is on civil times: a daylight saving change moves
//! the window's instant with the clock, as the owner reads it.

use jiff::Unit;
use jiff::civil::DateTime;
use pinna_api::ReleaseAction;
use serde::{Deserialize, Serialize};

/// After this many failed automatic attempts of one version in a row,
/// automatic installs skip it until the owner installs it by hand or a newer
/// version is found.
pub const MAX_ATTEMPTS: u32 = 3;

/// The "Install updates" setting, as stored in settings.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    Manually,
    Automatically,
}

impl InstallMode {
    pub const ALL: [InstallMode; 2] = [Self::Manually, Self::Automatically];

    /// The name Home Assistant's select shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Manually => pinna_esphome::INSTALL_MANUALLY,
            Self::Automatically => pinna_esphome::INSTALL_AUTOMATICALLY,
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.label() == s)
    }
}

/// Why two times are no window. The text is what the owner reads.
#[derive(Debug, PartialEq, Eq)]
pub enum WindowError {
    Empty(String),
    NotATime(String),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty(t) => write!(
                f,
                "The install window starts and ends at {t}, so it has no length. Choose an end \
                 time that differs from the start time."
            ),
            Self::NotATime(t) => write!(
                f,
                "\"{t}\" is not a time of day. Write it as HH:MM, for example 03:00."
            ),
        }
    }
}

impl std::error::Error for WindowError {}

/// A time of day as minutes after midnight, from `HH:MM`.
pub fn parse_time(s: &str) -> Result<u16, WindowError> {
    let bad = || WindowError::NotATime(s.to_owned());
    let (h, m) = s.split_once(':').ok_or_else(bad)?;
    let digits = |t: &str| t.len() == 2 && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || !digits(m) {
        return Err(bad());
    }
    let (h, m): (u16, u16) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

pub fn format_time(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The hours and minutes a time of day has.
pub fn split_time(minute: u16) -> (u32, u32) {
    (u32::from(minute / 60), u32::from(minute % 60))
}

/// The install window: two local times of day. It crosses midnight when the
/// end is before the start; start and end are never equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    start: u16,
    end: u16,
}

impl Window {
    /// 03:00 to 05:00, until the owner changes it.
    pub const DEFAULT: Window = Window {
        start: 3 * 60,
        end: 5 * 60,
    };

    pub fn new(start: u16, end: u16) -> Result<Self, WindowError> {
        if start == end {
            return Err(WindowError::Empty(format_time(start)));
        }
        Ok(Self { start, end })
    }

    pub fn start(self) -> u16 {
        self.start
    }

    pub fn end(self) -> u16 {
        self.end
    }

    /// Minutes from start to end, 1 to 1439.
    fn len(self) -> i64 {
        i64::from((self.end + 24 * 60 - self.start) % (24 * 60))
    }

    /// Whether the minute of the day `m` is in the window: the start
    /// belongs to it, the end doesn't.
    pub fn contains(self, m: u16) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&m)
        } else {
            m >= self.start || m < self.end
        }
    }

    pub fn contains_time(self, local: DateTime) -> bool {
        self.contains(minute_of_day(local))
    }
}

fn minute_of_day(t: DateTime) -> u16 {
    t.hour() as u16 * 60 + t.minute() as u16
}

/// `t` with the seconds cut off.
fn minute_start(t: DateTime) -> DateTime {
    t.date().at(t.hour(), t.minute(), 0, 0)
}

fn plus_minutes(t: DateTime, n: i64) -> Option<DateTime> {
    t.checked_add(jiff::Span::new().minutes(n)).ok()
}

/// The windows around `now`, oldest first: yesterday's (it may run past
/// midnight into today), today's, tomorrow's and the day after's. Each is a
/// start and an end in local time.
fn instances(window: Window, now: DateTime) -> impl Iterator<Item = (DateTime, DateTime)> {
    (-1..=2).filter_map(move |days| {
        let day = now.date().checked_add(jiff::Span::new().days(days)).ok()?;
        let start = day.at((window.start / 60) as i8, (window.start % 60) as i8, 0, 0);
        Some((start, plus_minutes(start, window.len())?))
    })
}

/// The start of the window that `now` is in, if it is in one.
pub fn window_start_at(window: Window, now: DateTime) -> Option<DateTime> {
    instances(window, now)
        .find(|(start, end)| *start <= now && now < *end)
        .map(|(start, _)| start)
}

/// A minute for an automatic install, at random: in the window `now` is in
/// when a later minute of it is left, otherwise in the next window.
/// Windows that start at or before `not_before` are skipped: the one an
/// attempt already ran in. `rand(n)` is a number below `n`.
pub fn pick_minute(
    window: Window,
    now: DateTime,
    not_before: Option<DateTime>,
    rand: &mut impl FnMut(u32) -> u32,
) -> Option<DateTime> {
    let first = plus_minutes(minute_start(now), 1)?;
    for (start, end) in instances(window, now) {
        if not_before.is_some_and(|nb| start <= nb) {
            continue;
        }
        let from = start.max(first);
        if from >= end {
            continue;
        }
        let minutes = end.since((Unit::Minute, from)).ok()?.get_minutes();
        let offset = i64::from(rand(u32::try_from(minutes).ok()?));
        return plus_minutes(from, offset);
    }
    None
}

/// What decides whether a release installs by itself, as it is right now.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    pub mode: InstallMode,
    /// Checks look at a channel: not `off`, and no `[update] manifest`.
    pub on_a_channel: bool,
    pub clock_synchronized: bool,
    /// An install is running.
    pub installing: bool,
    /// The running slot is still on trial.
    pub on_trial: bool,
    /// A `just push` overlay is active (ADR 0025).
    pub overlay: bool,
    /// What Install would do with the release the last check found.
    pub action: ReleaseAction,
    /// The release needs a higher boot level than the satellite has.
    pub boot_level_blocked: bool,
    /// Failed automatic attempts of this version in a row.
    pub failed_attempts: u32,
}

/// Why a release is never installed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Never {
    /// The version failed its trial boot.
    FailedTrialBoot,
    /// It needs a new boot image, installed over USB.
    BootLevel,
    /// The last automatic attempts failed.
    TooManyFailures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Install it now.
    Install,
    /// A release for the owner's window, which can't install just now.
    Wait,
    /// Nothing to do: installs are manual, or the satellite doesn't check.
    Off,
    /// The release is up to date, older, for another model, or already
    /// installed.
    Nothing,
    Never(Never),
}

pub fn verdict(f: &Facts) -> Verdict {
    use ReleaseAction::*;
    if f.mode != InstallMode::Automatically || !f.on_a_channel {
        return Verdict::Off;
    }
    if f.failed_attempts >= MAX_ATTEMPTS {
        return Verdict::Never(Never::TooManyFailures);
    }
    if f.action == RELEASE_ACTION_FAILED {
        return Verdict::Never(Never::FailedTrialBoot);
    }
    if f.boot_level_blocked {
        return Verdict::Never(Never::BootLevel);
    }
    if !matches!(f.action, RELEASE_ACTION_INSTALL | RELEASE_ACTION_BLOCKED) {
        return Verdict::Nothing;
    }
    if !f.clock_synchronized
        || f.installing
        || f.on_trial
        || f.overlay
        || f.action == RELEASE_ACTION_BLOCKED
    {
        return Verdict::Wait;
    }
    Verdict::Install
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn window(start: &str, end: &str) -> Window {
        Window::new(parse_time(start).unwrap(), parse_time(end).unwrap()).unwrap()
    }

    fn at(day: i8, hm: &str) -> DateTime {
        let m = parse_time(hm).unwrap();
        date(2026, 11, day).at((m / 60) as i8, (m % 60) as i8, 0, 0)
    }

    #[test]
    fn times_are_hh_mm() {
        assert_eq!(parse_time("03:00"), Ok(180));
        assert_eq!(parse_time("23:59"), Ok(1439));
        assert_eq!(format_time(305), "05:05");
        for bad in [
            "3:00", "24:00", "03:60", "0300", "03:0", "", "ab:cd", "+3:00",
        ] {
            assert_eq!(
                parse_time(bad),
                Err(WindowError::NotATime(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_window_needs_a_length_and_the_error_says_how_to_fix_it() {
        let err = Window::new(180, 180).unwrap_err();
        assert_eq!(err, WindowError::Empty("03:00".into()));
        let text = err.to_string();
        assert!(text.contains("03:00") && text.contains("Choose an end time"));
        assert_eq!(Window::DEFAULT, window("03:00", "05:00"));
    }

    #[test]
    fn a_window_contains_its_start_but_not_its_end() {
        let w = window("03:00", "05:00");
        let m = |s| parse_time(s).unwrap();
        assert!(!w.contains(m("02:59")));
        assert!(w.contains(m("03:00")));
        assert!(w.contains(m("04:59")));
        assert!(!w.contains(m("05:00")));
    }

    #[test]
    fn a_window_may_cross_midnight() {
        let w = window("23:00", "01:00");
        let m = |s| parse_time(s).unwrap();
        assert!(!w.contains(m("22:59")));
        assert!(w.contains(m("23:00")));
        assert!(w.contains(m("23:59")));
        assert!(w.contains(m("00:00")));
        assert!(w.contains(m("00:59")));
        assert!(!w.contains(m("01:00")));
        assert!(w.contains_time(at(19, "00:30")));
        assert_eq!(w.len(), 120);
        assert_eq!(window("00:00", "23:59").len(), 1439);
    }

    /// The generator that always answers `n`, to see where a pick can land.
    fn rand_at(offset: u32) -> impl FnMut(u32) -> u32 {
        move |n| offset.min(n - 1)
    }

    #[test]
    fn the_minute_is_in_the_current_window_while_one_is_ahead() {
        let w = window("03:00", "05:00");
        let now = at(19, "03:30");
        // 03:31 to 04:59 is left.
        assert_eq!(
            pick_minute(w, now, None, &mut rand_at(0)),
            Some(at(19, "03:31"))
        );
        assert_eq!(
            pick_minute(w, now, None, &mut rand_at(u32::MAX)),
            Some(at(19, "04:59"))
        );
        let mut asked = 0;
        pick_minute(w, now, None, &mut |n| {
            asked = n;
            0
        });
        assert_eq!(asked, 89);
    }

    #[test]
    fn the_last_minute_of_a_window_leaves_the_next_window() {
        let w = window("03:00", "05:00");
        assert_eq!(
            pick_minute(w, at(19, "04:59"), None, &mut rand_at(0)),
            Some(at(20, "03:00"))
        );
        // The seconds don't count: this minute is the last one.
        let late = date(2026, 11, 19).at(4, 59, 40, 0);
        assert_eq!(
            pick_minute(w, late, None, &mut rand_at(u32::MAX)),
            Some(at(20, "04:59"))
        );
    }

    #[test]
    fn before_a_window_the_minute_is_in_it() {
        let w = window("03:00", "05:00");
        assert_eq!(
            pick_minute(w, at(19, "02:00"), None, &mut rand_at(0)),
            Some(at(19, "03:00"))
        );
        assert_eq!(
            pick_minute(w, at(19, "05:00"), None, &mut rand_at(0)),
            Some(at(20, "03:00"))
        );
    }

    #[test]
    fn a_window_across_midnight_is_found_from_both_sides() {
        let w = window("23:00", "01:00");
        // Yesterday's window still runs.
        assert_eq!(
            pick_minute(w, at(19, "00:10"), None, &mut rand_at(0)),
            Some(at(19, "00:11"))
        );
        assert_eq!(window_start_at(w, at(19, "00:10")), Some(at(18, "23:00")));
        assert_eq!(
            pick_minute(w, at(19, "12:00"), None, &mut rand_at(0)),
            Some(at(19, "23:00"))
        );
        assert_eq!(window_start_at(w, at(19, "12:00")), None);
        assert_eq!(window_start_at(w, at(19, "23:30")), Some(at(19, "23:00")));
    }

    #[test]
    fn a_window_that_ran_an_attempt_is_skipped() {
        let w = window("03:00", "05:00");
        let now = at(19, "03:30");
        let ran = window_start_at(w, now);
        assert_eq!(ran, Some(at(19, "03:00")));
        assert_eq!(
            pick_minute(w, now, ran, &mut rand_at(0)),
            Some(at(20, "03:00"))
        );
        // Before the window of that day, an attempt of yesterday's doesn't block it.
        assert_eq!(
            pick_minute(w, at(20, "01:00"), ran, &mut rand_at(0)),
            Some(at(20, "03:00"))
        );
    }

    #[test]
    fn a_pick_is_spread_over_the_whole_window() {
        let w = window("03:00", "05:00");
        let mut seen = std::collections::BTreeSet::new();
        for offset in 0..120 {
            let p = pick_minute(w, at(19, "01:00"), None, &mut rand_at(offset)).unwrap();
            assert!(w.contains_time(p), "{p}");
            seen.insert(p);
        }
        assert_eq!(seen.len(), 120);
    }

    fn facts() -> Facts {
        Facts {
            mode: InstallMode::Automatically,
            on_a_channel: true,
            clock_synchronized: true,
            installing: false,
            on_trial: false,
            overlay: false,
            action: ReleaseAction::RELEASE_ACTION_INSTALL,
            boot_level_blocked: false,
            failed_attempts: 0,
        }
    }

    #[test]
    fn an_eligible_release_installs() {
        assert_eq!(verdict(&facts()), Verdict::Install);
    }

    #[test]
    fn manual_installs_and_no_channel_never_start_by_themselves() {
        let manual = Facts {
            mode: InstallMode::Manually,
            ..facts()
        };
        assert_eq!(verdict(&manual), Verdict::Off);
        let off = Facts {
            on_a_channel: false,
            ..facts()
        };
        assert_eq!(verdict(&off), Verdict::Off);
    }

    #[test]
    fn some_releases_never_install_by_themselves() {
        let failed = Facts {
            action: ReleaseAction::RELEASE_ACTION_FAILED,
            ..facts()
        };
        assert_eq!(verdict(&failed), Verdict::Never(Never::FailedTrialBoot));
        let level = Facts {
            action: ReleaseAction::RELEASE_ACTION_BLOCKED,
            boot_level_blocked: true,
            ..facts()
        };
        assert_eq!(verdict(&level), Verdict::Never(Never::BootLevel));
        let tried = Facts {
            failed_attempts: MAX_ATTEMPTS,
            ..facts()
        };
        assert_eq!(verdict(&tried), Verdict::Never(Never::TooManyFailures));
        let fewer = Facts {
            failed_attempts: MAX_ATTEMPTS - 1,
            ..facts()
        };
        assert_eq!(verdict(&fewer), Verdict::Install);
    }

    #[test]
    fn some_moments_wait() {
        let wait = |f: Facts| assert_eq!(verdict(&f), Verdict::Wait);
        wait(Facts {
            clock_synchronized: false,
            ..facts()
        });
        wait(Facts {
            installing: true,
            ..facts()
        });
        wait(Facts {
            on_trial: true,
            ..facts()
        });
        wait(Facts {
            overlay: true,
            ..facts()
        });
        // What the updater shows as blocked for the trial or the overlay.
        wait(Facts {
            action: ReleaseAction::RELEASE_ACTION_BLOCKED,
            on_trial: true,
            ..facts()
        });
    }

    #[test]
    fn nothing_installs_that_is_not_to_install() {
        use ReleaseAction::*;
        for action in [
            RELEASE_ACTION_UP_TO_DATE,
            RELEASE_ACTION_OLDER,
            RELEASE_ACTION_START,
            RELEASE_ACTION_RESTART,
            RELEASE_ACTION_OTHER_MODEL,
        ] {
            assert_eq!(
                verdict(&Facts { action, ..facts() }),
                Verdict::Nothing,
                "{action:?}"
            );
        }
    }
}
