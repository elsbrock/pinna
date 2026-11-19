//! The pre-shared keys a Sendspin session is keyed with (ADR 0027 §4, §5): their identifiers,
//! the three categories a server can use one as, and the published Sentinel key.

use std::fmt;

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::b64;

/// A 32-byte pre-shared key. Its Debug output never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Psk([u8; 32]);

impl Psk {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A new key from the system's secure random generator.
    pub fn random() -> Result<Self, RandomError> {
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| RandomError)?;
        Ok(Self(bytes))
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The identifier a server names this key by in Noise message 1.
    pub fn id(&self) -> String {
        psk_id(&self.0)
    }

    /// The key as the wire and the store write it: 43 base64url characters.
    pub fn to_base64url(&self) -> String {
        b64::encode_url(&self.0)
    }

    /// A key written by [`Psk::to_base64url`]; `None` unless it is exactly 32 bytes.
    pub fn from_base64url(text: &str) -> Option<Self> {
        b64::decode_url_32(text).map(Self)
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

impl Serialize for Psk {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for Psk {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_base64url(&text)
            .ok_or_else(|| serde::de::Error::custom("a key must be 32 bytes in base64url"))
    }
}

/// The system's random number generator failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the system's random number generator failed")]
pub struct RandomError;

/// `base64url(SHA-256("sendspin-psk-id-v1" || psk))`, the same for every category.
pub fn psk_id(psk: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"sendspin-psk-id-v1");
    hash.update(psk);
    b64::encode_url(&hash.finalize())
}

/// `SHA-256("sendspin-sentinel-psk-v1")`, the published key a server uses when no other
/// applies. It authenticates nothing.
pub const SENTINEL_PSK: Psk = Psk([
    0x1b, 0x5e, 0x24, 0xdb, 0xc1, 0xae, 0xd9, 0x5f, 0xc2, 0xa5, 0xa3, 0x38, 0xa9, 0x0c, 0x05, 0xdf,
    0x44, 0xbd, 0x10, 0xf5, 0xec, 0x1f, 0x4c, 0xd6, 0x6c, 0xbf, 0x86, 0x27, 0x27, 0x67, 0xb9, 0xd3,
]);

/// The Sentinel key's identifier, as the specification publishes it.
pub const SENTINEL_PSK_ID: &str = "GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zoo";

/// What the server uses a key as, named in Noise message 1 as `psk_category`. It decides which
/// activities the session allows: only a long-term key makes the session paired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PskCategory {
    /// A key from a pairing record.
    #[serde(rename = "lt")]
    LongTerm,
    /// The player's own pairing key, which the owner hands to a server as a token.
    #[serde(rename = "pr")]
    Pairing,
    /// The published Sentinel key.
    #[serde(rename = "sn")]
    Sentinel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_is_the_hash_of_its_label() {
        let hash: [u8; 32] = Sha256::digest(b"sendspin-sentinel-psk-v1").into();
        assert_eq!(&hash, SENTINEL_PSK.bytes());
    }

    #[test]
    fn sentinel_id_matches_the_published_constant() {
        assert_eq!(SENTINEL_PSK.id(), SENTINEL_PSK_ID);
        // The specification also gives it in hex.
        let raw = b64::decode_url(SENTINEL_PSK_ID).unwrap();
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "185b15f6d2da4909bd1dc156a4ab206103abef0153bcd52d926170b95cf7ce8a"
        );
    }

    #[test]
    fn debug_hides_the_key() {
        let psk = Psk::new([7; 32]);
        assert_eq!(format!("{psk:?}"), "Psk(<redacted>)");
    }

    #[test]
    fn serde_round_trips_and_refuses_wrong_lengths() {
        let psk = Psk::random().unwrap();
        let json = serde_json::to_string(&psk).unwrap();
        assert_eq!(json.len(), 45);
        assert_eq!(serde_json::from_str::<Psk>(&json).unwrap(), psk);
        assert!(serde_json::from_str::<Psk>("\"AAAA\"").is_err());
        // 43 characters whose last one carries stray bits do not decode.
        assert!(Psk::from_base64url(&format!("{}B", &json[1..43])).is_none());
    }

    #[test]
    fn categories_use_the_wire_codes() {
        for (category, code) in [
            (PskCategory::LongTerm, "\"lt\""),
            (PskCategory::Pairing, "\"pr\""),
            (PskCategory::Sentinel, "\"sn\""),
        ] {
            assert_eq!(serde_json::to_string(&category).unwrap(), code);
            assert_eq!(serde_json::from_str::<PskCategory>(code).unwrap(), category);
        }
        assert!(serde_json::from_str::<PskCategory>("\"xx\"").is_err());
    }
}
