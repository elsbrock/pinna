//! The player's side of Sendspin's Noise layer (ADR 0027 §4, §7): the responder of
//! `Noise_KKpsk2_25519_ChaChaPoly_SHA256`, the choice of the pre-shared key that Noise message
//! 1 names, and the transport that encrypts every message after the handshake.
//!
//! Only ChaChaPoly is offered: the MT8163 has no AES instructions (ADR 0027 §7).

use std::fmt;

use serde::Deserialize;

use crate::b64;
use crate::identity::Identity;
use crate::psk::{Psk, PskCategory, SENTINEL_PSK, psk_id};

/// The full Noise protocol name.
pub const PROTOCOL: &str = "Noise_KKpsk2_25519_ChaChaPoly_SHA256";
/// The suite as `client/init` announces it.
pub const SUITE: &str = "25519_ChaChaPoly_SHA256";
/// The Noise limit on one message, ciphertext and tag included.
pub const MAX_MESSAGE: usize = 65535;
const TAG: usize = 16;
/// The most plaintext one transport message can carry.
pub const MAX_PLAINTEXT: usize = MAX_MESSAGE - TAG;

/// Noise message 2's payload: the literal two bytes, not an empty payload.
const MSG2_PAYLOAD: &[u8] = b"{}";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoiseError {
    #[error("the Noise handshake failed: {0}")]
    Snow(String),
    #[error("Noise message 1 carries no valid psk_id and psk_category")]
    BadPayload,
    #[error("no key of this category has the psk_id the server named")]
    UnknownPsk,
    #[error("the pairing record with this key belongs to another server")]
    Misbinding,
    #[error("decryption failed")]
    Decrypt,
    #[error("a transport message exceeds the Noise limit")]
    TooLarge,
}

impl From<snow::Error> for NoiseError {
    fn from(e: snow::Error) -> Self {
        Self::Snow(e.to_string())
    }
}

/// What the server says about its key in Noise message 1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Msg1Payload {
    pub psk_id: String,
    pub psk_category: PskCategory,
}

/// The responder until it has written Noise message 2.
pub struct Responder {
    state: snow::HandshakeState,
}

impl Responder {
    /// `server_key` is the server's static key from `server_id`. `prologue` is the exact bytes
    /// of `client/init` then `server/init`, or, on a re-handshake, the previous handshake hash.
    pub fn new(
        identity: &Identity,
        server_key: &[u8; 32],
        prologue: &[u8],
    ) -> Result<Self, NoiseError> {
        // No PSK yet. snow checks for one only when it processes the `psk` token, and in
        // KKpsk2 that token closes message 2: message 1 (`e, es, ss`) is read without it, and
        // its payload names the key to set before message 2 is written.
        let state = snow::Builder::new(PROTOCOL.parse()?)
            .local_private_key(identity.private_key())?
            .remote_public_key(server_key)?
            .prologue(prologue)?
            .build_responder()?;
        Ok(Self { state })
    }

    /// Reads Noise message 1 and returns its payload.
    pub fn read_message1(&mut self, message: &[u8]) -> Result<Msg1Payload, NoiseError> {
        let mut payload = vec![0u8; message.len()];
        let n = self.state.read_message(message, &mut payload)?;
        payload.truncate(n);
        let parsed: Msg1Payload =
            serde_json::from_slice(&payload).map_err(|_| NoiseError::BadPayload)?;
        if !b64::is_url_chars(&parsed.psk_id, b64::KEY_CHARS) {
            return Err(NoiseError::BadPayload);
        }
        Ok(parsed)
    }

    /// Mixes in `psk`, writes Noise message 2 and switches to transport mode.
    pub fn write_message2(mut self, psk: &Psk) -> Result<(Vec<u8>, Transport), NoiseError> {
        self.state.set_psk(2, psk.bytes())?;
        let mut message = vec![0u8; 128];
        let n = self.state.write_message(MSG2_PAYLOAD, &mut message)?;
        message.truncate(n);
        let hash: [u8; 32] = self
            .state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| NoiseError::Snow("handshake hash is not 32 bytes".into()))?;
        let state = self.state.into_transport_mode()?;
        Ok((message, Transport { state, hash }))
    }
}

impl fmt::Debug for Responder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Responder")
    }
}

/// Encrypts and decrypts one session's messages.
pub struct Transport {
    state: snow::TransportState,
    hash: [u8; 32],
}

impl Transport {
    /// The handshake hash `h`: the prologue of the next re-handshake.
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.hash
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(NoiseError::TooLarge);
        }
        let mut message = vec![0u8; plaintext.len() + TAG];
        let n = self.state.write_message(plaintext, &mut message)?;
        message.truncate(n);
        Ok(message)
    }

    /// Fails on anything that is not this session's next message from the server.
    pub fn decrypt(&mut self, message: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if message.len() > MAX_MESSAGE {
            return Err(NoiseError::TooLarge);
        }
        let mut plaintext = vec![0u8; message.len()];
        let n = self
            .state
            .read_message(message, &mut plaintext)
            .map_err(|_| NoiseError::Decrypt)?;
        plaintext.truncate(n);
        Ok(plaintext)
    }
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transport")
    }
}

/// A key the player can complete a handshake with.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub category: PskCategory,
    pub psk: &'a Psk,
    /// For a long-term key, the server its pairing record names.
    pub server_id: Option<&'a str>,
}

/// Picks the key message 1 names, among `candidates` of the declared category. A long-term
/// key must belong to `server_id`. On the first handshake of a connection a miss falls back to
/// the Sentinel key, so a server whose record the player lost still gets an authenticated
/// session it can offer re-pairing on; a miss on a re-handshake fails.
pub fn select_psk(
    candidates: &[Candidate<'_>],
    msg1: &Msg1Payload,
    server_id: &str,
    initial: bool,
) -> Result<(PskCategory, Psk), NoiseError> {
    let found = candidates
        .iter()
        .filter(|c| c.category == msg1.psk_category)
        .find(|c| psk_id(c.psk.bytes()) == msg1.psk_id);
    match found {
        Some(c) if c.server_id.is_some_and(|bound| bound != server_id) => {
            Err(NoiseError::Misbinding)
        }
        Some(c) => Ok((c.category, c.psk.clone())),
        None if initial => Ok((PskCategory::Sentinel, SENTINEL_PSK)),
        None => Err(NoiseError::UnknownPsk),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Server {
        state: snow::HandshakeState,
    }

    fn server(server_private: &[u8], client: &Identity, prologue: &[u8], psk: &Psk) -> Server {
        let builder = snow::Builder::new(PROTOCOL.parse().unwrap())
            .local_private_key(server_private)
            .unwrap()
            .remote_public_key(client.public_key())
            .unwrap()
            .prologue(prologue)
            .unwrap()
            .psk(2, psk.bytes())
            .unwrap();
        Server {
            state: builder.build_initiator().unwrap(),
        }
    }

    fn msg1(server: &mut Server, payload: &str) -> Vec<u8> {
        let mut out = vec![0u8; 512];
        let n = server
            .state
            .write_message(payload.as_bytes(), &mut out)
            .unwrap();
        out.truncate(n);
        out
    }

    fn keys() -> (Identity, snow::Keypair) {
        let client = Identity::generate().unwrap();
        let server = snow::Builder::new(PROTOCOL.parse().unwrap())
            .generate_keypair()
            .unwrap();
        (client, server)
    }

    #[test]
    fn handshake_with_a_psk_chosen_after_message_1() {
        let (client, server_keys) = keys();
        let server_key: [u8; 32] = server_keys.public.clone().try_into().unwrap();
        let psk = Psk::new([9; 32]);
        let mut srv = server(&server_keys.private, &client, b"prologue", &psk);
        let payload = format!(r#"{{"psk_id":"{}","psk_category":"pr"}}"#, psk.id());
        let m1 = msg1(&mut srv, &payload);

        let mut responder = Responder::new(&client, &server_key, b"prologue").unwrap();
        let parsed = responder.read_message1(&m1).unwrap();
        assert_eq!(parsed.psk_category, PskCategory::Pairing);
        assert_eq!(parsed.psk_id, psk.id());
        let (m2, mut transport) = responder.write_message2(&psk).unwrap();

        let mut payload = vec![0u8; 64];
        let n = srv.state.read_message(&m2, &mut payload).unwrap();
        assert_eq!(&payload[..n], b"{}");
        assert_eq!(srv.state.get_handshake_hash(), transport.handshake_hash());
        let mut srv = srv.state.into_transport_mode().unwrap();

        let sealed = transport.encrypt(b"\x00hello").unwrap();
        let mut plain = vec![0u8; sealed.len()];
        let n = srv.read_message(&sealed, &mut plain).unwrap();
        assert_eq!(&plain[..n], b"\x00hello");

        let mut sealed = vec![0u8; 64];
        let n = srv.write_message(b"\x00back", &mut sealed).unwrap();
        assert_eq!(transport.decrypt(&sealed[..n]).unwrap(), b"\x00back");
        // A replayed message no longer decrypts.
        assert_eq!(transport.decrypt(&sealed[..n]), Err(NoiseError::Decrypt));
    }

    #[test]
    fn a_wrong_psk_fails_on_the_server() {
        let (client, server_keys) = keys();
        let server_key: [u8; 32] = server_keys.public.clone().try_into().unwrap();
        let mut srv = server(&server_keys.private, &client, b"p", &Psk::new([1; 32]));
        let m1 = msg1(
            &mut srv,
            r#"{"psk_id":"GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zoo","psk_category":"sn"}"#,
        );
        let mut responder = Responder::new(&client, &server_key, b"p").unwrap();
        responder.read_message1(&m1).unwrap();
        let (m2, _) = responder.write_message2(&SENTINEL_PSK).unwrap();
        let mut payload = vec![0u8; 64];
        assert!(srv.state.read_message(&m2, &mut payload).is_err());
    }

    #[test]
    fn a_different_prologue_fails_message_1() {
        let (client, server_keys) = keys();
        let server_key: [u8; 32] = server_keys.public.clone().try_into().unwrap();
        let mut srv = server(&server_keys.private, &client, b"one", &SENTINEL_PSK);
        let m1 = msg1(&mut srv, r#"{"psk_id":"x","psk_category":"sn"}"#);
        let mut responder = Responder::new(&client, &server_key, b"two").unwrap();
        assert!(matches!(
            responder.read_message1(&m1),
            Err(NoiseError::Snow(_))
        ));
    }

    #[test]
    fn malformed_message_1_payloads_are_refused() {
        let (client, server_keys) = keys();
        let server_key: [u8; 32] = server_keys.public.clone().try_into().unwrap();
        for payload in [
            "",
            "{}",
            "not json",
            r#"{"psk_id":"GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zoo","psk_category":"xx"}"#,
            r#"{"psk_id":"short","psk_category":"sn"}"#,
            r#"{"psk_id":"GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zo+","psk_category":"sn"}"#,
        ] {
            let mut srv = server(&server_keys.private, &client, b"p", &SENTINEL_PSK);
            let m1 = msg1(&mut srv, payload);
            let mut responder = Responder::new(&client, &server_key, b"p").unwrap();
            assert_eq!(
                responder.read_message1(&m1),
                Err(NoiseError::BadPayload),
                "{payload}"
            );
        }
    }

    #[test]
    fn psk_selection() {
        let pairing = Psk::new([2; 32]);
        let long_term = Psk::new([3; 32]);
        let candidates = [
            Candidate {
                category: PskCategory::LongTerm,
                psk: &long_term,
                server_id: Some("server-a"),
            },
            Candidate {
                category: PskCategory::Pairing,
                psk: &pairing,
                server_id: None,
            },
            Candidate {
                category: PskCategory::Sentinel,
                psk: &SENTINEL_PSK,
                server_id: None,
            },
        ];
        let named = |psk: &Psk, category| Msg1Payload {
            psk_id: psk.id(),
            psk_category: category,
        };

        let lt = named(&long_term, PskCategory::LongTerm);
        assert_eq!(
            select_psk(&candidates, &lt, "server-a", true),
            Ok((PskCategory::LongTerm, long_term.clone()))
        );
        // A record bound to another server is a misbinding, never a fallback.
        assert_eq!(
            select_psk(&candidates, &lt, "server-b", true),
            Err(NoiseError::Misbinding)
        );
        let pr = named(&pairing, PskCategory::Pairing);
        assert_eq!(
            select_psk(&candidates, &pr, "server-b", false),
            Ok((PskCategory::Pairing, pairing.clone()))
        );
        // The pairing key named as long-term is a miss: the category must match too.
        let wrong_category = named(&pairing, PskCategory::LongTerm);
        assert_eq!(
            select_psk(&candidates, &wrong_category, "server-a", true),
            Ok((PskCategory::Sentinel, SENTINEL_PSK))
        );
        assert_eq!(
            select_psk(&candidates, &wrong_category, "server-a", false),
            Err(NoiseError::UnknownPsk)
        );
        let sn = named(&SENTINEL_PSK, PskCategory::Sentinel);
        assert_eq!(
            select_psk(&candidates, &sn, "server-a", false),
            Ok((PskCategory::Sentinel, SENTINEL_PSK))
        );
    }

    #[test]
    fn oversized_and_short_transport_messages_fail_without_panicking() {
        let (client, server_keys) = keys();
        let server_key: [u8; 32] = server_keys.public.clone().try_into().unwrap();
        let mut srv = server(&server_keys.private, &client, b"p", &SENTINEL_PSK);
        let m1 = msg1(
            &mut srv,
            r#"{"psk_id":"GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zoo","psk_category":"sn"}"#,
        );
        let mut responder = Responder::new(&client, &server_key, b"p").unwrap();
        responder.read_message1(&m1).unwrap();
        let (_, mut transport) = responder.write_message2(&SENTINEL_PSK).unwrap();
        assert_eq!(
            transport.decrypt(&vec![0u8; MAX_MESSAGE + 1]),
            Err(NoiseError::TooLarge)
        );
        assert_eq!(transport.decrypt(&[]), Err(NoiseError::Decrypt));
        assert_eq!(transport.decrypt(&[1, 2, 3]), Err(NoiseError::Decrypt));
        assert_eq!(
            transport.encrypt(&vec![0u8; MAX_PLAINTEXT + 1]),
            Err(NoiseError::TooLarge)
        );
        assert_eq!(
            transport.encrypt(&vec![0u8; MAX_PLAINTEXT]).unwrap().len(),
            MAX_MESSAGE
        );
    }
}
