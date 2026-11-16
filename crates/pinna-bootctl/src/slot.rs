//! A/B slot state in `misc` at offset 0x40000 (LK only uses the bootloader
//! message at 0 and 5 bytes at 0x360; from 0x1000 up the partition is free).
//!
//! ```text
//! off  len  field
//!  0    8   magic "PINNASLT"
//!  8    1   version = 1
//!  9    1   active slot: 'a' or 'b'
//! 10    1   slot a: tries_left
//! 11    1   slot a: successful (0/1)
//! 12    1   slot b: tries_left
//! 13    1   slot b: successful (0/1)
//! 14    2   reserved, zero
//! 16    4   crc32 (IEEE, le) of bytes 0..16
//! ```
//!
//! A missing or invalid record means "first flash": slot a, treated as good.
//! pinna-init reads it (and decrements tries) on every boot; pinna-bootctl
//! marks the booted slot good after the health check; the updater activates
//! the other slot for a trial boot.

use std::fmt;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

pub const STATE_OFFSET: u64 = 0x40000;
pub const MAGIC: &[u8; 8] = b"PINNASLT";
pub const VERSION: u8 = 1;
pub const RECORD_LEN: usize = 20;
const SECTOR: usize = 512;
/// Tries a newly activated slot gets unless told otherwise.
pub const DEFAULT_TRIES: u8 = 3;
/// Where pinna-init records what it booted.
pub const RUN_DIR: &str = "/run/pinna";

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
    fn idx(self) -> usize {
        self as usize
    }
    pub fn letter(self) -> char {
        match self {
            Slot::A => 'a',
            Slot::B => 'b',
        }
    }
    /// GPT partition name of the slot's root.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotInfo {
    pub tries_left: u8,
    pub successful: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotState {
    pub active: Slot,
    pub slots: [SlotInfo; 2],
}

impl SlotState {
    /// What a missing record stands for: slot a, good (first flash).
    pub fn implicit() -> Self {
        Self {
            active: Slot::A,
            slots: [
                SlotInfo {
                    tries_left: 0,
                    successful: true,
                },
                SlotInfo::default(),
            ],
        }
    }

    pub fn get(&self, s: Slot) -> SlotInfo {
        self.slots[s.idx()]
    }

    pub fn set(&mut self, s: Slot, info: SlotInfo) {
        self.slots[s.idx()] = info;
    }

    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[..8].copy_from_slice(MAGIC);
        b[8] = VERSION;
        b[9] = self.active.letter() as u8;
        for (i, s) in self.slots.iter().enumerate() {
            b[10 + 2 * i] = s.tries_left;
            b[11 + 2 * i] = s.successful.into();
        }
        let crc = crc32fast::hash(&b[..16]);
        b[16..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// `None` for anything that is not a valid record.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < RECORD_LEN || &b[..8] != MAGIC || b[8] != VERSION || b[14..16] != [0, 0] {
            return None;
        }
        if crc32fast::hash(&b[..16]).to_le_bytes() != b[16..20] {
            return None;
        }
        let active = match b[9] {
            b'a' => Slot::A,
            b'b' => Slot::B,
            _ => return None,
        };
        let mut slots = [SlotInfo::default(); 2];
        for (i, s) in slots.iter_mut().enumerate() {
            s.tries_left = b[10 + 2 * i];
            s.successful = match b[11 + 2 * i] {
                0 => false,
                1 => true,
                _ => return None,
            };
        }
        Some(Self { active, slots })
    }

    /// One line per slot, for humans and the VM tests.
    pub fn describe(&self, s: Slot) -> String {
        let i = self.get(s);
        let what = if i.successful {
            "good"
        } else if i.tries_left > 0 {
            "trial"
        } else {
            "unbootable"
        };
        format!("{what} tries={}", i.tries_left)
    }
}

/// Read the record; `Ok(None)` when it is missing or invalid.
pub fn read_state(misc: &Path) -> Result<Option<SlotState>> {
    let f = std::fs::File::open(misc).with_context(|| format!("opening {}", misc.display()))?;
    let mut b = [0u8; RECORD_LEN];
    f.read_exact_at(&mut b, STATE_OFFSET)
        .with_context(|| format!("reading slot state from {}", misc.display()))?;
    Ok(SlotState::decode(&b))
}

/// Write the record's sector, flush it to the device, drop it from the page
/// cache and read it back: the sector must hold exactly what was written and
/// the sectors around it must be unchanged.
pub fn write_state(misc: &Path, state: &SlotState) -> Result<()> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(misc)
        .with_context(|| format!("opening {} for writing", misc.display()))?;
    let start = STATE_OFFSET - SECTOR as u64;
    let span = 3 * SECTOR;
    let mut before = vec![0u8; span];
    f.read_exact_at(&mut before, start)
        .with_context(|| format!("{} is too small for the slot state", misc.display()))?;
    let mut sector = before[SECTOR..2 * SECTOR].to_vec();
    sector[..RECORD_LEN].copy_from_slice(&state.encode());
    f.write_all_at(&sector, STATE_OFFSET)?;
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
    ensure!(
        after[SECTOR..2 * SECTOR] == sector[..],
        "slot state readback mismatch"
    );
    ensure!(
        after[..SECTOR] == before[..SECTOR] && after[2 * SECTOR..] == before[2 * SECTOR..],
        "misc changed around the slot state"
    );
    Ok(())
}

/// What pinna-init boots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub slot: Slot,
    /// State to write before booting, if it changed.
    pub persist: Option<SlotState>,
    pub reason: String,
}

/// Slot choice for this boot (before any image is checked).
pub fn decide(state: Option<SlotState>) -> Decision {
    let Some(mut s) = state else {
        return Decision {
            slot: Slot::A,
            persist: None,
            reason: "no slot state (first flash): slot a".into(),
        };
    };
    let act = s.active;
    let other = act.other();
    let info = s.get(act);
    if info.successful {
        Decision {
            slot: act,
            persist: None,
            reason: format!("slot {act} is good"),
        }
    } else if info.tries_left > 0 {
        s.set(
            act,
            SlotInfo {
                tries_left: info.tries_left - 1,
                ..info
            },
        );
        Decision {
            slot: act,
            persist: Some(s),
            reason: format!(
                "trial boot of slot {act} ({} tries left after this one)",
                info.tries_left - 1
            ),
        }
    } else if s.get(other).successful {
        s.active = other;
        Decision {
            slot: other,
            persist: Some(s),
            reason: format!("slot {act} is out of tries: rolling back to slot {other}"),
        }
    } else {
        Decision {
            slot: act,
            persist: None,
            reason: format!(
                "slot {act} is out of tries and slot {other} is not good: trying {act} anyway"
            ),
        }
    }
}

/// The chosen slot's image did not verify; pinna-init boots the other one and
/// records the failed slot as unbootable.
pub fn after_bad_image(state: Option<SlotState>, failed: Slot) -> SlotState {
    let mut s = state.unwrap_or_else(SlotState::implicit);
    s.set(failed, SlotInfo::default());
    s.active = failed.other();
    s
}

/// The health check passed: the booted slot is good.
pub fn mark_good(state: Option<SlotState>, booted: Slot) -> SlotState {
    let mut s = state.unwrap_or_else(SlotState::implicit);
    let info = s.get(booted);
    s.set(
        booted,
        SlotInfo {
            successful: true,
            ..info
        },
    );
    s
}

/// Make `slot` active for a trial boot with `tries` attempts.
pub fn set_active(state: Option<SlotState>, slot: Slot, tries: u8) -> SlotState {
    let mut s = state.unwrap_or_else(SlotState::implicit);
    s.active = slot;
    s.set(
        slot,
        SlotInfo {
            tries_left: tries,
            successful: false,
        },
    );
    s
}

/// Boot the running slot next again, as it is: undoes a pending switch or
/// update. The other slot keeps its state.
pub fn keep_booted(state: Option<SlotState>, booted: Slot) -> SlotState {
    let mut s = state.unwrap_or_else(SlotState::implicit);
    s.active = booted;
    s
}

/// The signed image in a slot partition, checked as pinna-init checks it
/// before booting: the footer's signature and the top hash block.
pub fn image(
    partdir: &Path,
    s: Slot,
    key: &pinna_image::keys::VerifyingKey,
) -> Result<pinna_image::Footer> {
    let path = partdir.join(s.partname());
    let dev = std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let f = pinna_image::read_footer(&dev, key)?;
    pinna_image::check_top_hash(&dev, &f)?;
    Ok(f)
}

/// Before overwriting `target`: it must not count as bootable meanwhile, and
/// the booted slot stays active.
pub fn invalidate(state: Option<SlotState>, booted: Slot, target: Slot) -> SlotState {
    let mut s = state.unwrap_or_else(SlotState::implicit);
    s.set(target, SlotInfo::default());
    s.active = booted;
    s
}

/// Fastboot guard policy: arm when the booted slot is already good or there is
/// no good fallback. A trial boot of a new slot with a good fallback relies on
/// rollback instead (an armed guard would turn its failure into fastboot).
pub fn guard_should_arm(state: Option<SlotState>, booted: Option<Slot>) -> bool {
    let Some(b) = booted else { return true };
    let s = state.unwrap_or_else(SlotState::implicit);
    s.get(b).successful || !s.get(b.other()).successful
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
    use std::io::Write;

    fn st(active: Slot, a: (u8, bool), b: (u8, bool)) -> SlotState {
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
    fn record_roundtrip_and_rejects() {
        let s = st(Slot::B, (0, true), (2, false));
        let b = s.encode();
        assert_eq!(&b[..8], b"PINNASLT");
        assert_eq!(b[9], b'b');
        assert_eq!(SlotState::decode(&b), Some(s));
        assert_eq!(SlotState::decode(&[0u8; RECORD_LEN]), None);
        for i in 0..RECORD_LEN {
            let mut c = b;
            c[i] ^= 0x40;
            assert_eq!(SlotState::decode(&c), None, "flip at {i}");
        }
    }

    #[test]
    fn decisions() {
        // first flash
        let d = decide(None);
        assert_eq!((d.slot, d.persist), (Slot::A, None));
        // good active slot: no write
        let d = decide(Some(st(Slot::B, (0, true), (0, true))));
        assert_eq!((d.slot, d.persist), (Slot::B, None));
        // trial: decrement and persist
        let d = decide(Some(st(Slot::B, (0, true), (3, false))));
        assert_eq!(d.slot, Slot::B);
        assert_eq!(d.persist, Some(st(Slot::B, (0, true), (2, false))));
        // out of tries: roll back to the good slot
        let d = decide(Some(st(Slot::B, (0, true), (0, false))));
        assert_eq!(d.slot, Slot::A);
        assert_eq!(d.persist, Some(st(Slot::A, (0, true), (0, false))));
        // out of tries, nothing good: try anyway, no write
        let d = decide(Some(st(Slot::A, (0, false), (0, false))));
        assert_eq!((d.slot, d.persist), (Slot::A, None));
    }

    #[test]
    fn trial_runs_out_then_rolls_back() {
        let mut s = set_active(Some(st(Slot::A, (0, true), (0, false))), Slot::B, 2);
        let mut booted = vec![];
        for _ in 0..4 {
            let d = decide(Some(s));
            booted.push(d.slot);
            if let Some(p) = d.persist {
                s = p;
            }
        }
        assert_eq!(booted, [Slot::B, Slot::B, Slot::A, Slot::A]);
        assert_eq!(s, st(Slot::A, (0, true), (0, false)));
    }

    #[test]
    fn transitions() {
        assert_eq!(
            after_bad_image(None, Slot::A),
            st(Slot::B, (0, false), (0, false))
        );
        assert_eq!(mark_good(None, Slot::A), st(Slot::A, (0, true), (0, false)));
        // marking good never changes the active slot (a pending update stays)
        assert_eq!(
            mark_good(Some(st(Slot::B, (0, true), (3, false))), Slot::A),
            st(Slot::B, (0, true), (3, false))
        );
        assert_eq!(
            invalidate(Some(st(Slot::B, (0, true), (3, false))), Slot::A, Slot::B),
            st(Slot::A, (0, true), (0, false))
        );
        assert_eq!(
            set_active(None, Slot::B, 3),
            st(Slot::B, (0, true), (3, false))
        );
        // keeping the booted slot leaves both slots' state alone
        assert_eq!(
            keep_booted(Some(st(Slot::B, (0, true), (3, false))), Slot::A),
            st(Slot::A, (0, true), (3, false))
        );
        assert_eq!(keep_booted(None, Slot::A), SlotState::implicit());
    }

    #[test]
    fn guard_policy() {
        // no pinna-init / first flash / good slot: arm
        assert!(guard_should_arm(None, None));
        assert!(guard_should_arm(None, Some(Slot::A)));
        assert!(guard_should_arm(
            Some(st(Slot::B, (0, true), (0, true))),
            Some(Slot::B)
        ));
        // trial of b with good a: rely on rollback
        assert!(!guard_should_arm(
            Some(st(Slot::B, (0, true), (2, false))),
            Some(Slot::B)
        ));
        // trial without a good fallback: arm
        assert!(guard_should_arm(
            Some(st(Slot::B, (0, false), (2, false))),
            Some(Slot::B)
        ));
    }

    fn misc() -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 0x80000];
        data[..19].copy_from_slice(b"bootonce-bootloader");
        data[0x360..0x365].copy_from_slice(&[0, b'A', b'B', 1, 2]);
        data[STATE_OFFSET as usize - 1] = 0x77;
        data[STATE_OFFSET as usize + 512] = 0x88;
        f.write_all(&data).unwrap();
        f
    }

    #[test]
    fn write_and_read_back() {
        let f = misc();
        assert_eq!(read_state(f.path()).unwrap(), None);
        let s = st(Slot::B, (0, true), (3, false));
        write_state(f.path(), &s).unwrap();
        assert_eq!(read_state(f.path()).unwrap(), Some(s));
        let data = std::fs::read(f.path()).unwrap();
        assert_eq!(&data[..19], b"bootonce-bootloader");
        assert_eq!(&data[0x360..0x365], &[0, b'A', b'B', 1, 2]);
        assert_eq!(data[STATE_OFFSET as usize - 1], 0x77);
        assert_eq!(data[STATE_OFFSET as usize + 512], 0x88);
        assert!(
            data[STATE_OFFSET as usize + RECORD_LEN..STATE_OFFSET as usize + 512]
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn too_small_misc_is_an_error() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        assert!(read_state(f.path()).is_err());
        assert!(write_state(f.path(), &SlotState::implicit()).is_err());
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
