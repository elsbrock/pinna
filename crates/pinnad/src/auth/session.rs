//! The session cookie (ADR 0029). pinnad keeps no list of sessions: the value
//! is an expiry and an HMAC-SHA256 tag under the session key, so a session
//! survives a restart, and a new key ends every session.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;

/// Bound into every tag, so a tag made for another purpose never validates
/// as a session.
const CONTEXT: &[u8] = b"pinna-session-v1";
/// How long a session lasts: 30 days.
pub const MAX_AGE_S: u64 = 30 * 24 * 3600;

/// The cookie's name: it ends with the last six hex digits of the MAC, because
/// cookies ignore ports and two satellites behind one proxy host would
/// otherwise overwrite each other's.
pub fn cookie_name(mac: &str) -> String {
    let hex: String = mac
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("pinna_session_{}", &hex[hex.len().saturating_sub(6)..])
}

fn input(expiry: u64) -> Vec<u8> {
    let mut input = CONTEXT.to_vec();
    input.extend_from_slice(&expiry.to_be_bytes());
    input
}

/// The cookie value of a session that ends at `expiry` (Unix seconds).
pub fn issue(key: &[u8], expiry: u64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut raw = expiry.to_be_bytes().to_vec();
    raw.extend_from_slice(hmac::sign(&key, &input(expiry)).as_ref());
    URL_SAFE_NO_PAD.encode(raw)
}

/// Whether `value` is a session this key signed that hasn't ended at `now`.
pub fn valid(key: &[u8], value: &str, now: u64) -> bool {
    if key.is_empty() {
        return false;
    }
    let Ok(raw) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    let Some((expiry, tag)) = raw.split_at_checked(8) else {
        return false;
    };
    let expiry = u64::from_be_bytes(expiry.try_into().expect("8 bytes"));
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&key, &input(expiry), tag).is_ok() && expiry > now
}

/// The value of cookie `name` in the request's `Cookie` headers.
pub fn read<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(n, v)| (n == name).then_some(v))
}

/// No `Secure`: the satellite serves HTTP. A proxy with TLS can add it.
pub fn set_cookie(name: &str, value: &str) -> String {
    format!("{name}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={MAX_AGE_S}")
}

pub fn clear_cookie(name: &str) -> String {
    format!("{name}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = &[7; 32];

    #[test]
    fn the_name_ends_with_the_last_six_hex_digits_of_the_mac() {
        assert_eq!(cookie_name("02:00:00:E3:2A:7F"), "pinna_session_e32a7f");
        assert_eq!(cookie_name("e32a7f"), "pinna_session_e32a7f");
    }

    #[test]
    fn a_session_is_valid_until_its_expiry_under_its_key() {
        let v = issue(KEY, 1_000);
        assert!(valid(KEY, &v, 999));
        assert!(!valid(KEY, &v, 1_000), "ended");
        assert!(!valid(&[8; 32], &v, 0), "another key");
        assert!(!valid(&[], &v, 0), "no key, no sessions");
    }

    #[test]
    fn a_tampered_or_malformed_value_is_refused() {
        let v = issue(KEY, 1_000);
        let mut raw = URL_SAFE_NO_PAD.decode(&v).unwrap();
        // A later expiry under the old tag.
        raw[6] ^= 0x10;
        assert!(!valid(KEY, &URL_SAFE_NO_PAD.encode(&raw), 0));
        for bad in ["", "abc", "!!!!", &v[..v.len() - 2]] {
            assert!(!valid(KEY, bad, 0), "{bad:?}");
        }
    }

    #[test]
    fn the_context_is_part_of_the_tag() {
        // The same key and expiry without the context is no session.
        let key = hmac::Key::new(hmac::HMAC_SHA256, KEY);
        let mut raw = 1_000u64.to_be_bytes().to_vec();
        raw.extend_from_slice(hmac::sign(&key, &1_000u64.to_be_bytes()).as_ref());
        assert!(!valid(KEY, &URL_SAFE_NO_PAD.encode(raw), 0));
    }

    #[test]
    fn reads_the_named_cookie_among_others() {
        let mut h = axum::http::HeaderMap::new();
        h.append(
            "cookie",
            "a=1; pinna_session_e32a7f=xyz; b=2".parse().unwrap(),
        );
        assert_eq!(read(&h, "pinna_session_e32a7f"), Some("xyz"));
        assert_eq!(read(&h, "pinna_session_000000"), None);
    }

    #[test]
    fn the_cookie_is_http_only_strict_and_not_secure() {
        let c = set_cookie("pinna_session_e32a7f", "v");
        assert_eq!(
            c,
            "pinna_session_e32a7f=v; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000"
        );
        assert!(clear_cookie("n").contains("Max-Age=0"));
    }
}
