//! Local time for the time tool and the brain's context block (ADR 0017, ADR 0007).
//!
//! The zone is the IANA name from the time zone setting. jiff reads the system
//! tzdb; `libc::localtime_r` is not used because musl caches `/etc/localtime`,
//! which pinnad repoints at runtime.

use std::path::Path;
use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Whether systemd-timesyncd has set the clock since boot. Until it has, a
/// TLS failure is most likely the clock, and wall-clock deadlines can't be
/// trusted.
pub fn synced() -> bool {
    Path::new("/run/systemd/timesync/synchronized").exists()
}

/// The local time and zone for a request.
///
/// `zone` is the IANA name from [`crate::settings::Settings::time_zone`].
/// `None`, an empty name or a name the tzdb does not know is UTC. The time is
/// written the way it is spoken, for example `Tuesday 17 November 2026, 14:03`.
pub fn local_now(zone: Option<&str>, now: SystemTime) -> (String, String) {
    let (tz, label) = resolve(zone);
    (speak(&tz, now), label)
}

/// The time zone and the label to show for a setting value.
pub(crate) fn resolve(zone: Option<&str>) -> (TimeZone, String) {
    let name = zone.map(str::trim).filter(|name| !name.is_empty());
    match name {
        None => (TimeZone::UTC, "UTC".to_owned()),
        Some(name) => match TimeZone::get(name) {
            Ok(tz) => {
                let label = tz.iana_name().unwrap_or(name).to_owned();
                (tz, label)
            }
            Err(_) => {
                tracing::warn!(zone = name, "unknown time zone, using UTC");
                (TimeZone::UTC, "UTC".to_owned())
            }
        },
    }
}

/// Formats `now` in `tz` the way it is spoken.
fn speak(tz: &TimeZone, now: SystemTime) -> String {
    // A `SystemTime` outside jiff's range falls back to the epoch; a real
    // clock, even unset, is never that far out.
    let ts = Timestamp::try_from(now).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.to_zoned(tz.clone())
        .strftime("%A %-d %B %Y, %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// 2026-11-17 14:03:00 UTC, a Tuesday.
    fn tuesday() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_794_924_180)
    }

    /// The zones the named tests use come from the tzdb, which a bare
    /// development machine may not have.
    fn zone_present(name: &str) -> bool {
        let dir = std::env::var_os("TZDIR").unwrap_or_else(|| "/usr/share/zoneinfo".into());
        Path::new(&dir).join(name).exists()
    }

    #[test]
    fn unset_zone_is_utc() {
        let (time, zone) = local_now(None, tuesday());
        assert_eq!(time, "Tuesday 17 November 2026, 14:03");
        assert_eq!(zone, "UTC");
    }

    #[test]
    fn empty_zone_is_utc() {
        let (time, zone) = local_now(Some("  "), tuesday());
        assert_eq!(time, "Tuesday 17 November 2026, 14:03");
        assert_eq!(zone, "UTC");
    }

    #[test]
    fn a_fixed_offset_shifts_the_time() {
        let tz = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(speak(&tz, tuesday()), "Tuesday 17 November 2026, 16:03");
    }

    #[test]
    fn a_single_digit_day_has_no_leading_zero() {
        // 2026-01-07 12:00:00 UTC, a Wednesday.
        let t = UNIX_EPOCH + Duration::from_secs(1_767_787_200);
        let (time, zone) = local_now(None, t);
        assert_eq!(time, "Wednesday 7 January 2026, 12:00");
        assert_eq!(zone, "UTC");
    }

    #[test]
    fn berlin_uses_winter_time() {
        if !zone_present("Europe/Berlin") {
            eprintln!("skipping: no tzdb");
            return;
        }
        let (time, zone) = local_now(Some("Europe/Berlin"), tuesday());
        assert_eq!(time, "Tuesday 17 November 2026, 15:03");
        assert_eq!(zone, "Europe/Berlin");
    }

    #[test]
    fn a_named_fixed_offset_zone() {
        if !zone_present("Etc/GMT-2") {
            eprintln!("skipping: no tzdb");
            return;
        }
        let (time, zone) = local_now(Some("Etc/GMT-2"), tuesday());
        assert_eq!(time, "Tuesday 17 November 2026, 16:03");
        assert_eq!(zone, "Etc/GMT-2");
    }

    #[test]
    fn an_unknown_zone_is_utc() {
        let (time, zone) = local_now(Some("Mars/Olympus"), tuesday());
        assert_eq!(time, "Tuesday 17 November 2026, 14:03");
        assert_eq!(zone, "UTC");
    }

    #[test]
    fn a_time_before_the_epoch_is_shown_as_it_is() {
        let (time, zone) = local_now(None, UNIX_EPOCH - Duration::from_secs(60));
        assert_eq!(time, "Wednesday 31 December 1969, 23:59");
        assert_eq!(zone, "UTC");
    }
}
