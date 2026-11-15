//! A dm-verity device through the device-mapper ioctl interface
//! (`/dev/mapper/control`, include/uapi/linux/dm-ioctl.h): DM_DEV_CREATE,
//! DM_TABLE_LOAD (read-only, one `verity` target), DM_DEV_SUSPEND to resume.

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;

use anyhow::{Context, Result, ensure};
use pinna_image::{Footer, keys::hex};

const DM_NAME_LEN: usize = 128;
const DM_UUID_LEN: usize = 129;
const DM_READONLY_FLAG: u32 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct DmIoctl {
    version: [u32; 3],
    data_size: u32,
    data_start: u32,
    target_count: u32,
    open_count: i32,
    flags: u32,
    event_nr: u32,
    padding: u32,
    dev: u64,
    name: [u8; DM_NAME_LEN],
    uuid: [u8; DM_UUID_LEN],
    data: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DmTargetSpec {
    sector_start: u64,
    length: u64,
    status: i32,
    next: u32,
    target_type: [u8; 16],
}

const HDR: usize = size_of::<DmIoctl>();
const SPEC: usize = size_of::<DmTargetSpec>();
const _: () = assert!(HDR == 312 && SPEC == 40);

/// `_IOWR(DM_IOCTL, nr, struct dm_ioctl)` (asm-generic encoding: ARM, x86).
const fn iowr(nr: u32) -> u32 {
    (3 << 30) | ((HDR as u32) << 16) | (0xfd << 8) | nr
}
const DM_DEV_CREATE: u32 = iowr(3);
const DM_DEV_REMOVE: u32 = iowr(4);
const DM_DEV_SUSPEND: u32 = iowr(6);
const DM_TABLE_LOAD: u32 = iowr(9);

fn header(name: &str, flags: u32, data_size: usize) -> DmIoctl {
    let mut h = DmIoctl {
        version: [4, 0, 0],
        data_size: data_size as u32,
        data_start: HDR as u32,
        target_count: 0,
        open_count: 0,
        flags,
        event_nr: 0,
        padding: 0,
        dev: 0,
        name: [0; DM_NAME_LEN],
        uuid: [0; DM_UUID_LEN],
        data: [0; 7],
    };
    h.name[..name.len()].copy_from_slice(name.as_bytes());
    h
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: repr(C) structs without padding (checked sizes above).
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()) }
}

struct Control(File);

impl Control {
    fn open() -> Result<Self> {
        Ok(Self(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/mapper/control")
                .context("opening /dev/mapper/control")?,
        ))
    }

    /// Run one ioctl on `hdr` + `payload`; returns the header as the kernel left it.
    fn call(&self, req: u32, hdr: DmIoctl, payload: &[u8]) -> io::Result<DmIoctl> {
        let len = (HDR + payload.len()).next_multiple_of(8);
        let mut buf = vec![0u64; len / 8];
        // SAFETY: buf is len bytes, 8-aligned.
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), len) };
        bytes[..HDR].copy_from_slice(as_bytes(&hdr));
        bytes[HDR..HDR + payload.len()].copy_from_slice(payload);
        // SAFETY: the kernel reads and writes at most data_size (= len) bytes.
        let r = unsafe { libc::ioctl(self.0.as_raw_fd(), req as libc::Ioctl, buf.as_mut_ptr()) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: buf holds at least HDR initialized bytes.
        Ok(unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<DmIoctl>()) })
    }
}

/// `huge_encode_dev` -> (major, minor).
fn decode_dev(dev: u64) -> (u32, u32) {
    let d = dev as u32;
    ((d & 0xfff00) >> 8, (d & 0xff) | ((d >> 12) & 0xfff00))
}

/// The dm-verity table for a slot image: data and hash tree on the same device.
pub fn verity_params(dev: (u32, u32), f: &Footer) -> String {
    let d = format!("{}:{}", dev.0, dev.1);
    format!(
        "1 {d} {d} {} {} {} {} {} {} {}",
        f.data_block_size,
        f.hash_block_size,
        f.data_blocks(),
        f.hash_start_block(),
        f.algorithm,
        hex(&f.root_hash),
        hex(&f.salt)
    )
}

/// Create the read-only dm-verity device `name` over the slot `dev`; returns
/// the dm device's (major, minor).
pub fn create_verity(name: &str, dev: (u32, u32), f: &Footer) -> Result<(u32, u32)> {
    ensure!(name.len() < DM_NAME_LEN, "dm name too long");
    let ctl = Control::open()?;
    ctl.call(DM_DEV_CREATE, header(name, 0, HDR), &[])
        .with_context(|| format!("DM_DEV_CREATE {name}"))?;
    let res = (|| {
        let params = verity_params(dev, f);
        let next = (SPEC + params.len() + 1).next_multiple_of(8);
        let mut spec = DmTargetSpec {
            sector_start: 0,
            length: f.data_size / 512,
            status: 0,
            next: next as u32,
            target_type: [0; 16],
        };
        spec.target_type[..6].copy_from_slice(b"verity");
        let mut payload = as_bytes(&spec).to_vec();
        payload.extend_from_slice(params.as_bytes());
        payload.resize(next, 0);
        let mut hdr = header(name, DM_READONLY_FLAG, HDR + payload.len());
        hdr.target_count = 1;
        ctl.call(DM_TABLE_LOAD, hdr, &payload)
            .with_context(|| format!("DM_TABLE_LOAD verity {params}"))?;
        // DM_DEV_SUSPEND without DM_SUSPEND_FLAG resumes: the table goes live.
        let out = ctl
            .call(DM_DEV_SUSPEND, header(name, 0, HDR), &[])
            .context("DM_DEV_SUSPEND (resume)")?;
        Ok(decode_dev(out.dev))
    })();
    if res.is_err() {
        let _ = remove(name);
    }
    res
}

pub fn remove(name: &str) -> Result<()> {
    Control::open()?
        .call(DM_DEV_REMOVE, header(name, 0, HDR), &[])
        .with_context(|| format!("DM_DEV_REMOVE {name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers() {
        // from <linux/dm-ioctl.h> on arm/x86 (sizeof(struct dm_ioctl) == 312)
        assert_eq!(DM_DEV_CREATE, 0xc138_fd03);
        assert_eq!(DM_DEV_REMOVE, 0xc138_fd04);
        assert_eq!(DM_DEV_SUSPEND, 0xc138_fd06);
        assert_eq!(DM_TABLE_LOAD, 0xc138_fd09);
    }

    #[test]
    fn dev_decoding() {
        // new_encode_dev(MKDEV(254, 0)) and MKDEV(259, 300)
        assert_eq!(decode_dev(254 << 8), (254, 0));
        let (maj, min) = (259u64, 300u64);
        let enc = (min & 0xff) | (maj << 8) | ((min & !0xff) << 12);
        assert_eq!(decode_dev(enc), (259, 300));
    }

    #[test]
    fn table() {
        let f = Footer::new(300 * 4096, [0xab; 32], [0xcd; 32], "1.0").unwrap();
        let p = verity_params((179, 13), &f);
        assert_eq!(
            p,
            format!(
                "1 179:13 179:13 4096 4096 300 301 sha256 {} {}",
                "cd".repeat(32),
                "ab".repeat(32)
            )
        );
    }
}
