//! pinna-init: PID 1 of the initramfs in Pinna's boot image.
//!
//! 1. mount devtmpfs, proc, sysfs, and a tmpfs on /run
//! 2. find `misc`, `system_a`, `system_b` by GPT name (PARTNAME in sysfs)
//! 3. pick a slot from the A/B state in misc (trial tries, rollback)
//! 4. check that slot's signed footer with the compiled-in release key; on
//!    failure use the other slot
//! 5. dm-verity device `root` over the slot, erofs read-only on /sysroot
//! 5. dm-verity device `root` over the slot, erofs read-only on /sysroot
//! 6. /run/pinna/boot-slot + boot-version + the root's boot protocol marker,
//!    bind-mount a machine id from the idme serial over /etc/machine-id, move
//!    /dev /proc /sys /run, switch root, exec /sbin/init with our argv
//!
//! Everything is logged to /dev/kmsg as `pinna-init: …`, and to pstore's
//! /dev/pmsg0 so that it survives a warm reset. The LED ring shows boot
//! stages 1-3 (ADR 0012). If there is nothing to boot, pinna-init stays up in
//! rescue mode ([`rescue`]) instead of rebooting.

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use pinna_bootctl::slot::{self, Slot, SlotState};
use pinna_bootctl::stages;
use pinna_image::{Footer, keys};

use crate::rescue::{Failure, Reason};
use crate::ring::Ring;

const SYSROOT: &str = "/sysroot";
const DM_NAME: &str = "root";
const PART_TIMEOUT: Duration = Duration::from_secs(15);

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::kmsg(&format!($($arg)*)) };
}

mod dm;
mod http;
mod machine_id;
mod rescue;
mod ring;

/// Everything logged so far, for the rescue page (bounded).
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
const LOG_LINES: usize = 2000;

/// Seconds since boot, as the kernel log prints them.
fn boottime() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Append a line to pstore's message log: it survives a warm reset and is
/// read back after the next boot.
pub fn pmsg(line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open("/dev/pmsg0") {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// pinna-init's log lines so far, with kernel timestamps.
pub fn log_lines() -> Vec<String> {
    LOG.lock().map(|l| l.clone()).unwrap_or_default()
}

/// One record per write; a fresh open each time avoids the per-open
/// ratelimit on /dev/kmsg. Mirrored to pstore (the kernel command line's
/// loglevel=4 keeps these level 5 lines out of console-ramoops) and kept for
/// the rescue page.
pub fn kmsg(msg: &str) {
    let line = format!("pinna-init: {msg}");
    match fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        Ok(mut f) => {
            let _ = f.write_all(format!("<5>{line}\n").as_bytes());
        }
        Err(_) => eprintln!("{line}"),
    }
    pmsg(&line);
    if let Ok(mut l) = LOG.lock() {
        if l.len() >= LOG_LINES {
            l.remove(0);
        }
        l.push(format!("[{:12.6}] {line}", boottime()));
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("no NUL in paths")
}

pub fn mount(
    src: &str,
    target: &str,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<()> {
    let (s, t) = (cstr(src), cstr(target));
    let fstype = fstype.map(cstr);
    let data = data.map(cstr);
    // SAFETY: valid NUL-terminated strings or NULL.
    let r = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            fstype.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            flags,
            data.as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr().cast()),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("mount {src} on {target}"));
    }
    Ok(())
}

pub fn early_mounts() -> Result<()> {
    let nosuid = libc::MS_NOSUID;
    let api = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    for (src, target, fstype, flags, data) in [
        ("devtmpfs", "/dev", "devtmpfs", nosuid, Some("mode=0755")),
        ("proc", "/proc", "proc", api, None),
        ("sysfs", "/sys", "sysfs", api, None),
        (
            "tmpfs",
            "/run",
            "tmpfs",
            libc::MS_NOSUID | libc::MS_NODEV,
            Some("mode=0755"),
        ),
    ] {
        let _ = fs::create_dir_all(target);
        match mount(src, target, Some(fstype), flags, data) {
            Err(e)
                if e.downcast_ref::<io::Error>().and_then(|e| e.raw_os_error())
                    == Some(libc::EBUSY) => {}
            r => r?,
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Part {
    devname: String,
    dev: (u32, u32),
}

impl Part {
    fn path(&self) -> PathBuf {
        Path::new("/dev").join(&self.devname)
    }

    /// The device node (devtmpfs creates it; make it if it lags behind sysfs).
    fn open(&self, write: bool) -> Result<File> {
        let p = self.path();
        if !p.exists() {
            let c = cstr(p.to_str().unwrap());
            // SAFETY: plain mknod.
            unsafe {
                libc::mknod(
                    c.as_ptr(),
                    libc::S_IFBLK | 0o600,
                    libc::makedev(self.dev.0, self.dev.1),
                );
            }
        }
        fs::OpenOptions::new()
            .read(true)
            .write(write)
            .open(&p)
            .with_context(|| format!("opening {}", p.display()))
    }
}

fn uevent(path: &Path) -> HashMap<String, String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Block devices by GPT partition name.
fn scan_parts() -> HashMap<String, Part> {
    let mut parts = HashMap::new();
    let Ok(dir) = fs::read_dir("/sys/class/block") else {
        return parts;
    };
    for e in dir.flatten() {
        let u = uevent(&e.path().join("uevent"));
        let (Some(name), Some(devname), Some(maj), Some(min)) = (
            u.get("PARTNAME"),
            u.get("DEVNAME"),
            u.get("MAJOR"),
            u.get("MINOR"),
        ) else {
            continue;
        };
        let (Ok(maj), Ok(min)) = (maj.parse(), min.parse()) else {
            continue;
        };
        parts.entry(name.clone()).or_insert(Part {
            devname: devname.clone(),
            dev: (maj, min),
        });
    }
    parts
}

fn wait_for_parts(names: &[&str]) -> HashMap<String, Part> {
    let start = Instant::now();
    loop {
        let parts = scan_parts();
        if names.iter().all(|n| parts.contains_key(*n)) || start.elapsed() > PART_TIMEOUT {
            for n in names.iter().filter(|n| !parts.contains_key(**n)) {
                log!("no partition named {n} after {}s", PART_TIMEOUT.as_secs());
            }
            return parts;
        }
        sleep(Duration::from_millis(100));
    }
}

/// The A/B slot state in misc, for the rescue diagnostics.
pub fn slot_summary() -> String {
    let parts = scan_parts();
    let Some(misc) = parts.get("misc") else {
        return "no misc partition\n".into();
    };
    match slot::read_state(&misc.path()) {
        Ok(Some(st)) => format!(
            "active: {}\na: {}\nb: {}\n",
            st.active,
            st.describe(Slot::A),
            st.describe(Slot::B)
        ),
        Ok(None) => "no slot state (first flash: slot a counts as good)\n".into(),
        Err(e) => format!("unreadable: {e:#}\n"),
    }
}

fn persist(misc: Option<&Part>, state: &SlotState) {
    let res = misc
        .context("no misc partition")
        .and_then(|m| slot::write_state(&m.path(), state));
    if let Err(e) = res {
        log!("WARNING: could not record the slot state: {e:#}");
    }
}

/// Check the slot's image and bring it up on /sysroot.
fn try_slot(part: &Part) -> Result<Footer> {
    let dev = part.open(false)?;
    let footer = pinna_image::read_footer(&dev, &keys::release_key())?;
    pinna_image::check_top_hash(&dev, &footer)?;
    drop(dev);
    let dm_dev = dm::create_verity(DM_NAME, part.dev, &footer)?;
    let res = (|| {
        let u = uevent(Path::new(&format!(
            "/sys/dev/block/{}:{}/uevent",
            dm_dev.0, dm_dev.1
        )));
        let dm = Part {
            devname: u
                .get("DEVNAME")
                .cloned()
                .unwrap_or_else(|| format!("dm-{}", dm_dev.1)),
            dev: dm_dev,
        };
        drop(dm.open(false)?);
        let _ = fs::create_dir_all(SYSROOT);
        mount(
            dm.path().to_str().unwrap(),
            SYSROOT,
            Some("erofs"),
            libc::MS_RDONLY,
            None,
        )?;
        for d in ["dev", "proc", "sys", "run"] {
            if !Path::new(SYSROOT).join(d).is_dir() {
                // SAFETY: plain umount.
                unsafe { libc::umount(cstr(SYSROOT).as_ptr()) };
                bail!("image has no /{d}");
            }
        }
        if !Path::new(SYSROOT).join("sbin/init").exists() {
            // SAFETY: plain umount.
            unsafe { libc::umount(cstr(SYSROOT).as_ptr()) };
            bail!("image has no /sbin/init");
        }
        Ok(dm.devname)
    })();
    match res {
        Ok(name) => {
            log!(
                "slot {} -> {name} (dm-verity, root hash {}) mounted on {SYSROOT}",
                part.devname,
                keys::hex(&footer.root_hash)
            );
            Ok(footer)
        }
        Err(e) => {
            let _ = dm::remove(DM_NAME);
            Err(e)
        }
    }
}

/// Choose, check and mount a slot. Returns what booted.
fn bring_up_root(ring: &Ring) -> Result<(Slot, Footer), Failure> {
    let parts = wait_for_parts(&["misc", Slot::A.partname(), Slot::B.partname()]);
    if !parts.contains_key(Slot::A.partname()) && !parts.contains_key(Slot::B.partname()) {
        return Err(Failure::new(
            Reason::NoStorage,
            anyhow!(
                "no {} or {} partition after {} s",
                Slot::A.partname(),
                Slot::B.partname(),
                PART_TIMEOUT.as_secs()
            ),
        ));
    }
    ring.done(stages::STORAGE);
    let misc = parts.get("misc");
    let state = match misc.map(|m| slot::read_state(&m.path())) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            log!("cannot read the slot state ({e:#}); using slot a");
            None
        }
        None => None,
    };
    let d = slot::decide(state);
    log!("{}", d.reason);
    if let Some(p) = &d.persist {
        persist(misc, p);
    }
    let state = d.persist.or(state);
    let mut why = Vec::new();
    for s in [d.slot, d.slot.other()] {
        let Some(part) = parts.get(s.partname()) else {
            log!("slot {s}: no {} partition", s.partname());
            why.push(format!("slot {s}: no {} partition", s.partname()));
            continue;
        };
        match try_slot(part) {
            Ok(footer) => {
                if s != d.slot {
                    log!(
                        "falling back to slot {s}; slot {} marked unbootable",
                        d.slot
                    );
                    persist(misc, &slot::after_bad_image(state, d.slot));
                }
                ring.done(stages::ROOT);
                return Ok((s, footer));
            }
            Err(e) => {
                log!("slot {s} ({}): not bootable: {e:#}", part.devname);
                why.push(format!("slot {s}: {e:#}"));
            }
        }
    }
    Err(Failure::new(
        Reason::NoRoot,
        anyhow!("no bootable slot ({})", why.join("; ")),
    ))
}

fn switch_root(argv: &[OsString]) -> Result<()> {
    for m in ["/dev", "/proc", "/sys", "/run"] {
        mount(m, &format!("{SYSROOT}{m}"), None, libc::MS_MOVE, None)?;
    }
    std::env::set_current_dir(SYSROOT)?;
    // The initramfs stays in memory unless emptied: /init and busybox (for
    // rescue mode) are all there is.
    let _ = fs::remove_file("/init");
    rescue::release_busybox();
    mount(".", "/", None, libc::MS_MOVE, None)?;
    // SAFETY: plain chroot/chdir on valid paths.
    ensure!(
        unsafe { libc::chroot(cstr(".").as_ptr()) } == 0,
        "chroot: {}",
        io::Error::last_os_error()
    );
    std::env::set_current_dir("/")?;
    let mut args: Vec<CString> = vec![cstr("/sbin/init")];
    args.extend(
        argv.iter()
            .skip(1)
            .map(|a| CString::new(a.clone().into_vec()).unwrap()),
    );
    let mut ptrs: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    // SAFETY: NUL-terminated argv; only returns on failure.
    unsafe { libc::execv(args[0].as_ptr(), ptrs.as_ptr()) };
    bail!("exec /sbin/init: {}", io::Error::last_os_error())
}

/// What the root says it can do (ADR 0012): `/usr/lib/pinna/boot-protocol`
/// holds the version of the boot protocol it speaks — it runs the stage helper
/// and (later) resets the boot counter. A root without the marker is neither
/// counted nor timed: it may be older than the boot image after a rollback.
fn root_protocol(sysroot: &Path) -> Option<u8> {
    fs::read_to_string(sysroot.join("usr/lib/pinna/boot-protocol"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Boot the root. Returns only when that is not possible.
fn boot(argv: &[OsString], ring: &Ring) -> Failure {
    let (s, footer) = match bring_up_root(ring) {
        Ok(booted) => booted,
        Err(f) => return f,
    };
    let protocol = root_protocol(Path::new(SYSROOT));
    let recorded = (|| -> Result<()> {
        fs::create_dir_all(Path::new(slot::RUN_DIR))?;
        fs::write(Path::new(slot::RUN_DIR).join("boot-slot"), format!("{s}\n"))?;
        fs::write(
            Path::new(slot::RUN_DIR).join("boot-version"),
            format!("{}\n", footer.version),
        )?;
        // Best effort: the stage helper only reads it, and its absence means
        // "no stages 4-6", not a failed boot.
        if protocol == Some(1) {
            let _ = fs::write(Path::new(slot::RUN_DIR).join("boot-protocol"), "1\n");
        }
        Ok(())
    })();
    if let Err(e) = recorded {
        return Failure::new(Reason::NoSystem, e.context("recording the booted slot"));
    }
    match protocol {
        Some(1) => log!("root speaks boot protocol 1: the stage helper shows stages 4-6"),
        Some(v) => log!("root speaks unknown boot protocol {v}; ignoring its marker"),
        None => log!("root has no boot protocol marker: no stages 4-6"),
    }
    match machine_id::apply(Path::new(SYSROOT), Path::new(slot::RUN_DIR)) {
        Ok(Some(id)) => log!("machine id {id}, from the device's serial number"),
        Ok(None) => log!("no serial number; keeping the image's machine id"),
        Err(e) => log!("machine id: {e:#}; keeping the image's"),
    }
    log!("booting slot {s}, version {}", footer.version);
    // Stage 3: the system starts. The still frame stays on the ring until
    // the root takes it over.
    ring.hold(stages::SYSTEM);
    let e = match switch_root(argv) {
        Ok(()) => anyhow!("switch root returned"),
        Err(e) => e,
    };
    Failure::new(Reason::NoSystem, e.context("switching to the root"))
}

fn main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    std::panic::set_hook(Box::new(|info| log!("panic: {info}")));
    if let Err(e) = early_mounts() {
        log!("early mounts: {e:#}");
    }
    let ring = Ring::start();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boot(&argv, &ring)))
        .unwrap_or_else(|_| {
            Failure::new(
                Reason::Crashed(ring.in_progress()),
                anyhow!("pinna-init panicked (the panic is in its log)"),
            )
        });
    rescue::run(failure, &ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_protocol_comes_from_its_marker() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(root_protocol(d.path()), None);
        fs::create_dir_all(d.path().join("usr/lib/pinna")).unwrap();
        fs::write(d.path().join("usr/lib/pinna/boot-protocol"), "1\n").unwrap();
        assert_eq!(root_protocol(d.path()), Some(1));
        fs::write(d.path().join("usr/lib/pinna/boot-protocol"), "2").unwrap();
        assert_eq!(root_protocol(d.path()), Some(2));
        fs::write(d.path().join("usr/lib/pinna/boot-protocol"), "soon").unwrap();
        assert_eq!(root_protocol(d.path()), None);
    }
}
