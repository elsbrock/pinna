//! The player's persistent X25519 identity (ADR 0027 §7). Its public key, in base64url, is the
//! `client_id` servers know the player by, and the static key of the Noise handshake. Changing
//! it makes the player a new device to every server.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::b64;
use crate::noise::PROTOCOL;

/// An X25519 key pair. Its Debug output never shows the private key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Stored", into = "Stored")]
pub struct Identity {
    private: [u8; 32],
    public: [u8; 32],
}

impl Identity {
    /// A new key pair from the system's secure random generator, through snow.
    pub fn generate() -> Result<Self, IdentityError> {
        let params = PROTOCOL.parse().map_err(|_| IdentityError::Generate)?;
        let pair = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|_| IdentityError::Generate)?;
        Ok(Self {
            private: pair
                .private
                .try_into()
                .map_err(|_| IdentityError::Generate)?,
            public: pair
                .public
                .try_into()
                .map_err(|_| IdentityError::Generate)?,
        })
    }

    /// A key pair kept from an earlier [`Identity::generate`]. The two halves are not checked
    /// against each other: a mismatched pair fails every handshake.
    pub fn from_keys(private: [u8; 32], public: [u8; 32]) -> Self {
        Self { private, public }
    }

    pub fn public_key(&self) -> &[u8; 32] {
        &self.public
    }

    pub(crate) fn private_key(&self) -> &[u8; 32] {
        &self.private
    }

    /// The `client_id`: the public key in base64url without padding, 43 characters.
    pub fn client_id(&self) -> String {
        b64::encode_url(&self.public)
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("client_id", &self.client_id())
            .field("private", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("could not generate an X25519 key pair")]
    Generate,
    #[error("an identity key must be 32 bytes in base64url")]
    Stored,
}

/// How a store file keeps the pair: both halves in base64url.
#[derive(Serialize, Deserialize)]
struct Stored {
    private: String,
    public: String,
}

impl From<Identity> for Stored {
    fn from(identity: Identity) -> Self {
        Self {
            private: b64::encode_url(&identity.private),
            public: b64::encode_url(&identity.public),
        }
    }
}

impl TryFrom<Stored> for Identity {
    type Error = IdentityError;

    fn try_from(stored: Stored) -> Result<Self, Self::Error> {
        Ok(Self {
            private: b64::decode_url_32(&stored.private).ok_or(IdentityError::Stored)?,
            public: b64::decode_url_32(&stored.public).ok_or(IdentityError::Stored)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_is_43_base64url_characters() {
        let identity = Identity::generate().unwrap();
        let id = identity.client_id();
        assert!(b64::is_url_chars(&id, 43), "{id}");
        assert_eq!(b64::decode_url_32(&id).unwrap(), *identity.public_key());
    }

    #[test]
    fn two_identities_differ() {
        assert_ne!(
            Identity::generate().unwrap().client_id(),
            Identity::generate().unwrap().client_id()
        );
    }

    #[test]
    fn debug_hides_the_private_key() {
        let identity = Identity::from_keys([0xab; 32], [1; 32]);
        let debug = format!("{identity:?}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains(&b64::encode_url(&[0xab; 32])), "{debug}");
    }

    #[test]
    fn serde_round_trips() {
        let identity = Identity::generate().unwrap();
        let text = toml::to_string(&identity).unwrap();
        assert_eq!(toml::from_str::<Identity>(&text).unwrap(), identity);
        assert!(toml::from_str::<Identity>("private = \"AA\"\npublic = \"AA\"\n").is_err());
    }
}
