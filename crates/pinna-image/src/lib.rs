//! Pinna slot images: what goes into `system_a` / `system_b`.
//!
//! ```text
//! 0                 data_size        data_size + 4096
//! [ erofs root     ][ Pinna footer  ][ dm-verity hash tree ]
//! ```
//!
//! The erofs comes from mkosi unchanged; its superblock says how big it is,
//! which is where the footer sits. The footer (see [`footer`]) is signed with
//! the release key and carries the verity root hash and salt, so pinna-init can
//! check an image and set up dm-verity from the slot alone. A sealed slot is
//! still a plain erofs at offset 0 (the trailing footer and tree are ignored).

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;

use anyhow::{Context, Result, ensure};
use ed25519_dalek::{SigningKey, VerifyingKey};

pub mod erofs;
pub mod footer;
pub mod keys;
pub mod verity;
pub mod version;

pub use footer::Footer;

/// Positional reads from a file, block device or buffer.
pub trait ReadAt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    /// Size in bytes (works for block devices too).
    fn size(&self) -> io::Result<u64>;
}

impl ReadAt for File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        FileExt::read_exact_at(self, buf, offset)
    }
    fn size(&self) -> io::Result<u64> {
        let mut f = self;
        f.seek(SeekFrom::End(0))
    }
}

impl ReadAt for [u8] {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let src = self
            .get(start..start + buf.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }
}

/// The erofs size, read from the superblock at the start of `dev`.
pub fn data_size(dev: &(impl ReadAt + ?Sized)) -> Result<u64> {
    let mut head = vec![0u8; 4096];
    dev.read_exact_at(&mut head, 0)
        .context("reading the superblock")?;
    erofs::fs_size(&head)
}

/// Build the hash tree over the first `data_size` bytes of `dev`.
pub fn hash_tree(
    dev: &(impl ReadAt + ?Sized),
    data_size: u64,
    salt: &[u8],
) -> Result<verity::Tree> {
    let block = verity::BLOCK as u64;
    ensure!(
        data_size.is_multiple_of(block),
        "data size not block aligned"
    );
    Ok(verity::build(data_size / block, salt, |first, buf| {
        dev.read_exact_at(buf, first * block)
    })?)
}

/// Seal the erofs image `input` into a slot image written to `out`.
pub fn seal(
    input: &(impl ReadAt + ?Sized),
    version: &str,
    salt: [u8; footer::SALT_LEN],
    key: &SigningKey,
    out: &mut impl Write,
) -> Result<Footer> {
    footer::check_version(version)?;
    let size = data_size(input)?;
    let input_len = input.size()?;
    ensure!(
        input_len >= size,
        "input is {input_len} bytes, its erofs superblock says {size}"
    );
    // Anything after the filesystem (partition padding from repart) must be empty.
    let mut buf = vec![0u8; 1 << 20];
    let mut off = size;
    while off < input_len {
        let n = (input_len - off).min(buf.len() as u64) as usize;
        input.read_exact_at(&mut buf[..n], off)?;
        ensure!(
            buf[..n].iter().all(|&b| b == 0),
            "non-zero data after the erofs filesystem (at {off})"
        );
        off += n as u64;
    }
    let tree = hash_tree(input, size, &salt)?;
    let footer = Footer::new(size, salt, tree.root, version)?;
    let mut off = 0;
    while off < size {
        let n = (size - off).min(buf.len() as u64) as usize;
        input.read_exact_at(&mut buf[..n], off)?;
        out.write_all(&buf[..n])?;
        off += n as u64;
    }
    out.write_all(&footer.sign(key))?;
    out.write_all(&tree.bytes)?;
    Ok(footer)
}

/// Find the footer of the slot image in `dev` (via the erofs superblock),
/// check its signature and that the whole image fits in `dev`.
pub fn read_footer(dev: &(impl ReadAt + ?Sized), key: &VerifyingKey) -> Result<Footer> {
    let size = data_size(dev)?;
    let mut buf = vec![0u8; footer::SIZE];
    dev.read_exact_at(&mut buf, size)
        .context("reading the footer")?;
    let footer = Footer::verify(&buf, key)?;
    ensure!(
        footer.data_size == size,
        "footer data size {} does not match the erofs superblock ({size})",
        footer.data_size
    );
    let dev_size = dev.size()?;
    ensure!(
        footer.image_size() <= dev_size,
        "image ({} bytes) is larger than the slot ({dev_size})",
        footer.image_size()
    );
    Ok(footer)
}

/// Cheap consistency check: the top hash block hashes to the root hash.
pub fn check_top_hash(dev: &(impl ReadAt + ?Sized), footer: &Footer) -> Result<()> {
    let mut block = vec![0u8; verity::BLOCK];
    let offset = if footer.data_blocks() > 1 {
        footer.hash_offset
    } else {
        0
    };
    dev.read_exact_at(&mut block, offset)?;
    ensure!(
        verity::hash_block(&footer.salt, &block) == footer.root_hash,
        "hash tree does not match the root hash"
    );
    Ok(())
}

/// Full check: signature, then the whole hash tree recomputed from the data
/// and compared with the root hash and the tree stored in the image.
pub fn verify(dev: &(impl ReadAt + ?Sized), key: &VerifyingKey) -> Result<Footer> {
    let footer = read_footer(dev, key)?;
    let tree = hash_tree(dev, footer.data_size, &footer.salt)?;
    ensure!(
        tree.root == footer.root_hash,
        "data does not match the signed root hash"
    );
    let mut stored = vec![0u8; tree.bytes.len()];
    dev.read_exact_at(&mut stored, footer.hash_offset)
        .context("reading the hash tree")?;
    ensure!(
        stored == tree.bytes,
        "stored hash tree differs from the data"
    );
    Ok(footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_erofs(blocks: u32, pad: usize) -> Vec<u8> {
        let mut img = erofs::fake_superblock(blocks);
        img.resize(blocks as usize * 4096, 0);
        for (i, b) in img.iter_mut().enumerate().skip(4096) {
            *b = (i * 31 % 251) as u8;
        }
        img.resize(img.len() + pad, 0);
        img
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    #[test]
    fn seal_then_verify() {
        let input = fake_erofs(300, 8192);
        let mut out = Vec::new();
        let f = seal(&input[..], "1.0", [1; 32], &key(), &mut out).unwrap();
        assert_eq!(out.len() as u64, f.image_size());
        assert_eq!(&out[..300 * 4096], &input[..300 * 4096]);
        let vk = key().verifying_key();
        assert_eq!(verify(&out[..], &vk).unwrap(), f);
        check_top_hash(&out[..], &f).unwrap();
        // a slot larger than the image is fine
        let mut slot = out.clone();
        slot.resize(out.len() + 100_000, 0xee);
        assert_eq!(read_footer(&slot[..], &vk).unwrap(), f);
    }

    #[test]
    fn detects_corruption() {
        let mut out = Vec::new();
        let f = seal(&fake_erofs(300, 0)[..], "1.0", [1; 32], &key(), &mut out).unwrap();
        let vk = key().verifying_key();
        let mut data = out.clone();
        data[200_000] ^= 1;
        assert!(verify(&data[..], &vk).is_err());
        let mut tree = out.clone();
        let last = tree.len() - 1;
        tree[last - 4096] ^= 1; // in level 0
        assert!(verify(&tree[..], &vk).is_err());
        let mut top = out.clone();
        top[f.hash_offset as usize] ^= 1;
        assert!(check_top_hash(&top[..], &f).is_err());
        assert!(read_footer(&out[..out.len() - 1], &vk).is_err());
        assert!(read_footer(&out[..], &SigningKey::from_bytes(&[4; 32]).verifying_key()).is_err());
    }

    #[test]
    fn seal_rejects_trailing_garbage() {
        let mut input = fake_erofs(10, 4096);
        *input.last_mut().unwrap() = 1;
        assert!(seal(&input[..], "1.0", [1; 32], &key(), &mut Vec::new()).is_err());
    }
}
