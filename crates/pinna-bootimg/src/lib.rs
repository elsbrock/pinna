//! Android boot image (header v0) as consumed by the Amazon LK on the MT8163
//! Echo devices.
//!
//! Layout, all sections page-aligned (page size 2048):
//!
//! ```text
//! [header page][kernel blob][ramdisk][second]
//! kernel blob = zImage ++ DTB (zero-padded to 64 KiB, totalsize rewritten)
//! ```
//!
//! The `id` field is SHA-1 over `kernel, len(kernel), ramdisk, len(ramdisk),
//! second, len(second)`, matching AOSP `mkbootimg` for v0 headers.

use anyhow::{Context, Result, bail, ensure};
use sha1::{Digest, Sha1};

pub const MAGIC: &[u8; 8] = b"ANDROID!";
pub const PAGE_SIZE: u32 = 0x800;
/// The LK boot partitions on the Dot are 16 MiB; images are padded to that.
pub const PARTITION_SIZE: usize = 0x100_0000;
pub const KERNEL_ADDR: u32 = 0x4000_8000;
pub const RAMDISK_ADDR: u32 = 0x4347_8000;
pub const SECOND_ADDR: u32 = 0x40F0_0000;
pub const TAGS_ADDR: u32 = 0x4800_0000;
/// LK only accepts the proven 32-bit boot path.
pub const BOOTOPT: &str = "bootopt=64S3,32N2,32N2";
/// DTBs are appended to the zImage, zero-padded to this size.
pub const DTB_SLOT: usize = 0x1_0000;
const ZIMAGE_MAGIC: u32 = 0x016F_2818;
const DTB_MAGIC: u32 = 0xD00D_FEED;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub kernel_addr: u32,
    pub ramdisk_addr: u32,
    pub second_addr: u32,
    pub tags_addr: u32,
    pub page_size: u32,
    /// v0: "unused"/dt_size field; must be 0 for LK.
    pub header_version: u32,
    pub os_version: u32,
    pub name: [u8; 16],
    pub cmdline: Vec<u8>,
    pub extra_cmdline: Vec<u8>,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            kernel_addr: KERNEL_ADDR,
            ramdisk_addr: RAMDISK_ADDR,
            second_addr: SECOND_ADDR,
            tags_addr: TAGS_ADDR,
            page_size: PAGE_SIZE,
            header_version: 0,
            os_version: 0,
            name: [0; 16],
            cmdline: BOOTOPT.as_bytes().to_vec(),
            extra_cmdline: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootImage {
    pub header: Header,
    pub kernel: Vec<u8>,
    pub ramdisk: Vec<u8>,
    pub second: Vec<u8>,
    /// The id stored in the header (as read); recomputed on write.
    pub id: [u8; 32],
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn pages(len: usize, page: usize) -> usize {
    len.div_ceil(page) * page
}

fn cstr(field: &[u8]) -> Vec<u8> {
    let end = field.iter().position(|&c| c == 0).unwrap_or(field.len());
    field[..end].to_vec()
}

impl BootImage {
    pub fn parse(data: &[u8]) -> Result<Self> {
        ensure!(data.len() >= 1632, "image too small");
        ensure!(&data[..8] == MAGIC, "missing ANDROID! magic");
        let kernel_size = u32_at(data, 8) as usize;
        let ramdisk_size = u32_at(data, 16) as usize;
        let second_size = u32_at(data, 24) as usize;
        let page = u32_at(data, 36) as usize;
        ensure!(
            page.is_power_of_two() && page >= 2048,
            "bad page size {page}"
        );
        let header = Header {
            kernel_addr: u32_at(data, 12),
            ramdisk_addr: u32_at(data, 20),
            second_addr: u32_at(data, 28),
            tags_addr: u32_at(data, 32),
            page_size: page as u32,
            header_version: u32_at(data, 40),
            os_version: u32_at(data, 44),
            name: data[48..64].try_into().unwrap(),
            cmdline: cstr(&data[64..576]),
            extra_cmdline: cstr(&data[608..1632]),
        };
        let id: [u8; 32] = data[576..608].try_into().unwrap();
        let k_off = page;
        let r_off = k_off + pages(kernel_size, page);
        let s_off = r_off + pages(ramdisk_size, page);
        ensure!(s_off + second_size <= data.len(), "image truncated");
        Ok(Self {
            header,
            kernel: data[k_off..k_off + kernel_size].to_vec(),
            ramdisk: data[r_off..r_off + ramdisk_size].to_vec(),
            second: data[s_off..s_off + second_size].to_vec(),
            id,
        })
    }

    pub fn compute_id(&self) -> [u8; 32] {
        let mut h = Sha1::new();
        for part in [&self.kernel, &self.ramdisk, &self.second] {
            h.update(part);
            h.update((part.len() as u32).to_le_bytes());
        }
        let mut id = [0u8; 32];
        id[..20].copy_from_slice(&h.finalize());
        id
    }

    /// Serialize, padding the output to `pad_to` bytes if given.
    pub fn to_bytes(&self, pad_to: Option<usize>) -> Result<Vec<u8>> {
        let h = &self.header;
        let page = h.page_size as usize;
        ensure!(h.cmdline.len() < 512, "cmdline too long");
        ensure!(h.extra_cmdline.len() < 1024, "extra cmdline too long");
        let mut out = vec![0u8; page];
        out[..8].copy_from_slice(MAGIC);
        let fields = [
            self.kernel.len() as u32,
            h.kernel_addr,
            self.ramdisk.len() as u32,
            h.ramdisk_addr,
            self.second.len() as u32,
            h.second_addr,
            h.tags_addr,
            h.page_size,
            h.header_version,
            h.os_version,
        ];
        for (i, v) in fields.iter().enumerate() {
            out[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        out[48..64].copy_from_slice(&h.name);
        out[64..64 + h.cmdline.len()].copy_from_slice(&h.cmdline);
        out[576..608].copy_from_slice(&self.compute_id());
        out[608..608 + h.extra_cmdline.len()].copy_from_slice(&h.extra_cmdline);
        for part in [&self.kernel, &self.ramdisk, &self.second] {
            if part.is_empty() {
                continue;
            }
            let start = out.len();
            out.extend_from_slice(part);
            out.resize(start + pages(part.len(), page), 0);
        }
        if let Some(size) = pad_to {
            ensure!(
                out.len() <= size,
                "boot image is {} bytes, exceeds {size}",
                out.len()
            );
            out.resize(size, 0);
        }
        Ok(out)
    }
}

/// Size of an ARM zImage as declared by its header (end - start).
pub fn zimage_len(z: &[u8]) -> Result<usize> {
    ensure!(z.len() >= 0x30, "zImage too small");
    ensure!(u32_at(z, 0x24) == ZIMAGE_MAGIC, "ARM zImage magic missing");
    let (start, end) = (u32_at(z, 0x28), u32_at(z, 0x2c));
    ensure!(end > start, "bad zImage start/end");
    Ok((end - start) as usize)
}

/// Zero-pad a DTB to the 64 KiB slot and rewrite its totalsize.
pub fn pad_dtb(dtb: &[u8]) -> Result<Vec<u8>> {
    ensure!(dtb.len() >= 40, "DTB too small");
    let magic = u32::from_be_bytes(dtb[..4].try_into().unwrap());
    ensure!(magic == DTB_MAGIC, "DTB magic missing");
    let total = u32::from_be_bytes(dtb[4..8].try_into().unwrap()) as usize;
    ensure!(total <= DTB_SLOT, "DTB larger than {DTB_SLOT} bytes");
    let mut out = dtb[..total.min(dtb.len())].to_vec();
    out.resize(DTB_SLOT, 0);
    out[4..8].copy_from_slice(&(DTB_SLOT as u32).to_be_bytes());
    Ok(out)
}

/// zImage ++ padded DTB, validating the zImage header against its length.
pub fn kernel_blob(zimage: &[u8], dtb: &[u8]) -> Result<Vec<u8>> {
    let declared = zimage_len(zimage)?;
    if declared != zimage.len() {
        bail!(
            "zImage end field ({declared}) does not match file size ({})",
            zimage.len()
        );
    }
    let mut blob = zimage.to_vec();
    blob.extend_from_slice(&pad_dtb(dtb)?);
    Ok(blob)
}

/// Split a kernel blob back into (zImage, DTB).
pub fn split_kernel_blob(blob: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    // Optional 512-byte MTK "KERNEL" header used by older images.
    let body = if blob.len() > 512 && blob[..4] == [0x88, 0x16, 0x88, 0x58] {
        &blob[512..]
    } else {
        blob
    };
    let zlen = zimage_len(body).context("kernel blob")?;
    ensure!(body.len() > zlen, "no DTB appended to zImage");
    Ok((body[..zlen].to_vec(), body[zlen..].to_vec()))
}

/// One member of a newc cpio archive (permissions in `perm`, type from the variant).
#[derive(Debug, Clone, Copy)]
pub enum CpioEntry<'a> {
    File {
        name: &'a str,
        perm: u32,
        data: &'a [u8],
    },
    Dir {
        name: &'a str,
        perm: u32,
    },
    CharDev {
        name: &'a str,
        perm: u32,
        major: u32,
        minor: u32,
    },
}

/// A newc cpio archive, padded to 512 bytes.
pub fn newc_cpio_entries(entries: &[CpioEntry]) -> Vec<u8> {
    fn entry(
        out: &mut Vec<u8>,
        ino: u32,
        name: &str,
        mode: u32,
        nlink: u32,
        rdev: (u32, u32),
        data: &[u8],
    ) {
        let hdr = format!(
            "070701{ino:08x}{mode:08x}{:08x}{:08x}{nlink:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            0,
            0,
            0,
            data.len(),
            0,
            0,
            rdev.0,
            rdev.1,
            name.len() + 1,
            0
        );
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let ino = i as u32 + 1;
        match *e {
            CpioEntry::File { name, perm, data } => {
                entry(&mut out, ino, name, 0o100000 | perm, 1, (0, 0), data)
            }
            CpioEntry::Dir { name, perm } => {
                entry(&mut out, ino, name, 0o040000 | perm, 2, (0, 0), &[])
            }
            CpioEntry::CharDev {
                name,
                perm,
                major,
                minor,
            } => entry(&mut out, ino, name, 0o020000 | perm, 1, (major, minor), &[]),
        }
    }
    entry(&mut out, 0, "TRAILER!!!", 0, 1, (0, 0), &[]);
    while !out.len().is_multiple_of(512) {
        out.push(0);
    }
    out
}

/// A minimal newc cpio archive containing the given regular files.
/// Used as a placeholder ramdisk: without an `/init` in it, the kernel falls
/// through to mounting `root=` itself.
pub fn newc_cpio(files: &[(&str, &[u8])]) -> Vec<u8> {
    let entries: Vec<CpioEntry> = files
        .iter()
        .map(|&(name, data)| CpioEntry::File {
            name,
            perm: 0o644,
            data,
        })
        .collect();
    newc_cpio_entries(&entries)
}

/// The initramfs: `/init` (pinna-init), the mount points it uses,
/// `/dev/console`, which the kernel opens for init's stdio, and optionally a
/// static busybox at `/bin/busybox` for rescue mode (ADR 0012).
pub fn initramfs(init: &[u8], busybox: Option<&[u8]>) -> Vec<u8> {
    let mut entries: Vec<CpioEntry> = ["dev", "proc", "sys", "run", "sysroot", "bin"]
        .into_iter()
        .map(|name| CpioEntry::Dir { name, perm: 0o755 })
        .collect();
    entries.push(CpioEntry::CharDev {
        name: "dev/console",
        perm: 0o600,
        major: 5,
        minor: 1,
    });
    entries.push(CpioEntry::File {
        name: "init",
        perm: 0o755,
        data: init,
    });
    if let Some(data) = busybox {
        entries.push(CpioEntry::File {
            name: "bin/busybox",
            perm: 0o755,
            data,
        });
    }
    newc_cpio_entries(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_zimage(len: usize) -> Vec<u8> {
        let mut z = vec![0xAAu8; len];
        z[0x24..0x28].copy_from_slice(&ZIMAGE_MAGIC.to_le_bytes());
        z[0x28..0x2c].copy_from_slice(&0u32.to_le_bytes());
        z[0x2c..0x30].copy_from_slice(&(len as u32).to_le_bytes());
        z
    }

    fn fake_dtb(len: usize) -> Vec<u8> {
        let mut d = vec![0u8; len];
        d[..4].copy_from_slice(&DTB_MAGIC.to_be_bytes());
        d[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        d
    }

    #[test]
    fn roundtrip() {
        let img = BootImage {
            header: Header::default(),
            kernel: kernel_blob(&fake_zimage(5000), &fake_dtb(300)).unwrap(),
            ramdisk: newc_cpio(&[("README", b"pinna")]),
            second: Vec::new(),
            id: [0; 32],
        };
        let bytes = img.to_bytes(Some(PARTITION_SIZE)).unwrap();
        assert_eq!(bytes.len(), PARTITION_SIZE);
        let parsed = BootImage::parse(&bytes).unwrap();
        assert_eq!(parsed.kernel, img.kernel);
        assert_eq!(parsed.ramdisk, img.ramdisk);
        assert_eq!(parsed.header, img.header);
        assert_eq!(parsed.id, img.compute_id());
        assert_eq!(parsed.to_bytes(Some(PARTITION_SIZE)).unwrap(), bytes);
        let (z, d) = split_kernel_blob(&parsed.kernel).unwrap();
        assert_eq!(z.len(), 5000);
        assert_eq!(d.len(), DTB_SLOT);
    }

    #[test]
    fn rejects_bad_zimage_length() {
        let mut z = fake_zimage(4000);
        z.truncate(3990);
        assert!(kernel_blob(&z, &fake_dtb(100)).is_err());
    }

    #[test]
    fn rejects_oversize() {
        let img = BootImage {
            header: Header::default(),
            kernel: vec![1; 3000],
            ramdisk: vec![],
            second: vec![],
            id: [0; 32],
        };
        assert!(img.to_bytes(Some(4096)).is_err());
    }

    #[test]
    fn cpio_is_aligned_and_terminated() {
        let c = newc_cpio(&[("a", b"xyz")]);
        assert_eq!(c.len() % 512, 0);
        assert!(c.windows(10).any(|w| w == b"TRAILER!!!"));
    }

    #[test]
    fn initramfs_members() {
        let c = initramfs(b"\x7fELF-init", Some(b"\x7fELF-busybox"));
        let text = String::from_utf8_lossy(&c);
        // mode field (2nd after the inode) per member
        let mode_of = |name: &str| {
            let at = text.find(&format!("{name}\0")).unwrap();
            let hdr = &text[at - 110..at];
            assert!(hdr.starts_with("070701"));
            (
                u32::from_str_radix(&hdr[14..22], 16).unwrap(),
                u32::from_str_radix(&hdr[78..86], 16).unwrap(),
                u32::from_str_radix(&hdr[86..94], 16).unwrap(),
            )
        };
        assert_eq!(mode_of("init"), (0o100755, 0, 0));
        assert_eq!(mode_of("sysroot"), (0o040755, 0, 0));
        assert_eq!(mode_of("dev/console"), (0o020600, 5, 1));
        assert!(text.contains("\x7fELF-init"));
        assert_eq!(mode_of("bin/busybox"), (0o100755, 0, 0));
        assert!(text.contains("\x7fELF-busybox"));
        assert_eq!(c.len() % 512, 0);
        let without = String::from_utf8_lossy(&initramfs(b"\x7fELF-init", None)).into_owned();
        assert!(!without.contains("bin/busybox"));
    }
}
