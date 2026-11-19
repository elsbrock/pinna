//! The password hash and the two tokens (ADR 0029), over the strings
//! `secrets.toml` holds ([`DashboardFile`]). Comparisons are constant-time.

use std::num::NonZeroU32;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use pinna_assist::secrets::DashboardFile;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{hmac, pbkdf2};
use sha2::{Digest, Sha256};

/// PBKDF2-HMAC-SHA256 iterations for a new password. The count is stored with
/// the hash, so it can change without locking anyone out.
///
/// ADR 0029 wants one check to take about a quarter of a second on the Echo
/// Dot (2nd gen). That is not measured there yet: this count is the default
/// until it is. On x86 (Intel Core Ultra X7 358H, release build) one check at
/// this count takes 27 ms (`measure_one_check` below; 281 ms in a debug
/// build). The device is slower than that, so set the count from a
/// measurement there.
pub const ITERATIONS: u32 = 100_000;

const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
const PASSWORD: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256;
/// A password is 8 to 128 characters (ADR 0029).
pub const MIN_CHARS: usize = 8;
pub const MAX_CHARS: usize = 128;

/// Which of the two tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Reads `/metrics`.
    Metrics,
    /// Opens `/mcp`.
    Agent,
}

/// Whether `a` and `b` are the same bytes, in time that doesn't depend on where
/// they differ. ring 0.17's own comparison is deprecated; its HMAC check is
/// constant-time, and two inputs have the same tag only when they are equal.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, b"pinna-compare-v1");
    hmac::verify(&key, a, hmac::sign(&key, b).as_ref()).is_ok()
}

fn fill<const N: usize>() -> Result<[u8; N], ring::error::Unspecified> {
    let mut bytes = [0u8; N];
    SystemRandom::new().fill(&mut bytes)?;
    Ok(bytes)
}

/// A new key for signing sessions, in the file's form.
pub fn new_session_key() -> Result<String, ring::error::Unspecified> {
    Ok(STANDARD.encode(fill::<KEY_LEN>()?))
}

/// Set `password` in `file`: a new salt, `iterations`, and the hash.
pub fn set_password(
    file: &mut DashboardFile,
    password: &str,
    iterations: u32,
) -> Result<(), ring::error::Unspecified> {
    let salt = fill::<SALT_LEN>()?;
    let iterations_nz = NonZeroU32::new(iterations).ok_or(ring::error::Unspecified)?;
    let mut hash = [0u8; KEY_LEN];
    pbkdf2::derive(
        PASSWORD,
        iterations_nz,
        &salt,
        password.as_bytes(),
        &mut hash,
    );
    file.salt = STANDARD.encode(salt);
    file.hash = STANDARD.encode(hash);
    file.iterations = iterations;
    Ok(())
}

pub fn has_password(file: &DashboardFile) -> bool {
    !file.hash.is_empty()
}

/// Whether `password` is the one in `file`. Slow on purpose: run it on a
/// blocking thread.
pub fn check_password(file: &DashboardFile, password: &str) -> bool {
    let (Ok(salt), Ok(hash), Some(iterations)) = (
        STANDARD.decode(&file.salt),
        STANDARD.decode(&file.hash),
        NonZeroU32::new(file.iterations),
    ) else {
        return false;
    };
    pbkdf2::verify(PASSWORD, iterations, &salt, password.as_bytes(), &hash).is_ok()
}

/// The session key's bytes; empty when there is none.
pub fn session_key(file: &DashboardFile) -> Vec<u8> {
    STANDARD.decode(&file.session_key).unwrap_or_default()
}

fn digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// A new token of `kind` in `file`: only its digest and last four characters
/// are kept. Returns the token, which is shown once.
pub fn new_token(
    file: &mut DashboardFile,
    kind: TokenKind,
) -> Result<String, ring::error::Unspecified> {
    let token = URL_SAFE_NO_PAD.encode(fill::<KEY_LEN>()?);
    let last4 = token[token.len() - 4..].to_owned();
    match kind {
        TokenKind::Metrics => (file.metrics_sha256, file.metrics_last4) = (digest(&token), last4),
        TokenKind::Agent => (file.agent_sha256, file.agent_last4) = (digest(&token), last4),
    }
    Ok(token)
}

/// Whether `presented` is the `kind` token. No token created, none matches.
pub fn token_matches(file: &DashboardFile, kind: TokenKind, presented: &str) -> bool {
    let stored = match kind {
        TokenKind::Metrics => &file.metrics_sha256,
        TokenKind::Agent => &file.agent_sha256,
    };
    !stored.is_empty() && same(digest(presented).as_bytes(), stored.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cost of one check, for [`ITERATIONS`]:
    /// `cargo test --release -p pinnad measure_one_check -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_one_check() {
        let mut file = DashboardFile::default();
        set_password(&mut file, "correct horse", ITERATIONS).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            assert!(check_password(&file, "correct horse"));
        }
        println!(
            "{ITERATIONS} iterations: {:?} per check",
            start.elapsed() / 10
        );
    }

    #[test]
    fn same_compares_bytes() {
        assert!(same(b"abc", b"abc"));
        assert!(same(b"", b""));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
        assert!(!same(b"", b"a"));
    }

    #[test]
    fn a_password_checks_against_its_own_hash_only() {
        let mut file = DashboardFile::default();
        assert!(!has_password(&file));
        set_password(&mut file, "correct horse", 1_000).unwrap();
        assert!(has_password(&file));
        assert_eq!(file.iterations, 1_000, "the count is stored with the hash");
        assert!(check_password(&file, "correct horse"));
        assert!(!check_password(&file, "correct horsf"));
        assert!(!check_password(&file, ""));
        assert!(!check_password(&DashboardFile::default(), ""));
        // The hash and salt aren't the password.
        assert!(!file.hash.contains("correct") && !file.salt.contains("correct"));
        // A new salt each time.
        let salt = file.salt.clone();
        set_password(&mut file, "correct horse", 1_000).unwrap();
        assert_ne!(file.salt, salt);
        assert_eq!(STANDARD.decode(&file.salt).unwrap().len(), SALT_LEN);
    }

    #[test]
    fn a_token_is_shown_once_and_kept_as_a_digest() {
        let mut file = DashboardFile::default();
        let token = new_token(&mut file, TokenKind::Agent).unwrap();
        assert_eq!(token.len(), 43, "32 bytes, base64url");
        assert_eq!(file.agent_last4, token[39..]);
        assert!(!format!("{file:?}{}", file.agent_sha256).contains(&token));
        assert!(token_matches(&file, TokenKind::Agent, &token));
        // Each token opens its own door.
        assert!(!token_matches(&file, TokenKind::Metrics, &token));
        assert!(!token_matches(&file, TokenKind::Agent, "nope"));

        // A new token revokes the old one.
        let newer = new_token(&mut file, TokenKind::Agent).unwrap();
        assert!(!token_matches(&file, TokenKind::Agent, &token));
        assert!(token_matches(&file, TokenKind::Agent, &newer));
        // No token, nothing matches, not even the empty one.
        assert!(!token_matches(&file, TokenKind::Metrics, ""));
    }
}
