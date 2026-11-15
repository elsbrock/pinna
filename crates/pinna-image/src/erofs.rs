//! The one erofs superblock field a slot image needs: the filesystem size.
//!
//! `struct erofs_super_block` (fs/erofs/erofs_fs.h) lives at byte 1024:
//! `magic` (le32, +0), `blkszbits` (u8, +12), `blocks` (le32, +36).

use anyhow::{Result, ensure};

pub const SUPERBLOCK_OFFSET: usize = 1024;
pub const MAGIC: u32 = 0xE0F5_E1E2;
/// Slot images use 4 KiB filesystem blocks (the dm-verity data block size).
pub const BLKSZBITS: u8 = 12;

/// Size of the filesystem in bytes (`blocks << blkszbits`), from the first
/// bytes of the image (at least 1024 + 40).
pub fn fs_size(head: &[u8]) -> Result<u64> {
    ensure!(
        head.len() >= SUPERBLOCK_OFFSET + 40,
        "image too small for an erofs superblock"
    );
    let sb = &head[SUPERBLOCK_OFFSET..];
    let le32 = |off: usize| u32::from_le_bytes(sb[off..off + 4].try_into().unwrap());
    ensure!(le32(0) == MAGIC, "no erofs superblock");
    let bits = sb[12];
    ensure!(
        bits == BLKSZBITS,
        "erofs block size must be 4096 (blkszbits {bits})"
    );
    let blocks = le32(36);
    ensure!(blocks > 0, "erofs superblock says 0 blocks");
    Ok(u64::from(blocks) << bits)
}

#[cfg(test)]
pub(crate) fn fake_superblock(blocks: u32) -> Vec<u8> {
    let mut b = vec![0u8; 4096];
    b[SUPERBLOCK_OFFSET..SUPERBLOCK_OFFSET + 4].copy_from_slice(&MAGIC.to_le_bytes());
    b[SUPERBLOCK_OFFSET + 12] = BLKSZBITS;
    b[SUPERBLOCK_OFFSET + 36..SUPERBLOCK_OFFSET + 40].copy_from_slice(&blocks.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_from_superblock() {
        assert_eq!(fs_size(&fake_superblock(38266)).unwrap(), 38266 * 4096);
    }

    #[test]
    fn rejects_other_data() {
        assert!(fs_size(&[0u8; 4096]).is_err());
        let mut sb = fake_superblock(1);
        sb[SUPERBLOCK_OFFSET + 12] = 9;
        assert!(fs_size(&sb).is_err());
        assert!(fs_size(&fake_superblock(0)).is_err());
    }
}
