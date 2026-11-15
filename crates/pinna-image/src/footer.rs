//! The 4 KiB Pinna footer between the erofs data and the hash tree.
//!
//! ```text
//! off   len  field
//! 0x00    8  magic "PINNAFTR"
//! 0x08    4  format version (le32) = 1
//! 0x0c    4  reserved, zero
//! 0x10    8  data_size (le64): erofs size = offset of this footer
//! 0x18    8  hash_offset (le64): data_size + 4096
//! 0x20    8  hash_size (le64): size of the hash tree
//! 0x28    4  data block size (le32) = 4096
//! 0x2c    4  hash block size (le32) = 4096
//! 0x30   16  hash algorithm, NUL-padded ASCII = "sha256"
//! 0x40   32  salt
//! 0x60   32  root hash
//! 0x80   64  image version, NUL-padded UTF-8 (= IMAGE_VERSION in os-release)
//! 0xc0   64  ed25519 signature over bytes 0x00..0xc0
//! 0x100      zero up to 4096
//! ```

use anyhow::{Result, bail, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::verity;

pub const MAGIC: &[u8; 8] = b"PINNAFTR";
pub const FORMAT: u32 = 1;
pub const SIZE: usize = 4096;
pub const SIGNED_LEN: usize = 0xc0;
pub const BLOCK_SIZE: u32 = verity::BLOCK as u32;
pub const ALGORITHM: &str = "sha256";
pub const VERSION_MAX: usize = 63;
pub const SALT_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub data_size: u64,
    pub hash_offset: u64,
    pub hash_size: u64,
    pub data_block_size: u32,
    pub hash_block_size: u32,
    pub algorithm: String,
    pub salt: [u8; SALT_LEN],
    pub root_hash: [u8; 32],
    pub version: String,
}

fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn padded_str(b: &[u8]) -> Result<String> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    ensure!(
        b[end..].iter().all(|&c| c == 0),
        "string field not NUL-padded"
    );
    Ok(std::str::from_utf8(&b[..end])?.to_owned())
}

pub fn check_version(version: &str) -> Result<()> {
    ensure!(!version.is_empty(), "empty image version");
    ensure!(
        version.len() <= VERSION_MAX,
        "image version longer than {VERSION_MAX} bytes"
    );
    ensure!(
        version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_+~^".contains(&c)),
        "image version {version:?} has characters outside [A-Za-z0-9.-_+~^]"
    );
    Ok(())
}

impl Footer {
    /// The footer for `data_size` bytes of erofs hashed with `salt` to `root_hash`.
    pub fn new(
        data_size: u64,
        salt: [u8; SALT_LEN],
        root_hash: [u8; 32],
        version: &str,
    ) -> Result<Self> {
        check_version(version)?;
        ensure!(
            data_size > 0 && data_size.is_multiple_of(u64::from(BLOCK_SIZE)),
            "data size {data_size} is not a positive multiple of {BLOCK_SIZE}"
        );
        Ok(Self {
            data_size,
            hash_offset: data_size + SIZE as u64,
            hash_size: verity::tree_size(data_size / u64::from(BLOCK_SIZE)),
            data_block_size: BLOCK_SIZE,
            hash_block_size: BLOCK_SIZE,
            algorithm: ALGORITHM.into(),
            salt,
            root_hash,
            version: version.into(),
        })
    }

    pub fn data_blocks(&self) -> u64 {
        self.data_size / u64::from(self.data_block_size)
    }

    /// The hash tree's first block, in hash blocks from the start of the slot.
    pub fn hash_start_block(&self) -> u64 {
        self.hash_offset / u64::from(self.hash_block_size)
    }

    /// Total size of the slot image.
    pub fn image_size(&self) -> u64 {
        self.hash_offset + self.hash_size
    }

    fn signed_part(&self) -> [u8; SIGNED_LEN] {
        let mut b = [0u8; SIGNED_LEN];
        b[0..8].copy_from_slice(MAGIC);
        b[0x08..0x0c].copy_from_slice(&FORMAT.to_le_bytes());
        b[0x10..0x18].copy_from_slice(&self.data_size.to_le_bytes());
        b[0x18..0x20].copy_from_slice(&self.hash_offset.to_le_bytes());
        b[0x20..0x28].copy_from_slice(&self.hash_size.to_le_bytes());
        b[0x28..0x2c].copy_from_slice(&self.data_block_size.to_le_bytes());
        b[0x2c..0x30].copy_from_slice(&self.hash_block_size.to_le_bytes());
        b[0x30..0x30 + self.algorithm.len()].copy_from_slice(self.algorithm.as_bytes());
        b[0x40..0x60].copy_from_slice(&self.salt);
        b[0x60..0x80].copy_from_slice(&self.root_hash);
        b[0x80..0x80 + self.version.len()].copy_from_slice(self.version.as_bytes());
        b
    }

    /// Serialize and sign.
    pub fn sign(&self, key: &SigningKey) -> Vec<u8> {
        let signed = self.signed_part();
        let sig = key.sign(&signed);
        let mut out = vec![0u8; SIZE];
        out[..SIGNED_LEN].copy_from_slice(&signed);
        out[SIGNED_LEN..SIGNED_LEN + 64].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse without checking the signature.
    pub fn parse_unverified(buf: &[u8]) -> Result<(Self, Signature)> {
        ensure!(buf.len() >= SIZE, "footer truncated");
        ensure!(&buf[..8] == MAGIC, "no Pinna footer (magic)");
        let format = le32(buf, 0x08);
        ensure!(format == FORMAT, "unsupported footer format {format}");
        ensure!(le32(buf, 0x0c) == 0, "footer reserved field set");
        ensure!(
            buf[SIGNED_LEN + 64..SIZE].iter().all(|&b| b == 0),
            "footer padding not zero"
        );
        let footer = Self {
            data_size: le64(buf, 0x10),
            hash_offset: le64(buf, 0x18),
            hash_size: le64(buf, 0x20),
            data_block_size: le32(buf, 0x28),
            hash_block_size: le32(buf, 0x2c),
            algorithm: padded_str(&buf[0x30..0x40])?,
            salt: buf[0x40..0x60].try_into().unwrap(),
            root_hash: buf[0x60..0x80].try_into().unwrap(),
            version: padded_str(&buf[0x80..0xc0])?,
        };
        let sig = Signature::from_bytes(buf[SIGNED_LEN..SIGNED_LEN + 64].try_into().unwrap());
        Ok((footer, sig))
    }

    /// Parse, check the signature against `key`, then the layout.
    pub fn verify(buf: &[u8], key: &VerifyingKey) -> Result<Self> {
        let (footer, sig) = Self::parse_unverified(buf)?;
        if key.verify_strict(&buf[..SIGNED_LEN], &sig).is_err() {
            bail!("footer signature does not verify with the release key");
        }
        footer.check_layout()?;
        Ok(footer)
    }

    /// The fixed layout every slot image has (also enforced on signed footers,
    /// so a validly signed but odd footer cannot steer dm-verity elsewhere).
    pub fn check_layout(&self) -> Result<()> {
        ensure!(
            self.data_block_size == BLOCK_SIZE && self.hash_block_size == BLOCK_SIZE,
            "block sizes {}/{} (expected {BLOCK_SIZE})",
            self.data_block_size,
            self.hash_block_size
        );
        ensure!(
            self.algorithm == ALGORITHM,
            "hash algorithm {:?}",
            self.algorithm
        );
        let expect = Self::new(self.data_size, self.salt, self.root_hash, &self.version)?;
        ensure!(
            self.hash_offset == expect.hash_offset && self.hash_size == expect.hash_size,
            "hash tree at {}+{} does not follow the footer (expected {}+{})",
            self.hash_offset,
            self.hash_size,
            expect.hash_offset,
            expect.hash_size
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn footer() -> Footer {
        Footer::new(300 * 4096, [5; 32], [9; 32], "2026.11.15.1200-abcdef0").unwrap()
    }

    #[test]
    fn roundtrip_and_layout() {
        let f = footer();
        assert_eq!(f.hash_offset, 301 * 4096);
        assert_eq!(f.hash_start_block(), 301);
        assert_eq!(f.hash_size, 4 * 4096);
        let bytes = f.sign(&key(1));
        assert_eq!(bytes.len(), SIZE);
        assert_eq!(&bytes[..8], b"PINNAFTR");
        assert_eq!(&bytes[0x30..0x36], b"sha256");
        assert_eq!(Footer::verify(&bytes, &key(1).verifying_key()).unwrap(), f);
    }

    #[test]
    fn rejects_wrong_key_and_tampering() {
        let bytes = footer().sign(&key(1));
        assert!(Footer::verify(&bytes, &key(2).verifying_key()).is_err());
        for off in [0x10, 0x18, 0x41, 0x61, 0x80, 0xc0] {
            let mut b = bytes.clone();
            b[off] ^= 1;
            assert!(
                Footer::verify(&b, &key(1).verifying_key()).is_err(),
                "flip at {off:#x}"
            );
        }
        let mut b = bytes.clone();
        b[0x200] = 1;
        assert!(Footer::verify(&b, &key(1).verifying_key()).is_err());
    }

    #[test]
    fn rejects_signed_but_odd_layout() {
        let mut f = footer();
        f.hash_offset += 4096;
        let bytes = f.sign(&key(1));
        assert!(Footer::verify(&bytes, &key(1).verifying_key()).is_err());
    }

    #[test]
    fn version_rules() {
        assert!(check_version("2026.11.15.1200-abcdef0").is_ok());
        assert!(check_version("").is_err());
        assert!(check_version("a b").is_err());
        assert!(check_version(&"x".repeat(64)).is_err());
    }
}
