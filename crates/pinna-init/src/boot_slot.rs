//! Which slot's root to boot, and what to write to `misc` first (ADR 0019).
//!
//! On the Echos the preloader has already counted the boot in Kaeru's slot
//! record, and LK has loaded that slot's kernel: pinna-init boots the root of
//! the same slot and writes nothing. Without a bootloader slot (the VMs, which
//! QEMU starts directly) pinna-init plays preloader and LK on the same record,
//! so the VM tests run the same rules.

use pinna_bootctl::bcb::{self, Record};
use pinna_bootctl::slot::{Slot, SlotState};

/// LK's slot, from its command line in the device tree
/// (`androidboot.slot_suffix=_a`).
pub fn lk_slot_from(bootargs: &str) -> Option<Slot> {
    bootargs
        .split(|c: char| c.is_whitespace() || c == '\0')
        .find_map(|t| match t.strip_prefix("androidboot.slot_suffix=")? {
            "_a" => Some(Slot::A),
            "_b" => Some(Slot::B),
            _ => None,
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub slot: Slot,
    /// The record after this boot's writes.
    pub record: Record,
    /// What to write to the slot record before booting.
    pub write: Option<Record>,
    /// Blank Pinna's own record (ADR 0003) once the write succeeded.
    pub clear_pinna: bool,
    pub reason: String,
}

/// `lk`: the slot LK started, `None` without a bootloader. `record`: Kaeru's
/// record as read, `None` when missing. `pinna`: Pinna's own record, which a
/// first boot of this pinna-init moves into Kaeru's.
pub fn plan(lk: Option<Slot>, record: Option<Record>, pinna: Option<SlotState>) -> Plan {
    let mut rec = record.unwrap_or(Record::KAERU_DEFAULT);
    let mut write = None;
    let mut clear_pinna = false;
    let mut notes = Vec::new();
    if let Some(p) = pinna {
        match bcb::from_pinna(&p) {
            Some(m) => {
                notes.push(format!(
                    "moved Pinna's own slot record (active {}) into the bootloader's",
                    p.active
                ));
                rec = m;
                write = Some(m);
                clear_pinna = true;
            }
            None => notes.push("Pinna's own slot record has no good slot: left as it is".into()),
        }
    }

    let (slot, reason) = match lk {
        Some(lk) if !clear_pinna => (
            lk,
            format!(
                "slot {lk}, as the bootloader started it ({})",
                rec.describe(lk)
            ),
        ),
        Some(lk) => {
            // Deployment wrote the same boot image to both boot partitions,
            // so the kernel LK started fits the migrated slot's root.
            let s = rec.lk_choice().unwrap_or(lk);
            (
                s,
                format!(
                    "slot {s}, from the moved record (the bootloader started {lk}) ({})",
                    rec.describe(s)
                ),
            )
        }
        None => {
            if record.is_none() && write.is_none() {
                write = Some(rec);
            }
            let stepped = rec.preloader_step();
            if stepped != rec {
                write = Some(stepped);
            }
            rec = stepped;
            let s = rec.lk_choice().unwrap_or(Slot::A);
            (
                s,
                format!(
                    "no bootloader slot: slot {s}, as the preloader and LK would choose ({})",
                    rec.describe(s)
                ),
            )
        }
    };
    notes.push(reason);
    Plan {
        slot,
        record: rec,
        write,
        clear_pinna,
        reason: notes.join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_bootctl::slot::SlotInfo;

    fn rec(a: u8, b: u8) -> Record {
        Record::decode(&[0, b'A', b'B', b'B', 1, a, b]).unwrap()
    }

    #[test]
    fn reads_the_suffix_lk_passes() {
        let measured = "console=tty0 androidboot.lk_build_desc=63cb91b-20221007_072309 \
                        bootopt=64S3,32N2,32N2 androidboot.slot_suffix=_b root=/dev/dm-0 skip_initramfs\0";
        assert_eq!(lk_slot_from(measured), Some(Slot::B));
        assert_eq!(lk_slot_from("androidboot.slot_suffix=_a"), Some(Slot::A));
        assert_eq!(lk_slot_from("console=ttyAMA0 root=/dev/vda"), None);
        assert_eq!(lk_slot_from("androidboot.slot_suffix=_c"), None);
    }

    #[test]
    fn on_a_device_boots_lks_slot_and_writes_nothing() {
        let p = plan(Some(Slot::B), Some(rec(0x8e, 0x2f)), None);
        assert_eq!((p.slot, p.write, p.clear_pinna), (Slot::B, None, false));
        assert!(p.reason.contains("trial tries=2"), "{}", p.reason);
    }

    #[test]
    fn in_a_vm_plays_preloader_and_lk() {
        // A fresh disk: Kaeru's default is written, slot a boots.
        let p = plan(None, None, None);
        assert_eq!((p.slot, p.write), (Slot::A, Some(Record::KAERU_DEFAULT)));
        // The measured rollback, one boot at a time.
        let mut r = rec(0x8e, 0x3f);
        let mut booted = vec![];
        for _ in 0..4 {
            let p = plan(None, Some(r), None);
            booted.push(p.slot);
            r = p.write.unwrap_or(r);
        }
        assert_eq!(booted, [Slot::B, Slot::B, Slot::A, Slot::A]);
        assert_eq!(r, rec(0x8e, 0x00));
        // A good slot: nothing to write.
        assert_eq!(plan(None, Some(rec(0x8f, 0x00)), None).write, None);
    }

    fn pinna(active: Slot, a: (u8, bool), b: (u8, bool)) -> SlotState {
        SlotState {
            active,
            slots: [
                SlotInfo {
                    tries_left: a.0,
                    successful: a.1,
                },
                SlotInfo {
                    tries_left: b.0,
                    successful: b.1,
                },
            ],
        }
    }

    #[test]
    fn the_first_boot_moves_pinnas_record() {
        // The owner's devices: Kaeru boots A, Pinna's root runs in b.
        let p = plan(
            Some(Slot::A),
            Some(rec(0x8f, 0x00)),
            Some(pinna(Slot::B, (2, true), (2, true))),
        );
        assert_eq!(p.slot, Slot::B);
        assert_eq!(p.write, Some(rec(0x8e, 0x8f)));
        assert!(p.clear_pinna);
        // Nothing good in it: Kaeru's record stays, LK's slot boots.
        let p = plan(
            Some(Slot::A),
            Some(rec(0x8f, 0x00)),
            Some(pinna(Slot::B, (0, false), (1, false))),
        );
        assert_eq!((p.slot, p.write, p.clear_pinna), (Slot::A, None, false));
    }
}
