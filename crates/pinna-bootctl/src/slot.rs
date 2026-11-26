//! The two A/B slots, and what pinna-init records about the boot in
//! `/run/pinna`. The slot record itself is the bootloader's, see
//! [`crate::bcb`] (ADR 0019).

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use pinna_board::BootFormat;

use crate::storage::Parts;

/// Where pinna-init records what it booted.
pub const RUN_DIR: &str = "/run/pinna";
/// In [`RUN_DIR`]: the boot partition LK loaded, as its image says
/// (`pinna.boot=`), when it says.
pub const BOOT_KERNEL: &str = "boot-kernel";
/// What `mark-good` learned about this device's bootloader: whether it loads
/// the started slot's kernel (ADR 0019).
pub const KERNEL_SLOTS: &str = "/var/lib/pinna/kernel-slots";

/// Whether the bootloader loads `boot_b` when it starts slot b. A Kaeru before
/// `ab93750` doesn't, and writing boot_a there replaces the only kernel it
/// ever loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelSlots {
    Unknown,
    Honoured,
    Ignored,
}

impl KernelSlots {
    /// What one boot shows: only a boot of slot b tells the two apart. A
    /// boot of slot a with another kernel says something is odd about the
    /// boot images, not about the bootloader.
    pub fn seen(booted: Slot, kernel: Option<Slot>) -> Option<KernelSlots> {
        match (booted, kernel?) {
            (Slot::B, Slot::B) => Some(KernelSlots::Honoured),
            (Slot::B, Slot::A) => Some(KernelSlots::Ignored),
            (Slot::A, _) => None,
        }
    }

    /// What this board's bootloader does with the slot. A FIT bootloader
    /// (Amazon's U-Boot) reads `boot_<slot>` by design and there is nothing
    /// to learn from a boot, so the file is not consulted.
    pub fn for_format(format: BootFormat, path: &Path) -> KernelSlots {
        match format {
            BootFormat::Fit => KernelSlots::Honoured,
            BootFormat::Android => KernelSlots::read(path),
        }
    }

    pub fn read(path: &Path) -> KernelSlots {
        match std::fs::read_to_string(path).as_deref().map(str::trim) {
            Ok("honoured") => KernelSlots::Honoured,
            Ok("ignored") => KernelSlots::Ignored,
            _ => KernelSlots::Unknown,
        }
    }

    pub fn write(self, path: &Path) -> Result<()> {
        let word = match self {
            KernelSlots::Unknown => return Ok(()),
            KernelSlots::Honoured => "honoured",
            KernelSlots::Ignored => "ignored",
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, format!("{word}\n"))
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// The boot partition LK loaded, as pinna-init recorded it.
pub fn booted_kernel(run_dir: &Path) -> Result<Option<Slot>> {
    read_run_file(run_dir, BOOT_KERNEL)?
        .map(|s| Slot::parse(&s))
        .transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    pub fn letter(self) -> char {
        match self {
            Slot::A => 'a',
            Slot::B => 'b',
        }
    }
    /// The partition (or UBI volume) name of the slot's root.
    pub fn partname(self) -> &'static str {
        match self {
            Slot::A => "system_a",
            Slot::B => "system_b",
        }
    }
    pub fn parse(s: &str) -> Result<Slot> {
        match s.trim() {
            "a" | "A" => Ok(Slot::A),
            "b" | "B" => Ok(Slot::B),
            other => bail!("not a slot: {other:?} (expected a or b)"),
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.letter())
    }
}

/// The signed image in a slot partition, checked as pinna-init checks it
/// before booting: the footer's signature and the top hash block.
pub fn image(
    parts: &Parts,
    s: Slot,
    key: &pinna_image::keys::VerifyingKey,
) -> Result<pinna_image::Footer> {
    let path = parts.path(s.partname());
    let dev = std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let f = pinna_image::read_footer(&dev, key)?;
    pinna_image::check_top_hash(&dev, &f)?;
    Ok(f)
}

fn read_run_file(run_dir: &Path, name: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(run_dir.join(name)) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}/{name}", run_dir.display())),
    }
}

/// The slot pinna-init booted (`None` without pinna-init).
pub fn booted_slot(run_dir: &Path) -> Result<Option<Slot>> {
    read_run_file(run_dir, "boot-slot")?
        .map(|s| Slot::parse(&s))
        .transpose()
}

pub fn booted_version(run_dir: &Path) -> Result<Option<String>> {
    read_run_file(run_dir, "boot-version")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_boot_of_b_shows_what_the_bootloader_does() {
        use KernelSlots::*;
        assert_eq!(KernelSlots::seen(Slot::B, Some(Slot::B)), Some(Honoured));
        assert_eq!(KernelSlots::seen(Slot::B, Some(Slot::A)), Some(Ignored));
        assert_eq!(KernelSlots::seen(Slot::A, Some(Slot::A)), None);
        assert_eq!(KernelSlots::seen(Slot::A, Some(Slot::B)), None);
        assert_eq!(KernelSlots::seen(Slot::B, None), None);
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("pinna/kernel-slots");
        assert_eq!(KernelSlots::read(&p), Unknown);
        Honoured.write(&p).unwrap();
        assert_eq!(KernelSlots::read(&p), Honoured);
        std::fs::write(d.path().join("boot-kernel"), "b\n").unwrap();
        assert_eq!(booted_kernel(d.path()).unwrap(), Some(Slot::B));
        // A FIT bootloader loads the started slot's kernel whatever the file says.
        let none = d.path().join("none");
        assert_eq!(KernelSlots::for_format(BootFormat::Fit, &none), Honoured);
        assert_eq!(KernelSlots::for_format(BootFormat::Android, &none), Unknown);
    }

    #[test]
    fn run_files() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(booted_slot(d.path()).unwrap(), None);
        std::fs::write(d.path().join("boot-slot"), "b\n").unwrap();
        std::fs::write(d.path().join("boot-version"), "1.2\n").unwrap();
        assert_eq!(booted_slot(d.path()).unwrap(), Some(Slot::B));
        assert_eq!(booted_version(d.path()).unwrap().as_deref(), Some("1.2"));
    }
}
