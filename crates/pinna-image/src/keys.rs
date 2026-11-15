//! Release keys: the signing key is a hex-encoded ed25519 seed in
//! `secrets/release.key` (never committed); the public key is hex in
//! `keys/release.pub`, committed and compiled into pinna-init and pinnactl.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
pub use ed25519_dalek::{SigningKey, VerifyingKey};

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("keys/release.pub: not a hex-encoded ed25519 public key"),
    }
}

const fn parse_hex_key(text: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(text[2 * i]) << 4) | nibble(text[2 * i + 1]);
        i += 1;
    }
    let mut j = 64;
    while j < text.len() {
        assert!(
            text[j].is_ascii_whitespace(),
            "keys/release.pub: trailing data"
        );
        j += 1;
    }
    out
}

/// The release public key, checked at compile time.
pub const RELEASE_PUBLIC_KEY: [u8; 32] = parse_hex_key(include_bytes!("../../../keys/release.pub"));

pub fn release_key() -> VerifyingKey {
    VerifyingKey::from_bytes(&RELEASE_PUBLIC_KEY).expect("keys/release.pub is not a valid key")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    ensure!(
        text.len().is_multiple_of(2) && text.bytes().all(|c| c.is_ascii_hexdigit()),
        "not hex"
    );
    Ok((0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect())
}

fn key32(text: &str, what: &str) -> Result<[u8; 32]> {
    unhex(text)
        .ok()
        .and_then(|b| b.try_into().ok())
        .with_context(|| format!("{what}: expected 64 hex characters"))
}

pub fn parse_public(text: &str) -> Result<VerifyingKey> {
    Ok(VerifyingKey::from_bytes(&key32(text, "public key")?)?)
}

pub fn read_public(path: &Path) -> Result<VerifyingKey> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_public(&text).with_context(|| path.display().to_string())
}

pub fn read_secret(path: &Path) -> Result<SigningKey> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(SigningKey::from_bytes(
        &key32(&text, "secret key").with_context(|| path.display().to_string())?,
    ))
}

/// Create the signing key if it does not exist (mode 0600), then (re)write
/// the public key derived from it. Returns whether a key was created.
pub fn keygen(secret: &Path, public: &Path) -> Result<bool> {
    let created = if secret.exists() {
        false
    } else {
        let mut seed = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
        if let Some(dir) = secret.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(secret)
            .with_context(|| format!("creating {}", secret.display()))?;
        writeln!(f, "{}", hex(&seed))?;
        f.sync_all()?;
        true
    };
    let key = read_secret(secret)?;
    if let Some(dir) = public.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(public, format!("{}\n", hex(key.verifying_key().as_bytes())))?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_key_is_valid() {
        let _ = release_key();
    }

    #[test]
    fn keygen_writes_matching_pair() {
        let dir = tempfile::tempdir().unwrap();
        let (s, p) = (
            dir.path().join("s/release.key"),
            dir.path().join("release.pub"),
        );
        assert!(keygen(&s, &p).unwrap());
        let mode = std::fs::metadata(&s).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o600
        );
        let pubkey = read_public(&p).unwrap();
        assert_eq!(read_secret(&s).unwrap().verifying_key(), pubkey);
        // idempotent: keeps the secret, rewrites the same public key
        assert!(!keygen(&s, &p).unwrap());
        assert_eq!(read_public(&p).unwrap(), pubkey);
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(
            unhex(&hex(&[0, 1, 0xab, 0xff])).unwrap(),
            [0, 1, 0xab, 0xff]
        );
        assert!(unhex("abc").is_err());
        assert!(unhex("zz").is_err());
    }
}
