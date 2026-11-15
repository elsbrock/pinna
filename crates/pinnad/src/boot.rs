//! The boot stages after pinna-init (ADR 0012), in pinnad.
//!
//! The stage helper drives the ring through stage 5 and writes what it reached
//! to `/run/pinna/boot-stages`. pinnad claims the ring when its control API
//! answers (stage 6), continues the arc for any stage still pending — the
//! helper may have exited before a late deadline — and fills the ring when
//! `pinna-bootctl mark-good` records the health verdict (stage 7), then fades
//! to idle. A stage that missed its deadline keeps flashing while the device
//! is idle, until the next boot, as the contract says.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pinna_bootctl::slot::RUN_DIR;
use pinna_bootctl::{board, stages};
use pinna_hal::led::{Frame, Rgb};

const HEALTHY_FADE: Duration = Duration::from_millis(1200);
/// Where the pstore archive wrapper records the previous boot's directory.
const PREVIOUS_PSTORE: &str = "previous-pstore";
/// How much of each pstore record to read; a record is a handful of KiB.
const MAX_RECORD: usize = 1 << 20;

/// How the previous boot ended (ADR 0012), from the pstore records that
/// `systemd-pstore` archived (`pstore-archive` keeps one directory per boot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No records: a cold start, or the power was cut.
    Cold,
    /// A `pinna: reboot` marker: a clean restart, with the reason when one was
    /// recorded.
    Clean,
    /// A `pinna: rescue` marker: the boot ended in rescue mode.
    Rescue,
    /// An oops or panic in the kernel log.
    Crashed,
    /// Records without either marker: reset without shutting down.
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastBoot {
    pub outcome: Outcome,
    /// The recorded reason, where there is one: "update", "user", "no-root".
    pub reason: Option<String>,
}

impl LastBoot {
    /// The verdict when there are no records at all: a cold start, or the
    /// power was cut.
    pub fn cold() -> Self {
        LastBoot {
            outcome: Outcome::Cold,
            reason: None,
        }
    }

    /// Read the records the previous boot left behind.
    pub fn read(run_dir: &Path) -> Self {
        let dir = fs::read_to_string(run_dir.join(PREVIOUS_PSTORE))
            .ok()
            .map(|p| PathBuf::from(p.trim()))
            .filter(|p| p.is_dir());
        let Some(dir) = dir else {
            return Self::cold();
        };
        let (mut dmesg, mut pmsg) = (String::new(), String::new());
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // The kernel log and the console both carry a panic or oops; the
            // message log carries pinna-init's and Pinna's own lines.
            let kernel_log = name.starts_with("dmesg") || name.starts_with("console");
            if !(kernel_log || name.starts_with("pmsg")) {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path()) else {
                continue;
            };
            let buf = if kernel_log { &mut dmesg } else { &mut pmsg };
            buf.extend(text.chars().take(MAX_RECORD));
        }
        classify(&dmesg, &pmsg)
    }

    /// One line for people, as the dashboard and `pinnactl status` show it.
    pub fn summary(&self) -> String {
        match (self.outcome, self.reason.as_deref()) {
            (Outcome::Cold, _) => "no records: a cold start, or the power was lost".into(),
            (Outcome::Clean, Some(r)) => format!("restarted ({r})"),
            (Outcome::Clean, None) => "restarted".into(),
            (Outcome::Rescue, Some(r)) => format!("rescue mode ({r})"),
            (Outcome::Rescue, None) => "rescue mode".into(),
            (Outcome::Crashed, _) => "crashed: a kernel panic or oops".into(),
            (Outcome::Reset, _) => "reset without shutting down".into(),
        }
    }
}

/// The most recent line like `pinna: reboot (update)`; the reason is empty for
/// `(not recorded)`.
fn marker(text: &str, prefix: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let reason = line
            .trim()
            .strip_prefix(prefix)?
            .trim_end_matches(')')
            .trim();
        Some(reason.to_string())
    })
}

fn classify(dmesg: &str, pmsg: &str) -> LastBoot {
    let last = |outcome, reason| LastBoot { outcome, reason };
    let reason = |r: String| (!r.is_empty()).then_some(r);
    if dmesg.contains("Kernel panic") || dmesg.contains("Oops") {
        return last(Outcome::Crashed, None);
    }
    // Rescue first: a reboot out of rescue mode keeps the more useful marker.
    if let Some(r) = marker(pmsg, "pinna: rescue (") {
        return last(Outcome::Rescue, reason(r));
    }
    if let Some(r) = marker(pmsg, "pinna: reboot (") {
        return last(Outcome::Clean, reason(r));
    }
    if !dmesg.trim().is_empty() || !pmsg.trim().is_empty() {
        return last(Outcome::Reset, None);
    }
    last(Outcome::Cold, None)
}

fn to_led(frame: stages::Frame) -> Frame {
    frame.map(|[r, g, b]| Rgb::new(r, g, b))
}

/// Seconds since boot, as the deadlines are measured.
fn boottime() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// Where the health check records that this boot passed.
const HEALTHY_FILE: &str = "boot-ok";

/// What the stage helper handed over, if one ran: without it (an older root,
/// or a boot that did not go through pinna-init) pinnad keeps its own startup
/// effect and shows no stages.
pub struct BootRing {
    run_dir: PathBuf,
    progress: stages::Progress,
    skips: board::Skips,
    /// When the arc or the code last changed, for breathing and flashing.
    since: Instant,
    /// When the health verdict arrived, for the full-ring fade.
    healthy: Option<Instant>,
    /// When this process started, and how far the boot had got then: the
    /// stage deadlines are measured from boot, not from pinnad's start.
    started: Instant,
    boot_at_start: Duration,
}

impl BootRing {
    /// Take the ring over from the stage helper: read its handover and write
    /// the marker that stops it. `None` when there is no handover.
    pub fn claim() -> Option<Self> {
        Self::claim_in(Path::new(RUN_DIR))
    }

    fn claim_in(run_dir: &Path) -> Option<Self> {
        let text = fs::read_to_string(run_dir.join("boot-stages")).ok()?;
        let _ = fs::create_dir_all(run_dir);
        if let Err(e) = fs::write(run_dir.join("ring"), "pinnad\n") {
            tracing::warn!("could not claim the ring from the stage helper: {e}");
        }
        Some(BootRing {
            run_dir: run_dir.to_path_buf(),
            progress: stages::Progress::parse(&text),
            skips: board::Skips::detect(),
            since: Instant::now(),
            healthy: None,
            started: Instant::now(),
            boot_at_start: boottime(),
        })
    }

    /// The frame for this LED tick: the arc, a failed stage's code, the
    /// healthy full ring fading out, or nothing (the ring is idle).
    pub fn frame(&mut self, now: Instant) -> Option<Frame> {
        let ok = self.run_dir.join(HEALTHY_FILE).exists();
        if ok && self.healthy.is_none() {
            self.healthy = Some(now);
        }
        let uptime = self.boot_at_start + now.saturating_duration_since(self.started);
        let before = self.progress;
        self.progress.update(stages::HEALTHY, uptime, |n| match n {
            stages::USB => self.skips.usb || board::usb_bound(Path::new(board::GADGET_DIR)),
            stages::WIFI_DRIVER => self.skips.wifi || board::wlan_up(Path::new(board::NET_DIR)),
            // Stage 6 is "pinnad answers": this is pinnad.
            stages::PINNA => true,
            stages::HEALTHY => ok,
            _ => false,
        });
        if self.progress != before {
            self.since = now;
        }

        if let Some(at) = self.healthy {
            let t = now.saturating_duration_since(at);
            if t < HEALTHY_FADE {
                let k = 1.0 - t.as_secs_f32() / HEALTHY_FADE.as_secs_f32();
                let full = stages::progress(stages::STAGES, Duration::ZERO);
                return Some(to_led(stages::scale_frame(&full, k)));
            }
        }
        if let Some(n) = self.progress.failed {
            return Some(to_led(stages::code(n, self.since.elapsed())));
        }
        if self.healthy.is_some() {
            // Healthy and faded: the idle ring (or the light, timers, ...).
            return None;
        }
        Some(to_led(stages::progress(
            self.progress.done,
            self.since.elapsed(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claimed(dir: &Path, state: &str, skips: board::Skips) -> BootRing {
        fs::write(dir.join("boot-stages"), state).unwrap();
        let mut boot = BootRing::claim_in(dir).unwrap();
        boot.skips = skips;
        boot.boot_at_start = Duration::from_secs(12);
        boot.started = Instant::now();
        boot
    }

    fn skips() -> board::Skips {
        board::Skips {
            usb: true,
            wifi: true,
        }
    }

    #[test]
    fn claim_reads_the_handover_and_stops_the_helper() {
        let d = tempfile::tempdir().unwrap();
        assert!(BootRing::claim_in(d.path()).is_none());
        let boot = claimed(d.path(), "stage 5\nfailed 4\n", skips());
        assert_eq!(
            boot.progress,
            stages::Progress {
                done: 5,
                failed: Some(4)
            }
        );
        assert!(d.path().join("ring").exists());
    }

    #[test]
    fn the_arc_ends_at_stage_six_until_the_health_verdict() {
        let d = tempfile::tempdir().unwrap();
        let mut boot = claimed(d.path(), "stage 5\n", skips());
        // USB and Wi-Fi are skipped here, so the arc reaches stage 6: pinnad
        // itself, which answers from this point on.
        let arc = boot.frame(Instant::now()).unwrap();
        assert_eq!(boot.progress.done, stages::PINNA);
        // The solid part of the arc starts at the Action button; the leading
        // segment of the pending stage 7 breathes at its dimmest right now.
        assert_eq!(arc[5], Rgb::new(0, 0x30, 0xff), "the solid arc");
        let edge = arc[4];
        assert!(edge.b > 0 && edge.b < 0xff, "pending stage 7: {edge:?}");

        // mark-good records the verdict: the full ring fades to idle.
        fs::write(d.path().join("boot-ok"), "").unwrap();
        let at = Instant::now();
        let full = boot.frame(at).unwrap();
        assert!(full.iter().all(|p| p.b > 0xf0), "{full:?}");
        let fading = boot.frame(at + HEALTHY_FADE / 2).unwrap();
        assert!(fading[0].b > 0 && fading[0].b < full[0].b, "{fading:?}");
        assert_eq!(
            boot.frame(at + HEALTHY_FADE + Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn a_failed_stage_keeps_flashing_while_idle() {
        let d = tempfile::tempdir().unwrap();
        let mut boot = claimed(d.path(), "stage 4\nfailed 4\n", skips());
        let white = to_led(stages::code(4, Duration::ZERO));
        assert_eq!(boot.frame(Instant::now()), Some(white));
        // Health does not clear the code: it shows again after the fade.
        fs::write(d.path().join("boot-ok"), "").unwrap();
        let now = Instant::now();
        assert_ne!(boot.frame(now), Some(white));
        assert_eq!(
            boot.frame(now + HEALTHY_FADE + Duration::from_millis(1)),
            Some(white)
        );
    }

    #[test]
    fn a_late_stage_records_its_deadline_here_too() {
        let d = tempfile::tempdir().unwrap();
        // The helper handed over with USB still pending and no failure yet.
        let mut boot = claimed(
            d.path(),
            "stage 3\n",
            board::Skips {
                usb: false,
                wifi: true,
            },
        );
        // No gadget can bind in the test, so the code is recorded once the
        // stage's deadline (30 s since boot) has passed.
        boot.boot_at_start = Duration::from_secs(31);
        let f = boot.frame(Instant::now());
        assert_eq!(boot.progress.failed, Some(stages::USB));
        assert_eq!(f, Some(to_led(stages::code(stages::USB, Duration::ZERO))));
    }

    #[test]
    fn the_previous_boot_is_read_from_its_archive() {
        let d = tempfile::tempdir().unwrap();
        // No archive: a cold start, or the power was cut.
        let cold = LastBoot::read(d.path());
        assert_eq!(cold.outcome, Outcome::Cold);
        assert!(cold.summary().contains("cold start"));

        let archive = d.path().join("archive");
        fs::create_dir(&archive).unwrap();
        fs::write(
            d.path().join(PREVIOUS_PSTORE),
            format!("{}\n", archive.display()),
        )
        .unwrap();
        // Records without a marker: reset without shutting down.
        fs::write(archive.join("dmesg-ramoops-0"), "[    0.1] something\n").unwrap();
        fs::write(archive.join("pmsg-ramoops-0"), "pinna-init: up\n").unwrap();
        assert_eq!(
            LastBoot::read(d.path()),
            LastBoot {
                outcome: Outcome::Reset,
                reason: None
            }
        );

        // A reboot marker, with its reason.
        fs::write(
            archive.join("pmsg-ramoops-0"),
            "pinna-init: up\npinna: reboot (update)\n",
        )
        .unwrap();
        assert_eq!(LastBoot::read(d.path()).summary(), "restarted (update)");

        // Rescue is the more useful marker when a reboot left rescue mode.
        fs::write(
            archive.join("pmsg-ramoops-0"),
            "pinna: rescue (no-root)\npinna: reboot (not recorded)\n",
        )
        .unwrap();
        let rescued = LastBoot::read(d.path());
        assert_eq!(rescued.outcome, Outcome::Rescue);
        assert_eq!(rescued.reason.as_deref(), Some("no-root"));

        // A panic in the kernel log wins over the markers; the console record
        // carries it too.
        fs::write(
            archive.join("dmesg-ramoops-0"),
            "[    0.1] Kernel panic - not syncing: fatal exception\n",
        )
        .unwrap();
        assert_eq!(LastBoot::read(d.path()).outcome, Outcome::Crashed);
        fs::write(
            archive.join("dmesg-ramoops-0"),
            "[    0.1] Internal error: Oops\n",
        )
        .unwrap();
        assert_eq!(LastBoot::read(d.path()).outcome, Outcome::Crashed);
        fs::write(archive.join("dmesg-ramoops-0"), "[    0.1] fine\n").unwrap();
        fs::write(
            archive.join("console-ramoops-0"),
            "[    0.2] Kernel panic - not syncing: \n",
        )
        .unwrap();
        assert_eq!(LastBoot::read(d.path()).outcome, Outcome::Crashed);
    }
}
