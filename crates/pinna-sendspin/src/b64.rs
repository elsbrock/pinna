//! The base64 form Sendspin uses for keys and identifiers (ADR 0027): base64url without
//! padding.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Characters in the base64url form of 32 bytes, without padding.
pub(crate) const KEY_CHARS: usize = 43;

pub(crate) fn encode_url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Strict: no padding, no characters outside the alphabet, no stray trailing bits.
pub(crate) fn decode_url(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

/// 32 bytes written as exactly 43 base64url characters.
pub(crate) fn decode_url_32(text: &str) -> Option<[u8; 32]> {
    if text.len() != KEY_CHARS {
        return None;
    }
    decode_url(text)?.try_into().ok()
}

/// Whether `text` is `len` characters of the base64url alphabet.
pub(crate) fn is_url_chars(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
