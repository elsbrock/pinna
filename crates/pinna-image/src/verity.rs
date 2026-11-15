//! dm-verity hash tree, format 1, without a verity superblock — the layout
//! `veritysetup format --no-superblock --format=1` writes and the kernel's
//! dm-verity target reads.
//!
//! Every hash is `sha256(salt || block)` over a 4 KiB block. Level 0 holds the
//! hashes of the data blocks, each next level the hashes of the previous
//! level's blocks, until one block remains; the root hash is the hash of that
//! block. Partly filled hash blocks are zero-padded. On disk the top level
//! comes first and level 0 last.

use std::io;

use sha2::{Digest, Sha256};

pub const BLOCK: usize = 4096;
pub const DIGEST: usize = 32;
const PER_BLOCK: u64 = (BLOCK / DIGEST) as u64;
/// Data is hashed in reads of this many blocks.
const CHUNK_BLOCKS: u64 = 256;

pub fn hash_block(salt: &[u8], block: &[u8]) -> [u8; DIGEST] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(block);
    h.finalize().into()
}

/// Hash blocks per level, level 0 first. Empty for a single data block (the
/// root hash is then the hash of the data block itself).
pub fn level_blocks(data_blocks: u64) -> Vec<u64> {
    let mut sizes = Vec::new();
    let mut n = data_blocks;
    while n > 1 {
        n = n.div_ceil(PER_BLOCK);
        sizes.push(n);
    }
    sizes
}

/// Size of the hash tree in bytes.
pub fn tree_size(data_blocks: u64) -> u64 {
    level_blocks(data_blocks).iter().sum::<u64>() * BLOCK as u64
}

pub struct Tree {
    pub root: [u8; DIGEST],
    /// The tree as stored on disk (top level first).
    pub bytes: Vec<u8>,
}

fn pad(level: &mut Vec<u8>) {
    let len = level.len().next_multiple_of(BLOCK);
    level.resize(len, 0);
}

/// Build the tree over `data_blocks` blocks; `read(first_block, buf)` fills
/// `buf` (a whole number of blocks) starting at data block `first_block`.
pub fn build(
    data_blocks: u64,
    salt: &[u8],
    mut read: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Tree> {
    if data_blocks == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no data"));
    }
    let mut buf = vec![0u8; (CHUNK_BLOCKS as usize) * BLOCK];
    let mut level = Vec::with_capacity((data_blocks as usize) * DIGEST);
    let mut first = 0;
    while first < data_blocks {
        let n = (data_blocks - first).min(CHUNK_BLOCKS);
        let chunk = &mut buf[..n as usize * BLOCK];
        read(first, chunk)?;
        for block in chunk.chunks(BLOCK) {
            level.extend_from_slice(&hash_block(salt, block));
        }
        first += n;
    }
    if data_blocks == 1 {
        let mut root = [0u8; DIGEST];
        root.copy_from_slice(&level);
        return Ok(Tree {
            root,
            bytes: Vec::new(),
        });
    }
    pad(&mut level);
    let mut levels = vec![level];
    while levels.last().unwrap().len() > BLOCK {
        let mut next = Vec::new();
        for block in levels.last().unwrap().chunks(BLOCK) {
            next.extend_from_slice(&hash_block(salt, block));
        }
        pad(&mut next);
        levels.push(next);
    }
    let root = hash_block(salt, levels.last().unwrap());
    let bytes = levels.iter().rev().flatten().copied().collect();
    Ok(Tree { root, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_layout_matches_kernel_formula() {
        // dm-verity: levels = min L with (data_blocks - 1) >> (7 * L) == 0;
        // level i has ceil(data_blocks / 128^(i+1)) blocks.
        for blocks in [1u64, 2, 127, 128, 129, 300, 16384, 16385, 38266] {
            let mut levels = 0;
            while (blocks - 1) >> (7 * levels) != 0 {
                levels += 1;
            }
            let expect: Vec<u64> = (0..levels)
                .map(|i| blocks.div_ceil(1 << (7 * (i + 1))))
                .collect();
            assert_eq!(level_blocks(blocks), expect, "{blocks} blocks");
        }
        assert_eq!(tree_size(300), 4 * 4096);
    }

    #[test]
    fn single_block_root_is_its_hash() {
        let data = vec![7u8; BLOCK];
        let t = build(1, b"salt", |_, buf| {
            buf.copy_from_slice(&data);
            Ok(())
        })
        .unwrap();
        assert_eq!(t.root, hash_block(b"salt", &data));
        assert!(t.bytes.is_empty());
    }

    #[test]
    fn two_levels() {
        let blocks = 130u64;
        let block = |i: u64| vec![i as u8; BLOCK];
        let t = build(blocks, b"s", |first, buf| {
            for (k, b) in buf.chunks_mut(BLOCK).enumerate() {
                b.copy_from_slice(&block(first + k as u64));
            }
            Ok(())
        })
        .unwrap();
        // top level (1 block) first, then level 0 (2 blocks)
        assert_eq!(t.bytes.len(), 3 * BLOCK);
        let level0 = &t.bytes[BLOCK..];
        assert_eq!(&level0[..DIGEST], &hash_block(b"s", &block(0)));
        assert_eq!(
            &level0[129 * DIGEST..130 * DIGEST],
            &hash_block(b"s", &block(129))
        );
        assert!(level0[130 * DIGEST..].iter().all(|&b| b == 0));
        let top = &t.bytes[..BLOCK];
        assert_eq!(&top[..DIGEST], &hash_block(b"s", &level0[..BLOCK]));
        assert_eq!(
            &top[DIGEST..2 * DIGEST],
            &hash_block(b"s", &level0[BLOCK..])
        );
        assert_eq!(t.root, hash_block(b"s", top));
    }
}
