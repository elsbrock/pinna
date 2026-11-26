//! `misc` as a UBI volume, on the Echo Dot (5th gen) (ADR 0035). A volume's
//! character device takes no plain writes: the kernel replaces a whole LEB
//! instead (`UBI_IOCEBCH`), atomically, so a power cut leaves the old or the
//! new contents. `misc` there is one LEB, and the slot record and the
//! bootloader command are both in it.

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

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
