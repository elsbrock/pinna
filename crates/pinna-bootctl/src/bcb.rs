//! Kaeru's A/B slot record in `misc` at 0x360: the record the Echo's
//! preloader counts down and Kaeru's LK boots by (ADR 0019). One slot holds a
//! kernel (`boot_a`/`boot_b`) and a root (`system_a`/`system_b`) together.
//!
//! ```text
//! off len field
//!  0   4  magic 00 'A' 'B' 'B' (0x42424100, little-endian)
//!  4   1  version = 1
//!  5   1  slot a: bits 0-3 priority, 4-6 tries, 7 good
//!  6   1  slot b: the same
//! ```
//!
//! Measured on the Echo Dot (2nd gen) with Kaeru `ab93750` (2026-11-17), from
//! A good at priority 14 and B at priority 15 with 3 tries:
//!
//! | boot | kernel | B after the boot |
//! | ---- | ------ | ---------------- |
//! | 1    | boot_b | `2f`: 2 tries    |
//! | 2    | boot_b | `1f`: 1 try      |
//! | 3    | boot_a | `0f`: no tries   |
//! | 4    | boot_a | `00`: retired    |
//!
//! The preloader decrements before LK runs, and LK doesn't count, so N tries
//! give a slot N - 1 attempts. A record without a good slot stops the
//! preloader before Kaeru runs, and only the BootROM tool brings the device
//! back: [`write`] refuses one.

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::slot::Slot;

pub const OFFSET: u64 = 0x360;
pub const LEN: usize = 7;
const HEADER: [u8; 5] = [0, b'A', b'B', b'B', 1];
const SECTOR: u64 = 512;
/// Tries fit in three bits.
pub const MAX_TRIES: u8 = 7;
/// The slot that boots next, and the one it falls back to.
pub const PRIORITY_ACTIVE: u8 = 15;
pub const PRIORITY_FALLBACK: u8 = 14;
/// Boot attempts a slot on trial gets; the record stores one try more,
/// because the preloader takes one before LK looks (see above).
pub const DEFAULT_ATTEMPTS: u8 = 3;
pub const MAX_ATTEMPTS: u8 = MAX_TRIES - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotMeta {
    pub priority: u8,
    pub tries: u8,
    pub good: bool,
}

impl SlotMeta {
    pub const EMPTY: SlotMeta = SlotMeta {
        priority: 0,
        tries: 0,
        good: false,
    };

    fn byte(self) -> u8 {
        (self.priority & 0x0f) | ((self.tries & 0x07) << 4) | (u8::from(self.good) << 7)
    }

    fn from_byte(b: u8) -> Self {
        Self {
            priority: b & 0x0f,
            tries: (b >> 4) & 0x07,
            good: b & 0x80 != 0,
        }
    }

    /// Boots that would still start this slot while it is on trial: the
    /// preloader takes a try and LK needs one left, so one less than its tries.
    pub fn attempts_left(self) -> u8 {
        if self.good || self.priority == 0 {
            0
        } else {
            self.tries.saturating_sub(1)
        }
    }

    /// What the slot is, in the words of `pinna-bootctl slot-status` and the
    /// dashboard.
    pub fn state(self) -> &'static str {
        if self.good {
            "good"
        } else if self.tries > 0 && self.priority > 0 {
            "trial"
        } else {
            "unbootable"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub slots: [SlotMeta; 2],
}

impl Record {
    /// What Kaeru writes for a missing or broken record, and for
    /// `fastboot set_active:a`: A good, B empty.
    pub const KAERU_DEFAULT: Record = Record {
        slots: [
            SlotMeta {
                priority: PRIORITY_ACTIVE,
                tries: 0,
                good: true,
            },
            SlotMeta::EMPTY,
        ],
    };

    pub fn get(&self, s: Slot) -> SlotMeta {
        self.slots[s as usize]
    }

    pub fn set(&mut self, s: Slot, m: SlotMeta) {
        self.slots[s as usize] = m;
    }

    pub fn encode(&self) -> [u8; LEN] {
        let mut b = [0u8; LEN];
        b[..5].copy_from_slice(&HEADER);
        b[5] = self.slots[0].byte();
        b[6] = self.slots[1].byte();
        b
    }

    /// `None` for anything that isn't a version 1 record.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < LEN || b[..5] != HEADER {
            return None;
        }
        Some(Self {
            slots: [SlotMeta::from_byte(b[5]), SlotMeta::from_byte(b[6])],
        })
    }

    pub fn has_good(&self) -> bool {
        self.slots.iter().any(|s| s.good)
    }

    /// The higher-priority slot first; A wins a tie.
    fn ranked(&self) -> (Slot, Slot) {
        if self.get(Slot::B).priority > self.get(Slot::A).priority {
            (Slot::B, Slot::A)
        } else {
            (Slot::A, Slot::B)
        }
    }

    /// The slot LK boots: Kaeru's `bcblib_bcb_get_active_slot(bcb, false,
    /// false)`. It counts nothing and never tries a slot that isn't good.
    pub fn lk_choice(&self) -> Option<Slot> {
        let (hi, lo) = self.ranked();
        let (h, l) = (self.get(hi), self.get(lo));
        if h.good || h.tries > 0 {
            Some(hi)
        } else if l.good {
            Some(lo)
        } else {
            None
        }
    }

    /// What the preloader does to the record before LK runs, as measured: a
    /// slot on trial loses a try, and a slot that is neither good nor has
    /// tries left is retired (priority 0) while the other one boots.
    pub fn preloader_step(&self) -> Record {
        let mut r = *self;
        let (hi, lo) = self.ranked();
        let h = self.get(hi);
        if h.good {
            return r;
        }
        if h.tries > 0 {
            r.set(
                hi,
                SlotMeta {
                    tries: h.tries - 1,
                    ..h
                },
            );
            return r;
        }
        r.set(hi, SlotMeta::EMPTY);
        let l = self.get(lo);
        if !l.good && l.tries > 0 {
            r.set(
                lo,
                SlotMeta {
                    tries: l.tries - 1,
                    ..l
                },
            );
        }
        r
    }

    /// The slot the next boot starts: the preloader's step, then LK's choice.
    pub fn next_boot(&self) -> Option<Slot> {
        self.preloader_step().lk_choice()
    }

    /// One line per slot, for humans and the VM tests: `trial attempts=2
    /// (priority 15, tries 3)`.
    pub fn describe(&self, s: Slot) -> String {
        let m = self.get(s);
        format!(
            "{} attempts={} (priority {}, tries {})",
            m.state(),
            m.attempts_left(),
            m.priority,
            m.tries
        )
    }
}

/// Boot `target` next, on trial with `attempts` boot attempts; the booted
/// slot stays good as its fallback. Refused unless the booted slot (the other
/// one) is good.
pub fn set_active(r: Record, target: Slot, attempts: u8) -> Result<Record> {
    ensure!(
        (1..=MAX_ATTEMPTS).contains(&attempts),
        "attempts must be 1 to {MAX_ATTEMPTS}, not {attempts}"
    );
    let tries = attempts + 1;
    let fallback = r.get(target.other());
    ensure!(
        fallback.good,
        "slot {} is not good: slot {target} would have no fallback",
        target.other()
    );
    let mut n = r;
    n.set(
        target,
        SlotMeta {
            priority: PRIORITY_ACTIVE,
            tries,
            good: false,
        },
    );
    n.set(
        target.other(),
        SlotMeta {
            priority: PRIORITY_FALLBACK,
            tries: 0,
            good: true,
        },
    );
    Ok(n)
}

/// The health check passed: the booted slot is good. The priorities stay, so
/// a pending switch stays pending.
pub fn mark_good(r: Record, booted: Slot) -> Record {
    let mut n = r;
    let m = r.get(booted);
    n.set(
        booted,
        SlotMeta {
            priority: if m.priority == 0 {
                PRIORITY_ACTIVE
            } else {
                m.priority
            },
            tries: 0,
            good: true,
        },
    );
    n
}

/// Before `target` is overwritten: it must not boot meanwhile. The booted
/// slot must be good and boots next.
pub fn invalidate(r: Record, booted: Slot, target: Slot) -> Result<Record> {
    ensure!(booted != target, "slot {booted} is running");
    ensure!(
        r.get(booted).good,
        "the running slot {booted} is not good: slot {target} is its only fallback"
    );
    let mut n = r;
    n.set(target, SlotMeta::EMPTY);
    n.set(
        booted,
        SlotMeta {
            priority: PRIORITY_ACTIVE,
            ..r.get(booted)
        },
    );
    Ok(n)
}

/// Boot the running slot next again, as it is: undoes a pending switch. The
/// other slot keeps its tries and whether it is good.
pub fn keep_booted(r: Record, booted: Slot) -> Result<Record> {
    ensure!(
        r.get(booted).good,
        "the running slot {booted} is not good yet"
    );
    let mut n = r;
    n.set(
        booted,
        SlotMeta {
            priority: PRIORITY_ACTIVE,
            ..r.get(booted)
        },
    );
    let o = r.get(booted.other());
    if o.priority > 0 {
        n.set(
            booted.other(),
            SlotMeta {
                priority: PRIORITY_FALLBACK,
                ..o
            },
        );
    }
    Ok(n)
}

/// `failed` can't start its root: retire it, so the next boot starts the
/// other slot's kernel and root. Refused unless the other slot is good.
pub fn retire(r: Record, failed: Slot) -> Result<Record> {
    ensure!(
        r.get(failed.other()).good,
        "slot {} is not good: retiring slot {failed} would leave nothing to boot",
        failed.other()
    );
    let mut n = r;
    n.set(failed, SlotMeta::EMPTY);
    Ok(n)
}

/// Fastboot guard policy: arm when the booted slot is already good or there is
/// no good fallback. A trial boot with a good fallback relies on rollback
/// instead: an armed guard would turn its failure into fastboot.
pub fn guard_should_arm(r: Option<Record>, booted: Option<Slot>) -> bool {
    let Some(b) = booted else { return true };
    let r = r.unwrap_or(Record::KAERU_DEFAULT);
    r.get(b).good || !r.get(b.other()).good
}

/// Read the record; `Ok(None)` when it is missing or not a version 1 record.
pub fn read(misc: &Path) -> Result<Option<Record>> {
    let f = std::fs::File::open(misc).with_context(|| format!("opening {}", misc.display()))?;
    let mut b = [0u8; LEN];
    f.read_exact_at(&mut b, OFFSET)
        .with_context(|| format!("reading the slot record from {}", misc.display()))?;
    Ok(Record::decode(&b))
}

/// The record as LK sees it: a missing one counts as Kaeru's default.
pub fn read_or_default(misc: &Path) -> Result<Record> {
    Ok(read(misc)?.unwrap_or(Record::KAERU_DEFAULT))
}

/// Write the record and read it back from the device: its sector must hold
/// exactly what was written, and the sectors around it must be unchanged.
/// Refuses a record without a good slot.
pub fn write(misc: &Path, r: &Record) -> Result<()> {
    if !r.has_good() {
        bail!(
            "refusing to write a slot record without a good slot ({:02x?}): the preloader would \
             not start the device",
            r.encode()
        );
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(misc)
        .with_context(|| format!("opening {} for writing", misc.display()))?;
    let sector_start = OFFSET - OFFSET % SECTOR;
    let start = sector_start - SECTOR;
    let span = 3 * SECTOR as usize;
    let mut before = vec![0u8; span];
    f.read_exact_at(&mut before, start)
        .with_context(|| format!("{} is too small for the slot record", misc.display()))?;
    let at = (OFFSET - sector_start) as usize;
    let mut sector = before[SECTOR as usize..2 * SECTOR as usize].to_vec();
    sector[at..at + LEN].copy_from_slice(&r.encode());
    crate::ubi::write_at(&f, &sector, sector_start)?;
    f.sync_all()?;
    // Read back from the device, not the page cache.
    // SAFETY: plain fadvise on an open fd.
    unsafe {
        libc::posix_fadvise(
            f.as_raw_fd(),
            start as libc::off_t,
            span as libc::off_t,
            libc::POSIX_FADV_DONTNEED,
        );
    }
    let mut after = vec![0u8; span];
    f.read_exact_at(&mut after, start)?;
    let s = SECTOR as usize;
    ensure!(
        after[s..2 * s] == sector[..],
        "slot record readback mismatch"
    );
    ensure!(
        after[..s] == before[..s] && after[2 * s..] == before[2 * s..],
        "misc changed around the slot record"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn rec(b: [u8; 2]) -> Record {
        Record::decode(&[0, b'A', b'B', b'B', 1, b[0], b[1]]).unwrap()
    }

    #[test]
    fn codec_matches_the_measured_bytes() {
        // A good at 15 (the Dot as shipped by Kaeru), B empty.
        assert_eq!(rec([0x8f, 0x00]), Record::KAERU_DEFAULT);
        assert_eq!(
            rec([0x8e, 0x3f]).slots,
            [
                SlotMeta {
                    priority: 14,
                    tries: 0,
                    good: true
                },
                SlotMeta {
                    priority: 15,
                    tries: 3,
                    good: false
                }
            ]
        );
        for b in [[0x8f, 0x00], [0x8e, 0x3f], [0x8f, 0x8e], [0x8e, 0x0f]] {
            assert_eq!(rec(b).encode()[5..], b);
        }
        assert_eq!(Record::decode(&[0u8; LEN]), None);
        assert_eq!(Record::decode(&[0, b'A', b'B', b'B', 2, 0x8f, 0]), None);
    }

    #[test]
    fn replays_the_measured_rollback() {
        // From 8e 3f: boot_b, boot_b, boot_a, then B retired.
        let mut r = rec([0x8e, 0x3f]);
        let mut seen = vec![];
        for _ in 0..4 {
            r = r.preloader_step();
            seen.push((r.lk_choice(), r.encode()[6]));
        }
        assert_eq!(
            seen,
            [
                (Some(Slot::B), 0x2f),
                (Some(Slot::B), 0x1f),
                (Some(Slot::A), 0x0f),
                (Some(Slot::A), 0x00),
            ]
        );
        assert_eq!(r.encode()[5], 0x8e, "A is never counted");
    }

    #[test]
    fn next_boot_follows_priority_and_goodness() {
        assert_eq!(Record::KAERU_DEFAULT.next_boot(), Some(Slot::A));
        // The Echo (2nd gen) as found: both good, A higher.
        assert_eq!(rec([0x8f, 0x8e]).next_boot(), Some(Slot::A));
        assert_eq!(rec([0x8e, 0x8f]).next_boot(), Some(Slot::B));
        assert_eq!(rec([0x8e, 0x3f]).next_boot(), Some(Slot::B));
        assert_eq!(rec([0x8e, 0x0f]).next_boot(), Some(Slot::A));
        assert_eq!(rec([0x00, 0x00]).next_boot(), None);
    }

    #[test]
    fn transitions_keep_a_good_slot() {
        let booted_a = Record::KAERU_DEFAULT;
        // 2 attempts store 3 tries: the measured record.
        let trial = set_active(booted_a, Slot::B, 2).unwrap();
        assert_eq!(trial.encode()[5..], [0x8e, 0x3f]);
        assert_eq!(trial.next_boot(), Some(Slot::B));
        assert_eq!(trial.get(Slot::B).attempts_left(), 2);
        // No fallback, no trial.
        assert!(set_active(rec([0x8e, 0x3f]), Slot::A, 2).is_err());
        assert!(set_active(booted_a, Slot::B, 0).is_err());
        assert!(set_active(booted_a, Slot::B, MAX_ATTEMPTS + 1).is_err());

        let kept = keep_booted(trial, Slot::A).unwrap();
        assert_eq!(kept.next_boot(), Some(Slot::A));
        assert_eq!(kept.get(Slot::B).tries, 3, "the trial keeps its tries");

        let retired = retire(trial, Slot::B).unwrap();
        assert_eq!(retired.next_boot(), Some(Slot::A));
        assert!(retire(trial, Slot::A).is_err(), "B isn't good");

        let good = mark_good(trial, Slot::B);
        assert_eq!(good.encode()[5..], [0x8e, 0x8f]);
        assert_eq!(good.next_boot(), Some(Slot::B));

        let inv = invalidate(good, Slot::B, Slot::A).unwrap();
        assert_eq!(inv.encode()[5..], [0x00, 0x8f]);
        assert!(
            invalidate(trial, Slot::B, Slot::A).is_err(),
            "B is still on trial"
        );
        assert!(invalidate(good, Slot::B, Slot::B).is_err());
    }

    /// Boot the record `n` times as preloader and LK would.
    fn boots(mut r: Record, n: usize) -> Vec<Option<Slot>> {
        (0..n)
            .map(|_| {
                r = r.preloader_step();
                r.lk_choice()
            })
            .collect()
    }

    #[test]
    fn attempts_are_boots_of_the_new_slot() {
        let a = Record::KAERU_DEFAULT;
        let (b, back) = (Some(Slot::B), Some(Slot::A));
        assert_eq!(boots(set_active(a, Slot::B, 1).unwrap(), 2), [b, back]);
        assert_eq!(boots(set_active(a, Slot::B, 2).unwrap(), 3), [b, b, back]);
        let default = set_active(a, Slot::B, DEFAULT_ATTEMPTS).unwrap();
        assert_eq!(default.encode()[6], 0x4f);
        assert_eq!(boots(default, 4), [b, b, b, back]);
    }

    #[test]
    fn guard_policy() {
        // No pinna-init, or a good booted slot: arm.
        assert!(guard_should_arm(None, None));
        assert!(guard_should_arm(None, Some(Slot::A)));
        assert!(guard_should_arm(Some(rec([0x8e, 0x8f])), Some(Slot::B)));
        // A trial of b with a good a: rely on rollback.
        assert!(!guard_should_arm(Some(rec([0x8e, 0x2f])), Some(Slot::B)));
        // A trial without a good fallback: arm.
        assert!(guard_should_arm(Some(rec([0x00, 0x2f])), Some(Slot::B)));
    }

    #[test]
    fn mark_good_revives_a_retired_slot_that_booted() {
        let r = mark_good(rec([0x8f, 0x00]), Slot::B);
        assert_eq!(r.get(Slot::B).priority, PRIORITY_ACTIVE);
        assert!(r.get(Slot::B).good);
    }

    fn misc() -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 0x80000];
        data[..19].copy_from_slice(b"bootonce-bootloader");
        data[0x35f] = 0x77;
        data[0x367] = 0x88;
        data[0x400] = 0x99;
        f.write_all(&data).unwrap();
        f
    }

    #[test]
    fn writes_only_its_bytes_and_reads_back() {
        let f = misc();
        assert_eq!(read(f.path()).unwrap(), None);
        assert_eq!(read_or_default(f.path()).unwrap(), Record::KAERU_DEFAULT);
        let r = rec([0x8e, 0x3f]);
        write(f.path(), &r).unwrap();
        assert_eq!(read(f.path()).unwrap(), Some(r));
        let data = std::fs::read(f.path()).unwrap();
        assert_eq!(&data[..19], b"bootonce-bootloader");
        assert_eq!((data[0x35f], data[0x367], data[0x400]), (0x77, 0x88, 0x99));
    }

    #[test]
    fn refuses_a_record_without_a_good_slot() {
        let f = misc();
        let err = write(f.path(), &rec([0x0f, 0x3f])).unwrap_err();
        assert!(format!("{err:#}").contains("without a good slot"));
        assert_eq!(read(f.path()).unwrap(), None, "nothing was written");
    }
}
