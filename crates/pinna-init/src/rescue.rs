//! Rescue mode (ADR 0012): what pinna-init does instead of booting when it
//! cannot. Everything comes from the boot image:
//!
//! - the ring flashes the code of the failed stage until USB is up, then shows
//!   green (steady once a computer has configured the gadget);
//! - a USB gadget, "Pinna rescue": ACM serial with a root shell, NCM network
//!   at 10.42.0.1/24 with a DHCP server;
//! - a root shell on the kernel console as well;
//! - a page on <http://10.42.0.1> with the logs and a restart ([`crate::http`]).
//!
//! It never opens `/dev/watchdog`: the kernel keeps feeding the watchdog.
//! `reboot`, `poweroff` and `halt` from the shell signal PID 1 as busybox
//! does (SIGTERM, SIGUSR2, SIGUSR1), and are honoured here.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pinna_board::Bootloader;
use pinna_bootctl::stages;

use crate::http;
use crate::ring::{Ring, Show};

pub const BUSYBOX: &str = "/bin/busybox";
/// Where busybox is restored when the root filesystem is already read-only.
const BUSYBOX_RUN: &str = "/run/busybox";
const GADGET: &str = "/sys/kernel/config/usb_gadget/pinna";
const IDME_SERIAL: &str = "/sys/firmware/devicetree/base/idme/serial/value";
pub const ADDRESS: &str = "10.42.0.1";
const DHCP_CONF: &str = "/run/udhcpd.conf";
const DHCP_LEASES: &str = "/run/udhcpd.leases";
/// A shell that exits sooner than this after starting is not restarted
/// right away (a console without input returns end-of-file at once).
const SHELL_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Neither `system_a` nor `system_b` appeared.
    NoStorage,
    /// No slot holds a root that can be verified and mounted.
    NoRoot,
    /// The root was mounted, but the system on it could not be started.
    NoSystem,
    /// pinna-init itself failed (a bug) during this stage.
    Crashed(u8),
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NoStorage => "no-storage",
            Reason::NoRoot => "no-root",
            Reason::NoSystem => "no-system",
            Reason::Crashed(_) => "crashed",
        }
    }

    /// The boot stage that did not complete: the code the ring flashes.
    pub fn stage(self) -> u8 {
        match self {
            Reason::NoStorage => stages::STORAGE,
            Reason::NoRoot => stages::ROOT,
            Reason::NoSystem => stages::SYSTEM,
            Reason::Crashed(n) => n,
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            Reason::NoStorage => {
                "The internal storage did not show its partitions within 15 seconds, so there \
                 is nothing to boot from. This points at the kernel or a driver."
            }
            Reason::NoRoot => {
                "No slot holds a root filesystem that pinna-init can verify and mount, so there \
                 is nothing to boot."
            }
            Reason::NoSystem => {
                "The root filesystem was verified and mounted, but the system on it could not \
                 be started."
            }
            Reason::Crashed(_) => {
                "pinna-init itself failed. This is a bug: please report it with the diagnostics."
            }
        }
    }

    /// What usually fixes it. `fastboot`: the board has it, so a fix is a
    /// flash; elsewhere the fix is the same image and [`recovery`] says how
    /// it gets there.
    pub fn fix(self, detail: &str, fastboot: bool) -> String {
        let flash = if fastboot { "Flash" } else { "Install" };
        match self {
            Reason::NoStorage => format!(
                "The kernel in the boot image usually can't drive the storage. {flash} a boot \
                 image that worked before."
            ),
            Reason::NoRoot if detail.contains("signature") => format!(
                "The boot image and the root filesystems were signed with different keys. \
                 {flash} a boot image and a root filesystem from the same build."
            ),
            Reason::NoRoot | Reason::NoSystem => {
                format!("{flash} a root filesystem, and a boot image from the same build.")
            }
            Reason::Crashed(_) => format!("{flash} a boot image that worked before."),
        }
    }
}

/// How a fix reaches the satellite, from its bootloader (`None`: a board
/// Pinna doesn't know). Only the MT8163 and MT8516 Echos' LK has fastboot; on
/// the Echo Dot (5th gen) the way back is the community installer's
/// preloader route over the USB test pads inside the base (ADR 0035), which
/// the satellite can't describe further.
pub fn recovery(bootloader: Option<Bootloader>) -> &'static str {
    match bootloader {
        Some(Bootloader::Kaeru { .. }) => FASTBOOT,
        Some(Bootloader::AmazonUboot) => {
            "This satellite has no fastboot. Install the image with the community installer, \
             over the USB test pads inside the base."
        }
        None => "Pinna doesn't know this board, so it can't say how to install an image.",
    }
}

/// How to open fastboot, where every fix on a Kaeru board starts.
pub const FASTBOOT: &str =
    "Open fastboot: unplug the device, hold Volume Down, and plug it in again.";
/// Where the docs describe rescue mode.
pub const DOCS: &str = "https://getpinna.dev/docs/recovery/#rescue-mode";

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why pinna-init could not boot.
pub struct Failure {
    pub reason: Reason,
    pub error: anyhow::Error,
}

impl Failure {
    pub fn new(reason: Reason, error: anyhow::Error) -> Self {
        Failure { reason, error }
    }
}

/// What the page and the shells say about this rescue.
pub struct Info {
    pub reason: Reason,
    pub detail: String,
    /// The board's bootloader, which decides what the text can tell the owner
    /// to do: fastboot and a USB cable exist only on some boards.
    pub bootloader: Option<Bootloader>,
}

impl Info {
    pub fn fix(&self) -> String {
        let fastboot = self.bootloader.and_then(Bootloader::fastboot_product);
        self.reason.fix(&self.detail, fastboot.is_some())
    }

    pub fn recovery(&self) -> &'static str {
        recovery(self.bootloader)
    }

    /// The rescue gadget's USB cable: the Echo Dot (5th gen) has no USB
    /// connector (ADR 0035), so its page and shells don't send the owner there.
    pub fn has_usb(&self) -> bool {
        self.bootloader != Some(Bootloader::AmazonUboot)
    }

    /// Where the shell's owner reads the logs.
    pub fn logs(&self) -> String {
        if self.has_usb() {
            format!("http://{ADDRESS} over this USB cable, or: dmesg | grep pinna-init")
        } else {
            "dmesg | grep pinna-init".into()
        }
    }
}

/// busybox, open (close-on-exec) after it left the initramfs, for a switch
/// to the root that fails late.
static KEPT_BUSYBOX: Mutex<Option<fs::File>> = Mutex::new(None);

/// Before switching root: delete busybox from the initramfs, which would
/// otherwise stay in memory, but keep it open. A successful exec closes it;
/// rescue mode after a failed switch restores it ([`restore_busybox`]).
pub fn release_busybox() {
    if let Ok(f) = fs::File::open(BUSYBOX) {
        *KEPT_BUSYBOX.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }
    let _ = fs::remove_file(BUSYBOX);
}

/// busybox for rescue mode: in the initramfs, or written back from the kept
/// file after a failed switch (to /run when / is the read-only root).
fn restore_busybox() -> Option<&'static str> {
    if Path::new(BUSYBOX).exists() {
        return Some(BUSYBOX);
    }
    let mut kept = KEPT_BUSYBOX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()?;
    for path in [BUSYBOX, BUSYBOX_RUN] {
        use std::io::Seek;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = kept.rewind();
        let copied = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(path)
            .and_then(|mut out| std::io::copy(&mut kept, &mut out));
        if copied.is_ok() {
            log!("rescue: restored busybox to {path}");
            return Some(path);
        }
    }
    None
}

/// The last power signal from the shell (`reboot`, `poweroff`, `halt`).
static POWER_REQUEST: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_power_signal(sig: libc::c_int) {
    POWER_REQUEST.store(sig, Ordering::SeqCst);
}

fn catch_power_signals() {
    for sig in [libc::SIGTERM, libc::SIGUSR1, libc::SIGUSR2] {
        // SAFETY: the handler only stores into an atomic.
        unsafe {
            libc::signal(sig, on_power_signal as *const () as libc::sighandler_t);
        }
    }
}

pub fn reboot_now() -> ! {
    power_off_or_reboot(libc::RB_AUTOBOOT)
}

fn power_off_or_reboot(how: libc::c_int) -> ! {
    // SAFETY: plain sync + reboot; we are PID 1.
    unsafe {
        libc::sync();
        libc::reboot(how);
    }
    loop {
        sleep(Duration::from_secs(60));
    }
}

pub fn run(failure: Failure, ring: &Ring) -> ! {
    let Failure { reason, error } = failure;
    log!("rescue mode ({reason}): {error:#}");
    crate::pmsg(&format!("pinna: rescue ({reason})"));
    ring.show(Show::Code(reason.stage()));
    catch_power_signals();
    if let Err(e) = crate::early_mounts() {
        log!("rescue: mounts: {e:#}");
    }
    for (src, target, fstype) in [
        ("configfs", "/sys/kernel/config", "configfs"),
        ("pstore", "/sys/fs/pstore", "pstore"),
    ] {
        // Kernels without configfs or pstore have no mount point for them.
        if Path::new(target).is_dir()
            && let Err(e) = crate::mount(src, target, Some(fstype), 0, None)
        {
            log!("rescue: {e:#}");
        }
    }
    let busybox = restore_busybox();
    match busybox {
        Some(bb) => {
            let _ = Command::new(bb).args(["--install", "-s", "/bin"]).status();
        }
        None => log!("rescue: no busybox in this boot image; no shells, no network"),
    }

    let info = Arc::new(Info {
        reason,
        detail: format!("{error:#}"),
        bootloader: pinna_board::detect().map(|b| b.bootloader),
    });
    {
        let info = Arc::clone(&info);
        let _ = thread::Builder::new()
            .name("http".into())
            .spawn(move || http::serve(&info));
    }

    let serial = device_serial();
    let mut udc: Option<String> = None;
    let mut gadget_failed = false;
    // Process name -> pid.
    let mut running: HashMap<&'static str, u32> = HashMap::new();
    let mut last_start: HashMap<&'static str, Instant> = HashMap::new();
    loop {
        match POWER_REQUEST.swap(0, Ordering::SeqCst) {
            0 => {}
            libc::SIGUSR2 | libc::SIGUSR1 => {
                log!("rescue: power off");
                power_off_or_reboot(libc::RB_POWER_OFF)
            }
            _ => {
                log!("rescue: reboot");
                reboot_now()
            }
        }
        reap(&mut running);

        match &udc {
            None => {
                if let Some(name) = first_udc() {
                    match gadget_up(&name, &serial).and_then(|()| network_up(busybox)) {
                        Ok(()) => {
                            log!("rescue: USB gadget on {name}, {ADDRESS}");
                            udc = Some(name);
                        }
                        Err(e) if !gadget_failed => {
                            log!("rescue: USB gadget: {e:#}");
                            gadget_failed = true;
                        }
                        Err(_) => {}
                    }
                }
            }
            Some(name) => {
                let host = fs::read_to_string(format!("/sys/class/udc/{name}/state"))
                    .is_ok_and(|s| s.trim() == "configured");
                ring.show(Show::Rescue { host });
            }
        }

        if let Some(bb) = busybox {
            let mut wanted = vec!["console"];
            if udc.is_some() {
                wanted.extend(["ttyGS0", "udhcpd"]);
            }
            for name in wanted {
                if running.contains_key(name)
                    || last_start
                        .get(name)
                        .is_some_and(|t| t.elapsed() < SHELL_BACKOFF)
                {
                    continue;
                }
                let started = match name {
                    "udhcpd" => spawn_dhcp(bb),
                    tty => spawn_shell(bb, &format!("/dev/{tty}"), &info),
                };
                last_start.insert(name, Instant::now());
                if let Ok(pid) = started {
                    running.insert(name, pid);
                }
            }
        }
        sleep(Duration::from_secs(1));
    }
}

/// Reap every exited child (PID 1 inherits orphans too).
fn reap(running: &mut HashMap<&'static str, u32>) {
    loop {
        let mut status = 0;
        // SAFETY: plain waitpid.
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            return;
        }
        running.retain(|_, p| *p != pid as u32);
    }
}

fn first_udc() -> Option<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/udc")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.into_iter().next()
}

/// The device's serial number from the bootloader's idme record, for the
/// USB serial number and stable MAC addresses.
fn device_serial() -> String {
    let raw = fs::read(IDME_SERIAL).unwrap_or_default();
    let s: String = String::from_utf8_lossy(&raw)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if s.is_empty() { "pinna".into() } else { s }
}

/// A locally administered MAC derived from the serial number, as the normal
/// gadget (usb-gadget in the root) derives it: the first five bytes of its
/// sha256. `first` is 02 for the device side, 06 for the host side.
fn stable_mac(first: u8, serial: &str) -> String {
    use sha2::Digest;
    let b = sha2::Sha256::digest(serial.as_bytes());
    format!(
        "{first:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4]
    )
}

/// The normal gadget's ids and functions (usb-gadget in the root), named
/// "Pinna rescue".
fn gadget_up(udc: &str, serial: &str) -> Result<()> {
    let g = Path::new(GADGET);
    let write = |rel: &str, value: &str| {
        fs::write(g.join(rel), value).with_context(|| format!("writing {rel}"))
    };
    for dir in [
        "strings/0x409",
        "configs/c.1/strings/0x409",
        "functions/acm.GS0",
        "functions/ncm.usb0",
    ] {
        fs::create_dir_all(g.join(dir)).with_context(|| format!("creating {dir}"))?;
    }
    write("idVendor", "0x1d6b")?;
    write("idProduct", "0x0104")?;
    write("bcdDevice", "0x0100")?;
    write("bcdUSB", "0x0200")?;
    write("bDeviceClass", "0xEF")?;
    write("bDeviceSubClass", "0x02")?;
    write("bDeviceProtocol", "0x01")?;
    write("strings/0x409/serialnumber", serial)?;
    write("strings/0x409/manufacturer", "Pinna")?;
    write("strings/0x409/product", "Pinna rescue")?;
    write(
        "configs/c.1/strings/0x409/configuration",
        "rescue: serial console + network",
    )?;
    write("configs/c.1/MaxPower", "250")?;
    write("functions/ncm.usb0/dev_addr", &stable_mac(0x02, serial))?;
    write("functions/ncm.usb0/host_addr", &stable_mac(0x06, serial))?;
    for f in ["acm.GS0", "ncm.usb0"] {
        let link = g.join("configs/c.1").join(f);
        if !link.exists() {
            std::os::unix::fs::symlink(g.join("functions").join(f), &link)
                .with_context(|| format!("linking {f}"))?;
        }
    }
    write("UDC", udc)
}

fn run_busybox(bb: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(bb)
        .args(args)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("busybox {}", args.join(" ")))?;
    anyhow::ensure!(status.success(), "busybox {}: {status}", args.join(" "));
    Ok(())
}

fn network_up(busybox: Option<&str>) -> Result<()> {
    let bb = busybox.context("no busybox to configure usb0 with")?;
    let addr = format!("{ADDRESS}/24");
    run_busybox(bb, &["ip", "link", "set", "lo", "up"])?;
    run_busybox(bb, &["ip", "addr", "add", &addr, "dev", "usb0"])?;
    run_busybox(bb, &["ip", "link", "set", "usb0", "up"])?;
    // The same pool as systemd-networkd's on usb0 (10-usb0.network); no
    // router or DNS, so the computer's own networking is unaffected.
    fs::write(
        DHCP_CONF,
        format!(
            "start 10.42.0.10\nend 10.42.0.29\ninterface usb0\nlease_file {DHCP_LEASES}\n\
             option subnet 255.255.255.0\n"
        ),
    )?;
    fs::write(DHCP_LEASES, "")?;
    Ok(())
}

fn spawn_dhcp(bb: &str) -> std::io::Result<u32> {
    Ok(Command::new(bb)
        .args(["udhcpd", "-f", DHCP_CONF])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .id())
}

/// A root shell on `tty`, as its controlling terminal.
fn spawn_shell(bb: &str, tty: &str, info: &Info) -> std::io::Result<u32> {
    let mut t = fs::OpenOptions::new().read(true).write(true).open(tty)?;
    let _ = write!(
        t,
        "\r\nPinna rescue mode ({reason}): {detail}\r\n{explain}\r\n\r\n\
         To fix it: {fix}\r\n{recovery}\r\n{DOCS}\r\n\r\n\
         Logs: {logs}\r\n\
         Restart: reboot\r\n\r\n",
        reason = info.reason,
        detail = info.detail,
        explain = info.reason.explain(),
        fix = info.fix(),
        recovery = info.recovery(),
        logs = info.logs(),
    );
    let mut cmd = Command::new(bb);
    cmd.args(["sh", "-i"])
        .env_clear()
        .env("PATH", "/bin")
        .env("HOME", "/")
        .env("TERM", "vt100")
        .env("PS1", "pinna-rescue# ")
        .stdin(t.try_clone()?)
        .stdout(t.try_clone()?)
        .stderr(t);
    // SAFETY: setsid and ioctl are async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::ioctl(0, libc::TIOCSCTTY, 1);
            Ok(())
        });
    }
    Ok(cmd.spawn()?.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons() {
        assert_eq!(Reason::NoStorage.stage(), 1);
        assert_eq!(Reason::NoRoot.stage(), 2);
        assert_eq!(Reason::NoRoot.to_string(), "no-root");
        assert_eq!(Reason::NoSystem.stage(), 3);
        assert_eq!(Reason::Crashed(1).stage(), 1);
        assert_eq!(Reason::Crashed(2).as_str(), "crashed");
        assert!(
            Reason::NoRoot
                .fix("slot a: footer signature does not verify", true)
                .contains("keys")
        );
        assert!(
            !Reason::NoRoot
                .fix("slot a: no erofs superblock", true)
                .contains("keys")
        );
    }

    #[test]
    fn fastboot_and_the_cable_appear_only_where_the_board_has_them() {
        use pinna_board::{ECHO_DOT_2, ECHO_DOT_5};
        let info = |b: Option<&pinna_board::Board>| Info {
            reason: Reason::NoRoot,
            detail: String::new(),
            bootloader: b.map(|b| b.bootloader),
        };
        let dot2 = info(Some(&ECHO_DOT_2));
        assert_eq!(dot2.recovery(), FASTBOOT);
        assert!(dot2.fix().starts_with("Flash"));
        assert!(dot2.has_usb());
        for i in [info(Some(&ECHO_DOT_5)), info(None)] {
            for text in [i.recovery().to_owned(), i.fix()] {
                assert!(!text.contains("Volume Down"), "{text}");
                assert!(
                    !text.contains("fastboot") || text.contains("no fastboot"),
                    "{text}"
                );
                assert!(!text.contains("Flash"), "{text}");
            }
        }
        let dot5 = info(Some(&ECHO_DOT_5));
        assert!(dot5.recovery().contains("community installer"));
        assert!(!dot5.has_usb());
        assert!(!dot5.logs().contains("USB"));
    }

    #[test]
    fn macs_match_the_normal_gadget() {
        // usb-gadget: printf '%s' G090LF0971521APS | sha256sum -> 7dcb87600f...
        assert_eq!(stable_mac(0x02, "G090LF0971521APS"), "02:7d:cb:87:60:0f");
        assert_eq!(stable_mac(0x06, "G090LF0971521APS"), "06:7d:cb:87:60:0f");
    }

    #[test]
    fn macs_are_stable_and_locally_administered() {
        let a = stable_mac(0x02, "G090XG1234");
        assert_eq!(a, stable_mac(0x02, "G090XG1234"));
        assert!(a.starts_with("02:"));
        assert_eq!(a.len(), 17);
        assert_ne!(a[3..], stable_mac(0x02, "G090XG1235")[3..]);
        assert_eq!(a[3..], stable_mac(0x06, "G090XG1234")[3..]);
    }
}
