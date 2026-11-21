//! The update manifest's signature and the limits both its writer
//! (`pinna-image manifest`) and its readers (the updater) keep (ADR 0022).
//!
//! `manifest.json.sig`, next to `manifest.json`, is an ed25519 signature
//! over the manifest's exact bytes, made with the release key: the raw 64
//! bytes, nothing else. A signature over the bytes needs no canonical JSON.

use anyhow::{Result, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// The lowest boot level a root built from this tree needs (ADR 0022); the
/// manifest copies it into `requires.boot_level`. Raise it together with
/// `pinna_bootimg::BOOT_LEVEL`, and only when there is no other way.
pub const ROOT_BOOT_LEVEL: u32 = 1;

/// Home Assistant's update card shows at most this much of the summary.
pub const SUMMARY_MAX: usize = 255;

/// A manifest is small; anything bigger is refused before it is read.
pub const MANIFEST_MAX: u64 = 1 << 20;

/// The models a release can name in `requires.models`: every model's id
/// ([`pinna_board::Model::id`]).
pub fn models() -> Vec<&'static str> {
    pinna_board::Model::ALL.map(pinna_board::Model::id).to_vec()
}

pub const SIGNATURE_LEN: usize = 64;

pub fn sign(manifest: &[u8], key: &SigningKey) -> [u8; SIGNATURE_LEN] {
    key.sign(manifest).to_bytes()
}

/// Check `sig` (the `.sig` file's bytes) over `manifest` with `key`.
pub fn verify(manifest: &[u8], sig: &[u8], key: &VerifyingKey) -> Result<()> {
    ensure!(
        sig.len() == SIGNATURE_LEN,
        "the signature is {} bytes, not {SIGNATURE_LEN}",
        sig.len()
    );
    let sig = Signature::from_slice(sig)?;
    key.verify(manifest, &sig)
        .map_err(|_| anyhow::anyhow!("the signature does not match the release key"))
}

/// One line of at most [`SUMMARY_MAX`] characters, without control
/// characters.
pub fn check_summary(summary: &str) -> Result<()> {
    ensure!(!summary.trim().is_empty(), "the summary is empty");
    ensure!(
        summary.chars().count() <= SUMMARY_MAX,
        "the summary has {} characters; Home Assistant shows at most {SUMMARY_MAX}",
        summary.chars().count()
    );
    ensure!(
        !summary.chars().any(char::is_control),
        "the summary has a line break or another control character"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_over_the_exact_bytes() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let m = br#"{"version":"0.4.0"}"#;
        let sig = sign(m, &key);
        verify(m, &sig, &key.verifying_key()).unwrap();
        assert!(verify(m, &sig, &other.verifying_key()).is_err());
        assert!(verify(br#"{"version":"0.4.1"}"#, &sig, &key.verifying_key()).is_err());
        // A space more is another manifest.
        assert!(verify(br#"{"version": "0.4.0"}"#, &sig, &key.verifying_key()).is_err());
        assert!(verify(m, &sig[..63], &key.verifying_key()).is_err());
        let hex = crate::keys::hex(&sig);
        assert!(verify(m, hex.as_bytes(), &key.verifying_key()).is_err());
    }

    #[test]
    fn summaries() {
        check_summary("Pinna 0.4.0.").unwrap();
        check_summary(&"é".repeat(255)).unwrap();
        assert!(check_summary(&"a".repeat(256)).is_err());
        assert!(check_summary("").is_err());
        assert!(check_summary("two\nlines").is_err());
    }
}
