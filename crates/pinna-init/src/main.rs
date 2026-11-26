//! pinna-init: PID 1 of the initramfs in Pinna's boot image.
//!
//! 1. mount devtmpfs, proc, sysfs, and a tmpfs on /run
//! 2. find `misc`, `system_a`, `system_b` by GPT name (PARTNAME in sysfs)
//! 3. take the slot the bootloader started (Kaeru's slot record, ADR 0019);
//!    without a bootloader slot, choose as the preloader would
//! 4. check that slot's signed footer with the compiled-in release key; when
//!    its root can't start, retire the slot and restart into the other one
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
use pinna_board::{BootFormat, StorageKind};
use pinna_bootctl::bcb;
use pinna_bootctl::slot::{self, Slot};
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

mod boot_slot;
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

/// Whether a partition's device node is a block device (a GPT partition, a
/// dm-verity device) or a character device (a UBI volume).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Block,
    Char,
}

#[derive(Debug, Clone)]
struct Part {
    devname: String,
    dev: (u32, u32),
    node: NodeKind,
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
            let mode = match self.node {
                NodeKind::Block => libc::S_IFBLK,
                NodeKind::Char => libc::S_IFCHR,
            };
            // SAFETY: plain mknod.
            unsafe {
                libc::mknod(
                    c.as_ptr(),
                    mode | 0o600,
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

/// The partitions, found the way this board keeps them (ADR 0035): GPT
/// partitions on the MT8163/MT8516 Echos, UBI volumes on the MT8519 ones.
/// The board is read once; hardware Pinna doesn't know, and the VMs, have no
/// board and fall back to GPT, so existing devices are unchanged.
fn scan_parts() -> HashMap<String, Part> {
    match pinna_board::storage() {
        StorageKind::Gpt => scan_gpt(Path::new("/sys/class/block")),
        StorageKind::Ubi => scan_ubi(Path::new("/sys/class/ubi"), Path::new("/sys/class/block")),
    }
}

/// Block devices by GPT partition name (`PARTNAME` in sysfs).
fn scan_gpt(sys_block: &Path) -> HashMap<String, Part> {
    let mut parts = HashMap::new();
    let Ok(dir) = fs::read_dir(sys_block) else {
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
            node: NodeKind::Block,
        });
    }
    parts
}

/// UBI volumes by name. Each `ubiN_M` under `sys_ubi` has a `name` (the
/// volume name Pinna looks up) and a `dev` (`major:minor`); the node is the
/// volume's character device `/dev/ubiN_M`. When the kernel made a block
/// device for the volume (`ubi.block=` on the command line), `ubiblockN_M`
/// under `sys_block`, that is the node instead: dm-verity needs a block
/// device under the root, as the stock system mounts its root.
fn scan_ubi(sys_ubi: &Path, sys_block: &Path) -> HashMap<String, Part> {
    let mut parts = HashMap::new();
    let Ok(dir) = fs::read_dir(sys_ubi) else {
        return parts;
    };
    for e in dir.flatten() {
        let devname = e.file_name().to_string_lossy().into_owned();
        // A volume is `ubiN_M`; the device `ubiN` (no underscore) is not one.
        if !devname.contains('_') {
            continue;
        }
        let read = |f: &str| fs::read_to_string(e.path().join(f));
        let (Ok(name), Ok(dev)) = (read("name"), read("dev")) else {
            continue;
        };
        let name = name.trim().to_string();
        let Some((maj, min)) = dev.trim().split_once(':') else {
            continue;
        };
        let (Ok(maj), Ok(min)) = (maj.parse(), min.parse()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let block = format!("ubiblock{}", devname.trim_start_matches("ubi"));
        let part = match fs::read_to_string(sys_block.join(&block).join("dev"))
            .ok()
            .and_then(|d| {
                let (maj, min) = d.trim().split_once(':')?;
                Some((maj.parse().ok()?, min.parse().ok()?))
            }) {
            Some(dev) => Part {
                devname: block,
                dev,
                node: NodeKind::Block,
            },
            None => Part {
                devname,
                dev: (maj, min),
                node: NodeKind::Char,
            },
        };
        parts.entry(name).or_insert(part);
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

/// LK's command line, as it put it into the device tree; empty without one.
fn bootargs() -> String {
    fs::read("/proc/device-tree/chosen/bootargs")
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// The slot LK started; `None` without a bootloader (the VMs).
fn lk_slot() -> Option<Slot> {
    boot_slot::lk_slot_from(&bootargs())
}

/// Whether pinna-init counts the boot attempts: no bootloader (the VMs), or
/// one that doesn't count them (Amazon's U-Boot, ADR 0035).
fn counts_attempts() -> bool {
    let counts = pinna_board::detect().is_none_or(|b| b.bootloader.counts_tries());
    boot_slot::counts_attempts(no_bootloader(), counts)
}

/// The format of the boot images this board's bootloader reads.
fn boot_format() -> BootFormat {
    pinna_board::detect().map_or(BootFormat::Android, |b| b.bootloader.boot_format())
}

/// A machine without a bootloader slot says so on the kernel command line.
fn no_bootloader() -> bool {
    fs::read_to_string("/proc/cmdline")
        .is_ok_and(|c| c.split_whitespace().any(|a| a == boot_slot::NO_BOOTLOADER))
}

/// The slot record in misc, for the rescue diagnostics.
pub fn slot_summary() -> String {
    let parts = scan_parts();
    let Some(misc) = parts.get("misc") else {
        return "no misc partition\n".into();
    };
    let started = match lk_slot() {
        Some(s) => format!("the bootloader started slot {s}\n"),
        None => "no bootloader slot\n".into(),
    };
    let record = match bcb::read(&misc.path()) {
        Ok(Some(r)) => format!(
            "next boot: {}\na: {}\nb: {}\n",
            r.next_boot()
                .map_or_else(|| "none".to_owned(), |s| s.to_string()),
            r.describe(Slot::A),
            r.describe(Slot::B)
        ),
        Ok(None) => "no slot record (the bootloader writes one: slot a, good)\n".into(),
        Err(e) => format!("unreadable: {e:#}\n"),
    };
    format!("{started}{record}")
}

fn write_record(misc: Option<&Part>, r: &bcb::Record) -> Result<()> {
    bcb::write(&misc.context("no misc partition")?.path(), r)
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
            node: NodeKind::Block,
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

/// Choose, check and mount a slot. Returns what booted. A slot whose root
/// can't start is retired, and the device restarts into the other slot's
/// kernel and root; when there is no good slot to go to, it's a failure.
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
    let record = match misc.map(|m| bcb::read(&m.path())) {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            log!("cannot read the slot record ({e:#})");
            None
        }
        None => None,
    };
    let plan = boot_slot::plan(lk_slot(), counts_attempts(), record);
    log!("{}", plan.reason);
    if let Some(w) = &plan.write
        && let Err(e) = write_record(misc, w)
    {
        log!("WARNING: could not write the slot record: {e:#}");
    }
    if plan.restart {
        // The kernel is the started slot's: only a restart loads the other.
        log!(
            "the kernel is slot {}'s but the count chose slot {}; restarting",
            plan.slot.other(),
            plan.slot
        );
        pmsg(&format!(
            "pinna: reboot (slot {} is out of attempts, back to slot {})",
            plan.slot.other(),
            plan.slot
        ));
        rescue::reboot_now();
    }

    let s = plan.slot;
    let res = match parts.get(s.partname()) {
        Some(part) => try_slot(part).inspect_err(|e| {
            log!("slot {s} ({}): not bootable: {e:#}", part.devname);
        }),
        None => Err(anyhow!("no {} partition", s.partname())),
    };
    match res {
        Ok(footer) => {
            ring.done(stages::ROOT);
            Ok((s, footer))
        }
        Err(e) => {
            // The other slot's kernel and root start together: retire this
            // slot and let the preloader take the other one.
            let retired = bcb::retire(plan.record, s).and_then(|r| write_record(misc, &r));
            match retired {
                Ok(()) => {
                    log!("slot {s} retired; restarting into slot {}", s.other());
                    // Why, for the next boot's report (ADR 0012); the
                    // shutdown hook that usually writes it doesn't run here.
                    pmsg(&format!(
                        "pinna: reboot (slot {s} didn't start, back to slot {})",
                        s.other()
                    ));
                    rescue::reboot_now()
                }
                Err(why) => Err(Failure::new(
                    Reason::NoRoot,
                    anyhow!("slot {s}: {e:#}; no other slot to go to: {why:#}"),
                )),
            }
        }
    }
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
    // Which boot partition LK really loaded (ADR 0019): the updater writes
    // boot_a only once a boot from b has shown LK loads boot_b too.
    let kernel = boot_slot::kernel_slot(&bootargs(), boot_format());
    match kernel {
        Some(k) if k != s => log!(
            "the bootloader started slot {s} but loaded boot_{k}: it ignores the slot for the kernel"
        ),
        Some(_) => {}
        None => log!("the boot image doesn't say which boot partition it is in"),
    }
    let recorded = (|| -> Result<()> {
        fs::create_dir_all(Path::new(slot::RUN_DIR))?;
        fs::write(Path::new(slot::RUN_DIR).join("boot-slot"), format!("{s}\n"))?;
        if let Some(k) = kernel {
            fs::write(
                Path::new(slot::RUN_DIR).join(slot::BOOT_KERNEL),
                format!("{k}\n"),
            )?;
        }
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
    leave_a_failed_trial(&failure);
    rescue::run(failure, &ring)
}

/// A slot on trial that fails, for any reason, goes back to the good slot:
/// it is retired, and the device restarts into the other slot's kernel and
/// root (ADR 0019). Returns when there is nothing to go back to, or the slot
/// record can't be read or written (no storage): then rescue mode.
fn leave_a_failed_trial(failure: &Failure) {
    let parts = scan_parts();
    let Some(misc) = parts.get("misc") else {
        return;
    };
    let Ok(Some(record)) = bcb::read(&misc.path()) else {
        return;
    };
    // Without a bootloader slot (the VMs), the slot pinna-init chose.
    let Some(s) = lk_slot().or_else(|| record.lk_choice()) else {
        return;
    };
    if record.get(s).good {
        return;
    }
    let Ok(retired) = bcb::retire(record, s) else {
        return;
    };
    if let Err(e) = bcb::write(&misc.path(), &retired) {
        log!("could not retire slot {s}: {e:#}");
        return;
    }
    log!(
        "slot {s} was on trial and failed ({}): retired; restarting into slot {}",
        failure.reason.as_str(),
        s.other()
    );
    pmsg(&format!(
        "pinna: reboot (slot {s} failed its trial: {}, back to slot {})",
        failure.reason.as_str(),
        s.other()
    ));
    rescue::reboot_now()
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

    #[test]
    fn gpt_finds_partitions_by_partname() {
        let d = tempfile::tempdir().unwrap();
        let part = |name: &str, dev: &str, uevent: &str| {
            let p = d.path().join(name);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("uevent"), uevent).unwrap();
            let _ = dev;
        };
        part(
            "mmcblk0p5",
            "",
            "MAJOR=179\nMINOR=5\nDEVNAME=mmcblk0p5\nPARTNAME=system_a\n",
        );
        part("mmcblk0", "", "MAJOR=179\nMINOR=0\nDEVNAME=mmcblk0\n"); // no PARTNAME
        let parts = scan_gpt(d.path());
        let p = parts.get("system_a").expect("system_a");
        assert_eq!(p.devname, "mmcblk0p5");
        assert_eq!(p.dev, (179, 5));
        assert_eq!(p.node, NodeKind::Block);
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn ubi_finds_volumes_by_name() {
        let d = tempfile::tempdir().unwrap();
        let vol = |dir: &str, name: &str, dev: &str| {
            let p = d.path().join(dir);
            fs::create_dir_all(&p).unwrap();
            if !name.is_empty() {
                fs::write(p.join("name"), format!("{name}\n")).unwrap();
            }
            fs::write(p.join("dev"), format!("{dev}\n")).unwrap();
        };
        vol("ubi0_2", "system_a", "247:2");
        vol("ubi0_3", "system_b", "247:3");
        vol("ubi0", "", "246:0"); // the ubi device itself, not a volume
        vol("ubi0_9", "", "247:9"); // a volume with no name file: skipped

        let parts = scan_ubi(d.path(), &d.path().join("block"));
        let a = parts.get("system_a").expect("system_a");
        assert_eq!(a.devname, "ubi0_2");
        assert_eq!(a.dev, (247, 2));
        assert_eq!(a.node, NodeKind::Char);
        assert_eq!(parts.get("system_b").unwrap().dev, (247, 3));
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn ubi_prefers_the_volumes_block_device() {
        let d = tempfile::tempdir().unwrap();
        let ubi = d.path().join("ubi");
        let block = d.path().join("block");
        for (dir, name, dev) in [
            ("ubi0_6", "misc", "247:7"),
            ("ubi0_9", "system_a", "247:10"),
        ] {
            fs::create_dir_all(ubi.join(dir)).unwrap();
            fs::write(ubi.join(dir).join("name"), format!("{name}\n")).unwrap();
            fs::write(ubi.join(dir).join("dev"), format!("{dev}\n")).unwrap();
        }
        // ubi.block=0,system_a: the kernel's ubiblock0_9 (stock Fire OS: 254:0).
        fs::create_dir_all(block.join("ubiblock0_9")).unwrap();
        fs::write(block.join("ubiblock0_9").join("dev"), "254:0\n").unwrap();

        let parts = scan_ubi(&ubi, &block);
        let a = parts.get("system_a").unwrap();
        assert_eq!(
            (a.devname.as_str(), a.dev, a.node),
            ("ubiblock0_9", (254, 0), NodeKind::Block)
        );
        let misc = parts.get("misc").unwrap();
        assert_eq!(
            (misc.devname.as_str(), misc.node),
            ("ubi0_6", NodeKind::Char)
        );
    }
}
