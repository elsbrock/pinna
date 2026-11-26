//! `misc` as a UBI volume, on the Echo Dot (5th gen) (ADR 0035). A volume's
//! character device takes no plain writes: the kernel replaces a whole LEB
//! instead (`UBI_IOCEBCH`), atomically, so a power cut leaves the old or the
//! new contents. `misc` there is one LEB, and the slot record and the
//! bootloader command are both in it.
//!
//! The other volumes are replaced whole: `UBI_IOCVOLUP` with the image's
//! exact size erases one, and the data then goes in, in order, until that
//! many bytes arrived. The updater and `write-volume` (the install from
//! TWRP, ADR 0035 Decision 10) both do that.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

use crate::storage::Parts;

/// `/dev/ubiN_M` for the volume named `name`, from `sys_ubi`
/// (`/sys/class/ubi`).
pub fn volume(sys_ubi: &Path, name: &str) -> Option<PathBuf> {
    let mut found: Vec<String> = fs::read_dir(sys_ubi)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let dev = e.file_name().to_string_lossy().into_owned();
            let n = fs::read_to_string(e.path().join("name")).ok()?;
            // A volume is `ubiN_M`; `ubiN` is the device itself.
            (dev.contains('_') && n.trim() == name).then_some(dev)
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|d| Path::new("/dev").join(d))
}

/// Starts the replacement of a UBI volume. A trait so a test can record the
/// call: the ioctl needs a real volume.
pub trait VolumeUpdate: Send + Sync {
    /// Erase the volume and expect exactly `size` bytes next.
    fn start(&self, dev: &File, size: u64) -> io::Result<()>;
}

/// `UBI_IOCVOLUP`, on the kernel's UBI volume character device.
pub struct UbiIoctl;

/// `_IOW('O', 0, __s64)` from `<mtd/ubi-user.h>`.
const UBI_IOCVOLUP: u32 = 0x4008_4F00;

impl VolumeUpdate for UbiIoctl {
    fn start(&self, dev: &File, size: u64) -> io::Result<()> {
        let bytes = i64::try_from(size).map_err(|_| io::ErrorKind::InvalidInput)?;
        // SAFETY: the request takes a pointer to an __s64, which `bytes` is.
        let r = unsafe { libc::ioctl(dev.as_raw_fd(), UBI_IOCVOLUP as _, &bytes) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// The node of the UBI volume `name` and what it holds (`data_bytes`), or why
/// it can't be written: a board with GPT partitions has no UBI volumes.
pub fn resolve(parts: &Parts, name: &str) -> Result<(PathBuf, u64)> {
    let Parts::Ubi(sys) = parts else {
        bail!(
            "this board's partitions are {:?}, not UBI volumes: write-volume is the Echo Dot (5th gen)'s install",
            parts.kind()
        );
    };
    let node = volume(sys, name).with_context(|| format!("no UBI volume named {name}"))?;
    let file = node.file_name().expect("a volume node has a name");
    let path = sys.join(file).join("data_bytes");
    let capacity = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?
        .trim()
        .parse()
        .with_context(|| format!("{} is not a number", path.display()))?;
    Ok((node, capacity))
}

/// Replace the volume at `node` with the next `size` bytes of `input`, then
/// read it back and return the sha256 of what it holds. `limit` is the most
/// the board's slot takes, `expected` the sha256 the readback must have.
///
/// Nothing is erased before the size is checked against the volume and the
/// limit. After that, an `input` that ends early leaves the volume in
/// UBI's update state (it holds no image until one is written in full), so
/// that error says so.
pub fn write_volume(
    node: &Path,
    capacity: u64,
    limit: Option<u64>,
    size: u64,
    expected: Option<&str>,
    input: &mut dyn Read,
    ubi: &dyn VolumeUpdate,
) -> Result<String> {
    ensure!(size > 0, "size is 0");
    ensure!(
        size <= capacity,
        "{size} bytes do not fit {}, which holds {capacity}",
        node.display()
    );
    if let Some(limit) = limit {
        ensure!(
            size <= limit,
            "{size} bytes are more than this board's slot takes ({limit})"
        );
    }
    let mut dev = OpenOptions::new()
        .read(true)
        .write(true)
        .open(node)
        .with_context(|| format!("opening {} for writing", node.display()))?;
    ubi.start(&dev, size)
        .with_context(|| format!("starting the update of {}", node.display()))?;
    let mut left = size;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = input.read(&mut buf[..want]).context("reading the image")?;
        ensure!(
            n > 0,
            "the image ended {left} bytes short of {size}: {} is left in UBI's update state and holds no image until it is written again in full",
            node.display()
        );
        dev.write_all(&buf[..n])
            .with_context(|| format!("writing {}", node.display()))?;
        left -= n as u64;
    }
    dev.sync_all()?;
    let sha = read_back(&mut dev, size)?;
    if let Some(want) = expected {
        ensure!(
            sha.eq_ignore_ascii_case(want),
            "{} reads back as sha256 {sha}, not {want}: write it again",
            node.display()
        );
    }
    Ok(sha)
}

/// The sha256 of the first `size` bytes of the volume, read from the start.
fn read_back(dev: &mut File, size: u64) -> Result<String> {
    dev.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = dev
            .read(&mut buf[..want])
            .context("reading the volume back")?;
        ensure!(n > 0, "the volume reads back {left} bytes short of {size}");
        hash.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(pinna_image::keys::hex(&hash.finalize()))
}

/// Write `data` at `offset` of `misc`: in place on a block device (or a file,
/// in the tests), as an atomic LEB change on a UBI volume.
pub fn write_at(f: &File, data: &[u8], offset: u64) -> Result<()> {
    if !f.metadata()?.file_type().is_char_device() {
        return Ok(f.write_all_at(data, offset)?);
    }
    let size = leb_size(f)?;
    let mut leb = vec![0u8; size];
    f.read_exact_at(&mut leb, 0)
        .context("reading the first LEB of the UBI volume")?;
    let leb = spliced(&leb, data, offset)?;
    let req = LebChangeReq {
        lnum: 0,
        bytes: i32::try_from(leb.len())?,
        dtype: 3, // UBI_UNKNOWN; the kernel ignores it
        padding: [0; 7],
    };
    // SAFETY: the request is a valid struct ubi_leb_change_req for the call.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), UBI_IOCEBCH as _, &req) };
    ensure!(
        r == 0,
        "starting the UBI LEB change: {}",
        std::io::Error::last_os_error()
    );
    // The kernel writes the LEB once it has all `bytes`.
    (&*f).write_all(&leb).context("writing the UBI LEB")?;
    Ok(())
}

/// The LEB with `data` at `offset`.
fn spliced(leb: &[u8], data: &[u8], offset: u64) -> Result<Vec<u8>> {
    let at = usize::try_from(offset)?;
    ensure!(
        at.checked_add(data.len())
            .is_some_and(|end| end <= leb.len()),
        "{} bytes at {offset:#x} reach past the first LEB ({} bytes)",
        data.len(),
        leb.len()
    );
    let mut b = leb.to_vec();
    b[at..at + data.len()].copy_from_slice(data);
    Ok(b)
}

/// The volume's LEB size, from sysfs by its device number.
fn leb_size(f: &File) -> Result<usize> {
    let rdev = f.metadata()?.rdev();
    let p = format!(
        "/sys/dev/char/{}:{}/usable_eb_size",
        libc::major(rdev),
        libc::minor(rdev)
    );
    let s = fs::read_to_string(&p).with_context(|| format!("{p}: not a UBI volume"))?;
    Ok(s.trim().parse()?)
}

/// `struct ubi_leb_change_req` from `<mtd/ubi-user.h>`.
#[repr(C)]
struct LebChangeReq {
    lnum: i32,
    bytes: i32,
    dtype: i8,
    padding: [i8; 7],
}

/// `_IOW('O', 2, __s32)`: `<mtd/ubi-user.h>` encodes the size of an `__s32`,
/// although the call takes a `struct ubi_leb_change_req`. With the struct's
/// size the kernel answers ENOTTY (Echo Dot (5th gen), 2026-11-26).
const UBI_IOCEBCH: u32 = 0x4004_4F02;

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_board::StorageKind;
    use std::sync::Mutex;

    /// Records the sizes it was asked to start an update with.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<u64>>);

    impl VolumeUpdate for Recorder {
        fn start(&self, _: &File, size: u64) -> io::Result<()> {
            self.0.lock().unwrap().push(size);
            Ok(())
        }
    }

    fn volume_file(len: usize) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        fs::write(f.path(), vec![0xbb; len]).unwrap();
        f
    }

    fn sha(data: &[u8]) -> String {
        pinna_image::keys::hex(&Sha256::digest(data))
    }

    #[test]
    fn write_volume_starts_with_the_exact_size_fills_in_order_and_reads_back() {
        let v = volume_file(4096);
        let rec = Recorder::default();
        let image = b"0123456789";
        let got = write_volume(
            v.path(),
            4096,
            Some(4096),
            10,
            Some(&sha(image)),
            &mut &image[..],
            &rec,
        )
        .unwrap();
        assert_eq!(got, sha(image));
        assert_eq!(*rec.0.lock().unwrap(), [10]);
        assert_eq!(&fs::read(v.path()).unwrap()[..10], image);
    }

    #[test]
    fn write_volume_refuses_a_size_the_volume_or_the_board_cannot_take_before_erasing() {
        let v = volume_file(64);
        let rec = Recorder::default();
        let data = [1u8; 32];
        let e = write_volume(v.path(), 16, None, 32, None, &mut &data[..], &rec).unwrap_err();
        assert!(format!("{e:#}").contains("do not fit"), "{e:#}");
        let e = write_volume(v.path(), 64, Some(8), 32, None, &mut &data[..], &rec).unwrap_err();
        assert!(format!("{e:#}").contains("slot takes"), "{e:#}");
        assert!(write_volume(v.path(), 64, None, 0, None, &mut &data[..], &rec).is_err());
        assert!(rec.0.lock().unwrap().is_empty(), "nothing was started");
    }

    #[test]
    fn write_volume_names_the_update_state_when_the_image_is_short() {
        let v = volume_file(64);
        let rec = Recorder::default();
        let data = [1u8; 5];
        let e = write_volume(v.path(), 64, None, 8, None, &mut &data[..], &rec).unwrap_err();
        let e = format!("{e:#}");
        assert!(
            e.contains("3 bytes short") && e.contains("update state"),
            "{e}"
        );
        assert_eq!(*rec.0.lock().unwrap(), [8], "it had started");
    }

    #[test]
    fn write_volume_fails_when_the_readback_is_not_the_checksum() {
        let v = volume_file(64);
        let rec = Recorder::default();
        let data = [1u8; 8];
        let e = write_volume(
            v.path(),
            64,
            None,
            8,
            Some(&sha(b"other")),
            &mut &data[..],
            &rec,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("reads back as sha256"), "{e:#}");
    }

    #[test]
    fn resolve_finds_the_volume_and_its_size_and_refuses_gpt() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("ubi0_7")).unwrap();
        fs::write(d.path().join("ubi0_7/name"), "boot_a\n").unwrap();
        fs::write(d.path().join("ubi0_7/data_bytes"), "13676544\n").unwrap();
        let ubi = Parts::Ubi(d.path().into());
        assert_eq!(
            resolve(&ubi, "boot_a").unwrap(),
            (PathBuf::from("/dev/ubi0_7"), 13_676_544)
        );
        assert!(resolve(&ubi, "boot_b").is_err());
        let e = resolve(&Parts::of(StorageKind::Gpt), "boot_a").unwrap_err();
        assert!(format!("{e:#}").contains("not UBI volumes"), "{e:#}");
    }

    #[test]
    fn the_volume_update_request_is_the_kernels() {
        // _IOW: write (1 << 30), the size of an __s64, type 'O', number 0.
        assert_eq!(UBI_IOCVOLUP, (1 << 30) | (8 << 16) | (u32::from(b'O') << 8));
    }

    #[test]
    fn finds_a_volume_by_name() {
        let d = tempfile::tempdir().unwrap();
        for (dev, name) in [("ubi0", ""), ("ubi0_6", "misc"), ("ubi0_7", "boot_a")] {
            fs::create_dir(d.path().join(dev)).unwrap();
            if !name.is_empty() {
                fs::write(d.path().join(dev).join("name"), format!("{name}\n")).unwrap();
            }
        }
        assert_eq!(volume(d.path(), "misc"), Some(PathBuf::from("/dev/ubi0_6")));
        assert_eq!(volume(d.path(), "system_b"), None);
        assert_eq!(volume(&d.path().join("missing"), "misc"), None);
    }

    #[test]
    fn splices_into_the_leb_and_refuses_past_it() {
        let leb = vec![0xffu8; 4096];
        let b = spliced(&leb, &[1, 2, 3], 0x360).unwrap();
        assert_eq!(&b[0x360..0x363], &[1, 2, 3]);
        assert!(b[..0x360].iter().chain(&b[0x363..]).all(|&x| x == 0xff));
        assert!(spliced(&leb, &[0; 2], 4095).is_err());
    }

    #[test]
    fn the_request_matches_the_kernel_struct() {
        assert_eq!(std::mem::size_of::<LebChangeReq>(), 16);
        // _IOW: write (1 << 30), the size of an __s32, type 'O', number 2.
        assert_eq!(
            UBI_IOCEBCH,
            (1 << 30) | (4 << 16) | (u32::from(b'O') << 8) | 2
        );
    }
}
