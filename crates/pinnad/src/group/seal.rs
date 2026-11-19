//! The group's id, key and signatures (ADR 0026), with `ring`.
//!
//! A Connect handler sees the decoded message, not the bytes that were sent,
//! so every call and answer but `Join` carries its inner message as bytes in
//! an envelope, and the tag covers those bytes, what they are for, the group
//! id and the sender's MAC. The receiver checks the tag before it decodes
//! anything.

use std::fmt;

use anyhow::{Context, Result};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

/// Bytes in a group's id. The id tells groups apart and grants nothing, but
/// two groups must not draw the same one.
pub const ID_LEN: usize = 16;
/// Bytes in a group's key: the output size of SHA-256, as RFC 2104 advises
/// for an HMAC key.
pub const KEY_LEN: usize = 32;
/// Bytes in a tag.
pub const TAG_LEN: usize = 32;

/// Starts every tag's input, so a tag made for one purpose or one version of
/// the envelope never verifies for another.
const CONTEXT: &[u8] = b"pinna-group-v1";

/// A new group's id: 16 random bytes in lowercase hex. From the system's
/// secure random generator, never fastrand.
pub fn new_id() -> Result<String> {
    let mut id = [0u8; ID_LEN];
    SystemRandom::new()
        .fill(&mut id)
        .ok()
        .context("drawing random bytes for the group's id")?;
    Ok(id.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether `id` has the form of a group's id: 32 lowercase hex digits. The id
/// goes into the mDNS record and names the key in `secrets.toml`, so nothing
/// else is taken from a file or another satellite.
pub fn is_id(id: &str) -> bool {
    id.len() == 2 * ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A group's key. Its Debug output never shows it: the key never appears in a
/// log line, a view or a response (ADR 0026).
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    pub fn new(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The key as `secrets.toml` stores it: standard base64.
    pub fn to_base64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }

    /// A key stored by [`Key::to_base64`]; `None` unless it is 32 bytes.
    pub fn from_base64(text: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(<redacted>)")
    }
}

/// A new group's key: 32 random bytes from the system's secure random
/// generator, never fastrand.
pub fn new_key() -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .ok()
        .context("drawing random bytes for the group's key")?;
    Ok(key)
}

/// The tag of an envelope: HMAC-SHA256 under the group's key over
/// `pinna-group-v1`, the purpose, the group id, the sender's MAC and the
/// body, joined by zero bytes.
///
/// The purpose names the call and its direction (`GetGroup/call`,
/// `GetGroup/answer`), so a signed answer can't be sent back as a call, nor
/// one call's body as another's.
pub fn sign(
    key: &[u8; KEY_LEN],
    purpose: &str,
    group_id: &str,
    sender_mac: &str,
    body: &[u8],
) -> [u8; TAG_LEN] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&key, &input(purpose, group_id, sender_mac, body));
    let mut out = [0u8; TAG_LEN];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Whether `tag` is the envelope's tag for `purpose`, compared in constant
/// time.
///
/// A purpose, group id or MAC with a zero byte in it never verifies: the zero
/// bytes separate the parts of the input, and one inside a part would let two
/// different envelopes share a tag.
pub fn verify(
    key: &[u8; KEY_LEN],
    purpose: &str,
    group_id: &str,
    sender_mac: &str,
    body: &[u8],
    tag: &[u8],
) -> bool {
    if [purpose, group_id, sender_mac]
        .iter()
        .any(|part| part.contains('\0'))
    {
        return false;
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&key, &input(purpose, group_id, sender_mac, body), tag).is_ok()
}

fn input(purpose: &str, group_id: &str, sender_mac: &str, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(
        CONTEXT.len() + purpose.len() + group_id.len() + sender_mac.len() + body.len() + 4,
    );
    data.extend_from_slice(CONTEXT);
    for part in [purpose, group_id, sender_mac] {
        data.push(0);
        data.extend_from_slice(part.as_bytes());
    }
    data.push(0);
    data.extend_from_slice(body);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOR: &str = "GetGroup/call";
    const ID: &str = "00112233445566778899aabbccddeeff";
    const MAC: &str = "aa:bb:cc:dd:ee:ff";
    /// A PeerHello whose sender's web port is 2000.
    const BODY: &[u8] = &[0x0a, 0x03, 0x08, 0xd0, 0x0f];

    fn key() -> [u8; KEY_LEN] {
        std::array::from_fn(|i| i as u8)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn a_signed_envelope_verifies() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        assert!(verify(&key(), FOR, ID, MAC, BODY, &tag));
        let empty = sign(&key(), FOR, ID, MAC, &[]);
        assert!(verify(&key(), FOR, ID, MAC, &[], &empty));
    }

    #[test]
    fn the_tag_is_hmac_sha256_over_the_documented_input() {
        // Computed independently with Python's hmac module:
        // hmac.new(bytes(range(32)), b"pinna-group-v1\0GetGroup/call\0" + id
        //          + b"\0" + mac + b"\0" + body, hashlib.sha256).hexdigest()
        assert_eq!(
            hex(&sign(&key(), FOR, ID, MAC, BODY)),
            "c63f272d7c4c96d6d56a5b3b021432a1f9fd316cfe9f6ead330adf4d35d2b884"
        );
    }

    #[test]
    fn ring_computes_the_standard_hmac_sha256() {
        // RFC 4231, test case 2.
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"Jefe");
        let tag = hmac::sign(&key, b"what do ya want for nothing?");
        assert_eq!(
            hex(tag.as_ref()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn any_changed_byte_of_the_group_id_fails() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        for i in 0..ID.len() {
            let mut id = ID.as_bytes().to_vec();
            id[i] = if id[i] == b'f' { b'e' } else { b'f' };
            let id = String::from_utf8(id).unwrap();
            assert!(!verify(&key(), FOR, &id, MAC, BODY, &tag), "byte {i}");
        }
        assert!(!verify(&key(), FOR, &ID[1..], MAC, BODY, &tag));
        assert!(!verify(&key(), FOR, &format!("{ID}0"), MAC, BODY, &tag));
    }

    #[test]
    fn any_changed_byte_of_the_sender_mac_fails() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        for i in 0..MAC.len() {
            let mut mac = MAC.as_bytes().to_vec();
            mac[i] = if mac[i] == b'0' { b'1' } else { b'0' };
            let mac = String::from_utf8(mac).unwrap();
            assert!(!verify(&key(), FOR, ID, &mac, BODY, &tag), "byte {i}");
        }
        // The same MAC in upper case is another input: the sender signs the
        // form it stores, the lowercase colon one.
        assert!(!verify(&key(), FOR, ID, &MAC.to_uppercase(), BODY, &tag));
    }

    #[test]
    fn any_changed_byte_of_the_body_fails() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        for i in 0..BODY.len() {
            for bit in 0..8 {
                let mut body = BODY.to_vec();
                body[i] ^= 1 << bit;
                assert!(
                    !verify(&key(), FOR, ID, MAC, &body, &tag),
                    "byte {i} bit {bit}"
                );
            }
        }
        assert!(!verify(&key(), FOR, ID, MAC, &BODY[..4], &tag));
        assert!(!verify(&key(), FOR, ID, MAC, &[BODY, &[0]].concat(), &tag));
    }

    #[test]
    fn any_changed_byte_of_the_tag_fails() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        for i in 0..TAG_LEN {
            for bit in 0..8 {
                let mut bad = tag;
                bad[i] ^= 1 << bit;
                assert!(
                    !verify(&key(), FOR, ID, MAC, BODY, &bad),
                    "byte {i} bit {bit}"
                );
            }
        }
        assert!(!verify(&key(), FOR, ID, MAC, BODY, &tag[..TAG_LEN - 1]));
        assert!(!verify(
            &key(),
            FOR,
            ID,
            MAC,
            BODY,
            &[&tag[..], &[0]].concat()
        ));
        assert!(!verify(&key(), FOR, ID, MAC, BODY, &[]));
    }

    #[test]
    fn another_key_fails() {
        let tag = sign(&key(), FOR, ID, MAC, BODY);
        let mut other = key();
        other[31] ^= 1;
        assert!(!verify(&other, FOR, ID, MAC, BODY, &tag));
    }

    #[test]
    fn a_tag_is_good_for_its_purpose_only() {
        // An answer sent back as a call, or one call's body as another's.
        let tag = sign(&key(), "GetGroup/answer", ID, MAC, BODY);
        assert!(verify(&key(), "GetGroup/answer", ID, MAC, BODY, &tag));
        for other in ["GetGroup/call", "PutGroup/call", "PutGroup/answer", ""] {
            assert!(!verify(&key(), other, ID, MAC, BODY, &tag), "{other}");
        }
    }

    #[test]
    fn the_parts_cannot_trade_bytes() {
        // Moving the boundary between the MAC and the body changes the input,
        // since a MAC may not hold the zero byte that separates them.
        let tag = sign(&key(), FOR, ID, "aa", b"b\0c");
        assert!(!verify(&key(), FOR, ID, "aa\0b", b"c", &tag));
        let tag = sign(&key(), FOR, "a\0b", MAC, BODY);
        assert!(!verify(&key(), FOR, "a\0b", MAC, BODY, &tag));
        // The same bytes split another way, from the purpose on.
        let tag = sign(&key(), FOR, ID, "aa", b"b\0c");
        assert!(!verify(
            &key(),
            &format!("{FOR}\0{ID}"),
            "aa",
            "b",
            b"c",
            &tag
        ));
    }

    #[test]
    fn a_new_id_is_16_random_bytes_in_lowercase_hex() {
        let id = new_id().unwrap();
        assert_eq!(id.len(), 2 * ID_LEN);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{id}"
        );
        assert_ne!(id, new_id().unwrap());
    }

    #[test]
    fn only_32_lowercase_hex_digits_are_an_id() {
        assert!(is_id(ID));
        assert!(is_id(&new_id().unwrap()));
        for bad in [
            "",
            &ID[1..],
            &format!("{ID}0"),
            &ID.to_uppercase(),
            "00112233445566778899aabbccddeefg",
            "00112233-4455-6677-8899-aabbccddee",
        ] {
            assert!(!is_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_key_is_stored_in_base64_and_never_shown() {
        let key = Key::new(key());
        let text = key.to_base64();
        assert_eq!(text, "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=");
        assert_eq!(Key::from_base64(&text), Some(key.clone()));
        assert_eq!(Key::from_base64(&format!(" {text}\n")), Some(key.clone()));
        // Not 32 bytes, or not base64: no key.
        assert_eq!(Key::from_base64("AAECAw=="), None);
        assert_eq!(Key::from_base64("not base64"), None);
        assert_eq!(Key::from_base64(""), None);

        let debug = format!("{key:?}");
        assert_eq!(debug, "Key(<redacted>)");
        assert!(!debug.contains(&text));
        assert!(!debug.contains(&hex(key.bytes())));
    }

    #[test]
    fn a_new_key_is_32_random_bytes() {
        let key = new_key().unwrap();
        assert_ne!(key, [0u8; KEY_LEN]);
        assert_ne!(key, new_key().unwrap());
    }
}
