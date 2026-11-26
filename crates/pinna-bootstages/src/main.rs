//! The stage helper (ADR 0012): the root's half of the boot stages.
//!
//! pinna-init holds the ring after stage 3 (storage, root, system). This takes
//! it over in sysinit and drives stages 4–6 — USB ready, Wi-Fi driver up,
//! Pinna running — each with its deadline; a stage that misses one flashes its
//! number. It hands the progress to pinnad through `/run/pinna/boot-stages`
//! and exits once pinnad claims the ring (`/run/pinna/ring`), which is what
//! stage 6 waits for; pinnad then ends the arc with stage 7.
//!
//! `pinna-init` records the root's `/usr/lib/pinna/boot-protocol` marker as
//! `/run/pinna/boot-protocol`. Without it — an older root, e.g. after a
//! rollback — the stages are neither counted nor timed, and the helper leaves
//! the ring to that root's pinnad.

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use pinna_bootctl::slot::RUN_DIR;
use pinna_bootctl::{board, ring, stages};

const TICK: Duration = Duration::from_millis(100);
const RESCAN: Duration = Duration::from_secs(1);

fn log(msg: &str) {
    println!("pinna-bootstages: {msg}");
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

/// pinnad claimed the ring: it runs and drives the stages from here on.
fn claimed() -> bool {
    Path::new(RUN_DIR).join("ring").exists()
}

fn write_state(progress: &stages::Progress) {
    if let Err(e) = fs::write(Path::new(RUN_DIR).join("boot-stages"), progress.encode()) {
        log(&format!("cannot write boot-stages: {e}"));
    }
}

/// Floor `n` to 0.1 s, for readable timings.
fn secs(d: Duration) -> f32 {
    (d.as_millis() / 100) as f32 / 10.0
}

fn main() {
    if !Path::new(RUN_DIR).join("boot-protocol").exists() {
        log("no boot protocol recorded by pinna-init; not driving the ring");
        return;
    }
    let skips = board::Skips::detect();
    if skips.usb {
        log("no UDC on this board: skipping the USB stage");
    }
    if skips.wifi {
        log("no known board: skipping the Wi-Fi stage");
    }
    log("up");

    let mut progress = stages::Progress::new(stages::SYSTEM);
    write_state(&progress);
    // When the arc or the code last changed, for the breathing and flashing.
    let mut since = Instant::now();
    let mut last: Option<stages::Frame> = None;
    let mut ring_path = ring::find(Path::new(ring::SYSFS_I2C_DIR));
    let mut looked = Some(Instant::now());

    loop {
        let mut next = progress;
        next.update(stages::PINNA, boottime(), |n| match n {
            stages::USB => skips.usb || board::usb_bound(Path::new(board::GADGET_DIR)),
            stages::WIFI_DRIVER => skips.wifi || board::wlan_up(Path::new(board::NET_DIR)),
            stages::PINNA => claimed(),
            _ => false,
        });
        if next != progress {
            if next.done != progress.done {
                log(&format!(
                    "stage {} ({}) at {:.1}s",
                    next.done,
                    stages::name(next.done),
                    secs(boottime())
                ));
            }
            if next.failed != progress.failed
                && let Some(n) = next.failed
            {
                log(&format!(
                    "stage {n} ({}) missed its deadline at {:.1}s; flashing {n}",
                    stages::name(n),
                    secs(boottime())
                ));
            }
            progress = next;
            since = Instant::now();
            write_state(&progress);
        }

        if ring_path.is_none() && looked.is_some_and(|l| l.elapsed() >= RESCAN) {
            ring_path = ring::find(Path::new(ring::SYSFS_I2C_DIR));
            looked = Some(Instant::now());
            last = None;
        }
        if let Some(path) = &ring_path {
            let frame = match progress.failed {
                Some(n) => stages::code(n, since.elapsed()),
                None => stages::progress(progress.done, since.elapsed()),
            };
            if last != Some(frame) {
                if ring::write(path, &frame).is_err() {
                    ring_path = None;
                }
                last = Some(frame);
            }
        }

        if claimed() {
            log(&format!(
                "pinnad claimed the ring at stage {} ({:.1}s); handing over",
                progress.done,
                secs(boottime())
            ));
            return;
        }
        sleep(TICK);
    }
}
