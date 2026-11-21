//! Android boot image (header v0) as consumed by the Amazon LK on the MT8163
//! Echo devices, and on the MT8516 ones in their 64-bit layout ([`arm64`],
//! ADR 0031).
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

/// The 64-bit layout LK takes on the MT8516 Echos (ADR 0031), as
/// lidar-mainline's `tools/lidarboot.py` writes it, which was checked there
/// against a stock Echo Plus (2nd gen) boot image:
///
/// ```text
/// kernel blob = MediaTek "KERNEL" header (512 bytes) ++ gzip(Image) ++ DTB ++ DTB ...
/// ```
///
/// LK gunzips the Image to [`arm64::KERNEL_ADDR`] and copies the appended
/// DTB whose first root `compatible` matches the board exactly to the tags
/// address. It takes the 64-bit path only when the command line starts with
/// [`arm64::BOOTOPT`]. That the decompressed kernel ends below BL31 is
/// checked on the uncompressed Image by `kernel/build.sh`.
pub mod arm64 {
    use super::{BOOT_LEVEL, Header, LEVEL_TAG, u32_at};
    use anyhow::{Result, bail, ensure};

    pub const KERNEL_ADDR: u32 = 0x4008_0000;
    /// LK copies the chosen DTB here (lidar-mainline's address).
    pub const TAGS_ADDR: u32 = 0x4E00_0000;
    pub const BOOTOPT: &str = "bootopt=64S3,32N2,64N2";
    /// The most of a DTB LK copies.
    pub const DTB_MAX: usize = 0x8_0000;
    const MTK_MAGIC: u32 = 0x5888_1688;
    const MTK_HEADER: usize = 512;
    const GZIP_MAGIC: [u8; 3] = [0x1f, 0x8b, 0x08];
    /// What LK matches the first root compatible against starts like this.
    const BOARD_VENDOR: &str = "amzn,";

    /// The header for a 64-bit image: the MT8163 one's ramdisk address
    /// (TECHO5 uses the same on the Echo Dot (3rd gen)), the 64-bit kernel
    /// and tags addresses and boot option.
    pub fn header() -> Header {
        Header {
            kernel_addr: KERNEL_ADDR,
            tags_addr: TAGS_ADDR,
            cmdline: default_cmdline().into_bytes(),
            ..Header::default()
        }
    }

    /// The header command line of a 64-bit image this tree packs.
    pub fn default_cmdline() -> String {
        format!("{BOOTOPT} {LEVEL_TAG}{BOOT_LEVEL}")
    }

    /// The MediaTek header, then the gzip'd Image, then the DTBs.
    pub fn kernel_blob(image_gz: &[u8], dtbs: &[&[u8]]) -> Result<Vec<u8>> {
        ensure!(
            image_gz.starts_with(&GZIP_MAGIC),
            "the kernel is not a gzip'd Image (Image.gz)"
        );
        ensure!(!dtbs.is_empty(), "no DTB: LK needs one to hand over");
        let mut payload = image_gz.to_vec();
        for (i, dtb) in dtbs.iter().enumerate() {
            let len = fdt_len(dtb).ok_or_else(|| anyhow::anyhow!("DTB {i} is not a DTB"))?;
            ensure!(
                len == dtb.len(),
                "DTB {i}: totalsize {len}, file {}",
                dtb.len()
            );
            ensure!(
                len <= DTB_MAX,
                "DTB {i} is larger than LK copies ({DTB_MAX} bytes)"
            );
            let compat = root_compatibles(dtb).unwrap_or_default();
            match compat.first() {
                Some(c) if c.starts_with(BOARD_VENDOR) => {}
                _ => bail!(
                    "DTB {i}: LK matches the first root compatible against the board, \
                     and {compat:?} does not start with {BOARD_VENDOR:?}"
                ),
            }
            payload.extend_from_slice(dtb);
        }
        let mut blob = Vec::with_capacity(MTK_HEADER + payload.len());
        blob.extend_from_slice(&MTK_MAGIC.to_le_bytes());
        blob.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut name = [0u8; 32];
        name[..6].copy_from_slice(b"KERNEL");
        blob.extend_from_slice(&name);
        blob.resize(MTK_HEADER, 0xff);
        blob.extend_from_slice(&payload);
        Ok(blob)
    }

    /// Split a 64-bit kernel blob into the gzip'd Image and its DTBs.
    /// `None` when the blob doesn't have this layout.
    pub fn split_kernel_blob(blob: &[u8]) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
        if blob.len() < MTK_HEADER || u32_at(blob, 0) != MTK_MAGIC {
            return None;
        }
        let payload = blob.get(MTK_HEADER..MTK_HEADER + u32_at(blob, 4) as usize)?;
        if !payload.starts_with(&GZIP_MAGIC) {
            return None;
        }
        // The DTBs are the first run of whole DTBs that ends exactly at the
        // end of the payload; a DTB magic inside the gzip stream doesn't.
        let magic = 0xD00D_FEEDu32.to_be_bytes();
        (GZIP_MAGIC.len()..payload.len())
            .filter(|&at| payload[at..].starts_with(&magic))
            .find_map(|at| {
                let mut dtbs = Vec::new();
                let mut pos = at;
                while pos < payload.len() {
                    let len = fdt_len(&payload[pos..])?;
                    dtbs.push(payload[pos..pos + len].to_vec());
                    pos += len;
                }
                Some((payload[..at].to_vec(), dtbs))
            })
    }

    /// A DTB's totalsize, when `b` starts with a plausible one that fits.
    fn fdt_len(b: &[u8]) -> Option<usize> {
        let be = |off: usize| Some(u32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?));
        if be(0)? != 0xD00D_FEED {
            return None;
        }
        let total = be(4)? as usize;
        let (off_struct, off_strings) = (be(8)? as usize, be(12)? as usize);
        let ok = total >= 40 && total <= b.len() && off_struct < total && off_strings < total;
        ok.then_some(total)
    }

    /// The root node's `compatible` strings.
    pub fn root_compatibles(dtb: &[u8]) -> Option<Vec<String>> {
        let be = |off: usize| Some(u32::from_be_bytes(dtb.get(off..off + 4)?.try_into().ok()?));
        let (off_struct, off_strings) = (be(8)? as usize, be(12)? as usize);
        let align = |p: usize| (p + 3) & !3;
        let mut pos = off_struct;
        if be(pos)? != 1 {
            return None; // not FDT_BEGIN_NODE
        }
        let name_end = pos + 4 + dtb.get(pos + 4..)?.iter().position(|&c| c == 0)?;
        pos = align(name_end + 1);
        loop {
            match be(pos)? {
                4 => pos += 4, // FDT_NOP
                3 => {
                    let (len, name_off) = (be(pos + 4)? as usize, be(pos + 8)? as usize);
                    let names = dtb.get(off_strings + name_off..)?;
                    let name = &names[..names.iter().position(|&c| c == 0)?];
                    let value = dtb.get(pos + 12..pos + 12 + len)?;
                    if name == b"compatible" {
                        return Some(
                            value
                                .split(|&c| c == 0)
                                .filter(|s| !s.is_empty())
                                .map(|s| String::from_utf8_lossy(s).into_owned())
                                .collect(),
                        );
                    }
                    pos = align(pos + 12 + len);
                }
                // A child node or the end: the root has no compatible.
                _ => return Some(Vec::new()),
            }
        }
    }
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

/// The header's command line field: LK passes its text on in the device tree.
const CMDLINE: std::ops::Range<usize> = 64..576;
/// What marks the boot partition an image was written to (ADR 0019).
const SLOT_TAG: &str = "pinna.boot=";
/// What a boot image provides to the root it starts (ADR 0022).
const LEVEL_TAG: &str = "pinna.level=";

/// The boot level of the boot images this tree builds: what pinna-init and
/// the kernel provide to a root (ADR 0022). Raise it, and the root's need in
/// `pinna_image::ROOT_BOOT_LEVEL`, only when a root comes to depend on
/// something new here: satellites whose bootloader keeps the slot's kernel
/// can then no longer update over the network.
pub const BOOT_LEVEL: u32 = 1;

/// The header command line of a boot image this tree packs.
pub fn default_cmdline() -> String {
    format!("{BOOTOPT} {LEVEL_TAG}{BOOT_LEVEL}")
}

/// The boot level an image provides: `pinna.level=` in its header's command
/// line, 1 for an image without one (all from before ADR 0022). `None` when
/// it isn't a boot image.
pub fn level(image: &[u8]) -> Option<u32> {
    if image.len() < CMDLINE.end || &image[..8] != MAGIC {
        return None;
    }
    let line = cstr(&image[CMDLINE]);
    Some(
        String::from_utf8_lossy(&line)
            .split_whitespace()
            .find_map(|w| w.strip_prefix(LEVEL_TAG)?.parse().ok())
            .unwrap_or(1),
    )
}

/// Mark the image as written to boot partition `slot` (`a` or `b`):
/// `pinna.boot=<slot>` in the header's command line, replacing an earlier
/// mark. Kernel, ramdisk and the header's id stay as they are; the kernel
/// ignores the line, but LK passes it on, so pinna-init sees which boot
/// partition LK really loaded.
pub fn tag_slot(image: &mut [u8], slot: char) -> Result<()> {
    ensure!(slot == 'a' || slot == 'b', "not a slot: {slot:?}");
    ensure!(
        image.len() >= CMDLINE.end && &image[..8] == MAGIC,
        "not a boot image (no ANDROID! magic)"
    );
    let old = String::from_utf8_lossy(&cstr(&image[CMDLINE])).into_owned();
    let mut words: Vec<&str> = old
        .split_whitespace()
        .filter(|w| !w.starts_with(SLOT_TAG))
        .collect();
    let tag = format!("{SLOT_TAG}{slot}");
    words.push(&tag);
    let line = words.join(" ");
    ensure!(
        line.len() < CMDLINE.len(),
        "the header's command line is full"
    );
    image[CMDLINE].fill(0);
    image[CMDLINE.start..CMDLINE.start + line.len()].copy_from_slice(line.as_bytes());
    Ok(())
}

/// The boot partition an image is marked for, if any.
pub fn slot_tag(image: &[u8]) -> Option<char> {
    let line = cstr(image.get(CMDLINE)?);
    String::from_utf8_lossy(&line)
        .split_whitespace()
        .find_map(|w| w.strip_prefix(SLOT_TAG)?.chars().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_from_the_command_line() {
        let packed = |cmdline: &str| {
            let img = BootImage {
                header: Header {
                    cmdline: cmdline.as_bytes().to_vec(),
                    ..Header::default()
                },
                kernel: vec![1; 16],
                ramdisk: vec![2; 16],
                second: Vec::new(),
                id: [0; 32],
            };
            img.to_bytes(None).unwrap()
        };
        assert_eq!(level(&packed(BOOTOPT)), Some(1));
        let mut img = packed(&default_cmdline());
        assert_eq!(level(&img), Some(BOOT_LEVEL));
        assert_eq!(level(&packed(&format!("{BOOTOPT} pinna.level=3"))), Some(3));
        // Marking the slot keeps the level.
        tag_slot(&mut img, 'b').unwrap();
        assert_eq!(level(&img), Some(BOOT_LEVEL));
        assert_eq!(slot_tag(&img), Some('b'));
        assert_eq!(level(b"not a boot image"), None);
    }

    #[test]
    fn tags_the_slot_in_the_header_only() {
        let img = BootImage {
            header: Header::default(),
            kernel: kernel_blob(&fake_zimage(5000), &fake_dtb(300)).unwrap(),
            ramdisk: vec![7u8; 1000],
            second: Vec::new(),
            id: [0; 32],
        };
        let original = img.to_bytes(None).unwrap();
        assert_eq!(slot_tag(&original), None);
        let mut tagged = original.clone();
        tag_slot(&mut tagged, 'b').unwrap();
        assert_eq!(slot_tag(&tagged), Some('b'));
        let parsed = BootImage::parse(&tagged).unwrap();
        assert_eq!(
            parsed.header.cmdline,
            format!("{BOOTOPT} pinna.boot=b").into_bytes()
        );
        // Re-tagging replaces the mark; nothing past the field changes.
        tag_slot(&mut tagged, 'a').unwrap();
        assert_eq!(
            BootImage::parse(&tagged).unwrap().header.cmdline,
            format!("{BOOTOPT} pinna.boot=a").into_bytes()
        );
        assert_eq!(tagged[CMDLINE.end..], original[CMDLINE.end..]);
        assert_eq!(tagged[..CMDLINE.start], original[..CMDLINE.start]);
        assert!(tag_slot(&mut tagged, 'c').is_err());
        assert!(tag_slot(&mut [0u8; 600], 'a').is_err());
    }

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

    /// A minimal DTB whose root node has `compatible` (and one more property).
    fn dtb_with(compatible: &[&str]) -> Vec<u8> {
        let be = |v: u32| v.to_be_bytes();
        let value: Vec<u8> = compatible
            .iter()
            .flat_map(|c| [c.as_bytes(), b"\0"].concat())
            .collect();
        let strings = b"model\0compatible\0";
        let mut st = Vec::new();
        st.extend_from_slice(&be(1)); // BEGIN_NODE ""
        st.extend_from_slice(&[0; 4]);
        for (name_off, val) in [(0u32, &b"m\0"[..]), (6, &value[..])] {
            st.extend_from_slice(&be(3)); // PROP
            st.extend_from_slice(&be(val.len() as u32));
            st.extend_from_slice(&be(name_off));
            st.extend_from_slice(val);
            while st.len() % 4 != 0 {
                st.push(0);
            }
        }
        st.extend_from_slice(&be(2)); // END_NODE
        st.extend_from_slice(&be(9)); // END
        let off_mem = 40;
        let off_struct = off_mem + 16;
        let off_strings = off_struct + st.len();
        let total = off_strings + strings.len();
        let mut d = Vec::new();
        for v in [
            0xD00D_FEED,
            total as u32,
            off_struct as u32,
            off_strings as u32,
            off_mem as u32,
            17,
            16,
            0,
            strings.len() as u32,
            st.len() as u32,
        ] {
            d.extend_from_slice(&be(v));
        }
        d.extend_from_slice(&[0; 16]);
        d.extend_from_slice(&st);
        d.extend_from_slice(strings);
        assert_eq!(d.len(), total);
        d
    }

    #[test]
    fn arm64_blob_has_the_mediatek_header_the_image_and_the_dtbs() {
        // A gzip stream with a DTB magic inside it, which must not be taken
        // for the first DTB.
        let mut gz = vec![0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0];
        gz.extend_from_slice(&0xD00D_FEEDu32.to_be_bytes());
        gz.extend_from_slice(&[7; 21]);
        let pvt = dtb_with(&["amzn,donut-pvt", "amzn,donut", "mediatek,mt8516"]);
        let evt = dtb_with(&["amzn,donut-evt", "amzn,donut"]);
        let blob = arm64::kernel_blob(&gz, &[&pvt, &evt]).unwrap();

        assert_eq!(&blob[..4], &[0x88, 0x16, 0x88, 0x58]);
        assert_eq!(u32_at(&blob, 4) as usize, gz.len() + pvt.len() + evt.len());
        assert_eq!(&blob[8..15], b"KERNEL\0");
        assert!(blob[40..512].iter().all(|&b| b == 0xff));
        assert_eq!(&blob[512..512 + gz.len()], &gz[..]);

        let (image, dtbs) = arm64::split_kernel_blob(&blob).unwrap();
        assert_eq!(image, gz);
        assert_eq!(dtbs, vec![pvt.clone(), evt]);
        assert_eq!(arm64::root_compatibles(&pvt).unwrap()[0], "amzn,donut-pvt");
        // The MT8163 layout isn't taken for this one.
        assert!(
            arm64::split_kernel_blob(&kernel_blob(&fake_zimage(4096), &fake_dtb(64)).unwrap())
                .is_none()
        );
    }

    #[test]
    fn arm64_blob_refuses_what_lk_would_not_boot() {
        let gz = [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0];
        let pvt = dtb_with(&["amzn,donut-pvt"]);
        assert!(arm64::kernel_blob(&[0u8; 8], &[&pvt]).is_err(), "not gzip");
        assert!(arm64::kernel_blob(&gz, &[]).is_err(), "no DTB");
        let pumpkin = dtb_with(&["mediatek,mt8516-pumpkin", "mediatek,mt8516"]);
        assert!(
            arm64::kernel_blob(&gz, &[&pumpkin]).is_err(),
            "LK matches amzn,<board>"
        );
        let mut short = pvt.clone();
        short.pop();
        assert!(arm64::kernel_blob(&gz, &[&short]).is_err(), "truncated DTB");
    }

    #[test]
    fn arm64_header_takes_the_64_bit_path() {
        let h = arm64::header();
        assert_eq!(h.kernel_addr, 0x4008_0000);
        assert_eq!(h.ramdisk_addr, RAMDISK_ADDR);
        assert!(String::from_utf8_lossy(&h.cmdline).starts_with("bootopt=64S3,32N2,64N2 "));
        let img = BootImage {
            header: h,
            kernel: vec![1; 16],
            ramdisk: vec![2; 16],
            second: Vec::new(),
            id: [0; 32],
        };
        let mut bytes = img.to_bytes(None).unwrap();
        assert_eq!(level(&bytes), Some(BOOT_LEVEL));
        tag_slot(&mut bytes, 'b').unwrap();
        assert_eq!(slot_tag(&bytes), Some('b'));
        assert_eq!(
            BootImage::parse(&bytes).unwrap().header.kernel_addr,
            0x4008_0000
        );
    }
}
