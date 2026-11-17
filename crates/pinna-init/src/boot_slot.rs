//! Which slot's root to boot, and what to write to `misc` first (ADR 0019).
//!
//! On the Echos the preloader has already counted the boot in Kaeru's slot
//! record, and LK has loaded that slot's kernel: pinna-init boots the root of
//! the same slot and writes nothing. The VMs, which QEMU starts directly, say
//! `pinna.bootloader=none` on their command line; there pinna-init plays
//! preloader and LK on the same record, so the VM tests run the same rules.

use pinna_bootctl::bcb::Record;
use pinna_bootctl::slot::Slot;

/// The command line flag of a machine without a bootloader slot (the VMs).
pub const NO_BOOTLOADER: &str = "pinna.bootloader=none";

fn arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split(|c: char| c.is_whitespace() || c == '\0')
        .find_map(|t| t.strip_prefix(key))
}

fn letter(s: &str) -> Option<Slot> {
    match s {
        "a" => Some(Slot::A),
        "b" => Some(Slot::B),
        _ => None,
    }
}

/// LK's slot, from its command line in the device tree
/// (`androidboot.slot_suffix=_a`).
pub fn lk_slot_from(bootargs: &str) -> Option<Slot> {
    letter(arg(bootargs, "androidboot.slot_suffix=")?.strip_prefix('_')?)
}

/// The boot partition LK actually loaded: Pinna writes `pinna.boot=a` or `=b`
/// into the header of each boot image it puts into a slot, and LK passes the
/// header's line on. A Kaeru before `ab93750` reports slot b but loads boot_a.
pub fn kernel_slot_from(bootargs: &str) -> Option<Slot> {
    letter(arg(bootargs, "pinna.boot=")?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub slot: Slot,
    /// The record after this boot's writes.
    pub record: Record,
    /// What to write to the slot record before booting.
    pub write: Option<Record>,
    pub reason: String,
}

/// `lk`: the slot LK started. `emulate`: no bootloader at all (the VMs).
/// `record`: Kaeru's record as read, `None` when missing.
pub fn plan(lk: Option<Slot>, emulate: bool, record: Option<Record>) -> Plan {
    let rec = record.unwrap_or(Record::KAERU_DEFAULT);
    if emulate {
        let mut write = record.is_none().then_some(rec);
        let stepped = rec.preloader_step();
        if stepped != rec {
            write = Some(stepped);
        }
        let s = stepped.lk_choice().unwrap_or(Slot::A);
        return Plan {
            slot: s,
            record: stepped,
            write,
            reason: format!(
                "no bootloader: slot {s}, as the preloader and LK would choose ({})",
                stepped.describe(s)
            ),
        };
    }
    match lk {
        Some(s) => Plan {
            slot: s,
            record: rec,
            write: None,
            reason: format!(
                "slot {s}, as the bootloader started it ({})",
                rec.describe(s)
            ),
        },
        None => {
            // A bootloader that names no slot: boot what LK would choose,
            // and leave the counting to whatever counts.
            let s = rec.lk_choice().unwrap_or(Slot::A);
            Plan {
                slot: s,
                record: rec,
                write: None,
                reason: format!(
                    "the bootloader named no slot: slot {s}, as the record says ({})",
                    rec.describe(s)
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(a: u8, b: u8) -> Record {
        Record::decode(&[0, b'A', b'B', b'B', 1, a, b]).unwrap()
    }

    const MEASURED: &str = "console=tty0 androidboot.lk_build_desc=63cb91b-20221007_072309 \
                            bootopt=64S3,32N2,32N2 pinna.boot=b androidboot.slot_suffix=_b \
                            root=/dev/dm-0 skip_initramfs\0";

    #[test]
    fn reads_what_lk_passes() {
        assert_eq!(lk_slot_from(MEASURED), Some(Slot::B));
        assert_eq!(kernel_slot_from(MEASURED), Some(Slot::B));
        assert_eq!(lk_slot_from("androidboot.slot_suffix=_a"), Some(Slot::A));
        assert_eq!(lk_slot_from("console=ttyAMA0 root=/dev/vda"), None);
        assert_eq!(lk_slot_from("androidboot.slot_suffix=_c"), None);
        assert_eq!(kernel_slot_from("bootopt=64S3,32N2,32N2"), None);
    }

    #[test]
    fn on_a_device_boots_lks_slot_and_writes_nothing() {
        let p = plan(Some(Slot::B), false, Some(rec(0x8e, 0x2f)));
        assert_eq!((p.slot, p.write), (Slot::B, None));
        assert!(p.reason.contains("trial attempts=1"), "{}", p.reason);
        // No suffix and no VM flag: follow the record (b has the higher
        // priority), write nothing.
        let p = plan(None, false, Some(rec(0x8e, 0x8f)));
        assert_eq!((p.slot, p.write), (Slot::B, None));
    }

    #[test]
    fn in_a_vm_plays_preloader_and_lk() {
        // A fresh disk: Kaeru's default is written, slot a boots.
        let p = plan(None, true, None);
        assert_eq!((p.slot, p.write), (Slot::A, Some(Record::KAERU_DEFAULT)));
        // The measured rollback, one boot at a time.
        let mut r = rec(0x8e, 0x3f);
        let mut booted = vec![];
        for _ in 0..4 {
            let p = plan(None, true, Some(r));
            booted.push(p.slot);
            r = p.write.unwrap_or(r);
        }
        assert_eq!(booted, [Slot::B, Slot::B, Slot::A, Slot::A]);
        assert_eq!(r, rec(0x8e, 0x00));
        // A good slot: nothing to write.
        assert_eq!(plan(None, true, Some(rec(0x8f, 0x00))).write, None);
    }
}
