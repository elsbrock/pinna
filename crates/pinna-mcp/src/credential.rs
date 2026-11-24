//! The credential pinnad sends with every request to one of the owner's MCP
//! servers (ADR 0017 §"The owner's MCP servers are remote only"), and the
//! rules a typed one must meet before it is stored.

use std::fmt;

use base64::{Engine, engine::general_purpose::STANDARD};
use pinna_http::{
    Secret,
    reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue},
};
use thiserror::Error;

/// The longest token, user name, password or header value, in bytes: a
/// provider key's limit.
pub const CREDENTIAL_MAX: usize = 1024;
/// The longest header name.
pub const HEADER_NAME_MAX: usize = 64;
/// A shorter token or value doesn't show its last four characters: they
/// would give much of it away.
const LAST4_MIN: usize = 16;

/// Headers the client sets itself, or that frame the request. A credential
/// in one of them would break the exchange or be dropped by HTTP.
const RESERVED: [&str; 8] = [
    "host",
    "accept",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "transfer-encoding",
];
const RESERVED_PREFIXES: [&str; 3] = ["content-", "mcp-", "proxy-"];

/// Why a typed credential can't be stored, as a sentence for the owner.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct CredentialError(&'static str);

#[derive(Clone, PartialEq, Eq)]
pub enum McpCredential {
    /// Sent as `Authorization: Bearer <token>`.
    Bearer { token: Secret },
    /// Sent as `Authorization: Basic`, RFC 7617 in UTF-8.
    Basic { user: String, password: Secret },
    /// A header the owner names, such as `X-API-Key`.
    Header { name: String, value: Secret },
}

impl McpCredential {
    pub fn bearer(token: &str) -> Result<Self, CredentialError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(CredentialError("Enter the token."));
        }
        if token.len() > CREDENTIAL_MAX {
            return Err(CredentialError("That token is too long."));
        }
        if !token.chars().all(|c| c.is_ascii_graphic()) {
            return Err(CredentialError(
                "A token is letters, digits and symbols, with no spaces.",
            ));
        }
        Ok(Self::Bearer {
            token: Secret::new(token),
        })
    }

    /// The password is taken as typed: spaces at its ends are part of it.
    pub fn basic(user: &str, password: &str) -> Result<Self, CredentialError> {
        let user = user.trim();
        if user.is_empty() {
            return Err(CredentialError("Enter the user name."));
        }
        if user.len() > CREDENTIAL_MAX {
            return Err(CredentialError("That user name is too long."));
        }
        // RFC 7617: the first colon ends the user name.
        if user.contains(':') {
            return Err(CredentialError("A user name can't contain a colon."));
        }
        if user.chars().any(char::is_control) {
            return Err(CredentialError(
                "A user name can't contain control characters.",
            ));
        }
        if password.is_empty() {
            return Err(CredentialError("Enter the password."));
        }
        if password.len() > CREDENTIAL_MAX {
            return Err(CredentialError("That password is too long."));
        }
        if password.chars().any(char::is_control) {
            return Err(CredentialError(
                "A password can't contain control characters.",
            ));
        }
        Ok(Self::Basic {
            user: user.to_owned(),
            password: Secret::new(password),
        })
    }

    pub fn header(name: &str, value: &str) -> Result<Self, CredentialError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CredentialError("Enter the header's name."));
        }
        if name.len() > HEADER_NAME_MAX {
            return Err(CredentialError("A header's name is at most 64 characters."));
        }
        // RFC 9110 §5.6.2: a token.
        if !name.bytes().all(is_tchar) {
            return Err(CredentialError(
                "A header's name is letters, digits and hyphens, such as X-API-Key.",
            ));
        }
        let lower = name.to_ascii_lowercase();
        if RESERVED.contains(&lower.as_str())
            || RESERVED_PREFIXES.iter().any(|p| lower.starts_with(p))
        {
            return Err(CredentialError(
                "Pinna sets that header itself. Use the server's own header, such as X-API-Key.",
            ));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(CredentialError("Enter the header's value."));
        }
        if value.len() > CREDENTIAL_MAX {
            return Err(CredentialError("That value is too long."));
        }
        if !value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(CredentialError(
                "A header's value is letters, digits, symbols and spaces, without accents.",
            ));
        }
        Ok(Self::Header {
            name: name.to_owned(),
            value: Secret::new(value),
        })
    }

    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::Bearer { .. } => CredentialKind::Bearer,
            Self::Basic { .. } => CredentialKind::Basic,
            Self::Header { .. } => CredentialKind::Header,
        }
    }

    /// What tells it apart in the dashboard: the user name, or a header's
    /// name. Never a password.
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Bearer { .. } => None,
            Self::Basic { user, .. } => Some(user),
            Self::Header { name, .. } => Some(name),
        }
    }

    /// The last four characters of a token or a header's value of at least
    /// 16. A password's are never shown: a password is short enough for them
    /// to help a guess, and so is a short token.
    pub fn last4(&self) -> Option<String> {
        let shown = |s: &Secret| (s.expose().chars().count() >= LAST4_MIN).then(|| s.last4());
        match self {
            Self::Bearer { token } => shown(token),
            Self::Basic { .. } => None,
            Self::Header { value, .. } => shown(value),
        }
    }

    /// The header to send, marked sensitive so HTTP logging leaves it out.
    /// `None` only for a value the constructors refuse.
    pub(crate) fn to_header(&self) -> Option<(HeaderName, HeaderValue)> {
        let (name, value) = match self {
            Self::Bearer { token } => (AUTHORIZATION, format!("Bearer {}", token.expose())),
            Self::Basic { user, password } => {
                let pair = format!("{user}:{}", password.expose());
                (AUTHORIZATION, format!("Basic {}", STANDARD.encode(pair)))
            }
            Self::Header { name, value } => (
                HeaderName::from_bytes(name.as_bytes()).ok()?,
                value.expose().to_owned(),
            ),
        };
        let mut value = HeaderValue::from_str(&value).ok()?;
        value.set_sensitive(true);
        Some((name, value))
    }
}

impl fmt::Debug for McpCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpCredential")
            .field("kind", &self.kind())
            .field("label", &self.label())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    Bearer,
    Basic,
    Header,
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(c: &McpCredential) -> (String, String) {
        let (name, value) = c.to_header().unwrap();
        assert!(value.is_sensitive());
        (name.to_string(), value.to_str().unwrap().to_owned())
    }

    #[test]
    fn bearer_is_sent_as_authorization() {
        let c = McpCredential::bearer("  abc.def-123  ").unwrap();
        assert_eq!(
            sent(&c),
            ("authorization".into(), "Bearer abc.def-123".into())
        );
        assert_eq!(c.label(), None);
        assert_eq!(c.last4(), None, "a short token's end would give it away");
        let c = McpCredential::bearer("0123456789abcdef").unwrap();
        assert_eq!(c.last4().as_deref(), Some("cdef"));
    }

    #[test]
    fn basic_is_rfc_7617_in_utf8() {
        // RFC 7617 §2.1's example, and a non-ASCII password.
        let c = McpCredential::basic("Aladdin", "open sesame").unwrap();
        assert_eq!(sent(&c).1, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        let c = McpCredential::basic("test", "123£").unwrap();
        assert_eq!(sent(&c).1, "Basic dGVzdDoxMjPCow==");
        assert_eq!(c.label(), Some("test"));
        assert_eq!(c.last4(), None, "a password's end is never shown");
    }

    #[test]
    fn a_password_keeps_its_spaces() {
        let c = McpCredential::basic(" me ", " pw ").unwrap();
        assert_eq!(sent(&c).1, format!("Basic {}", STANDARD.encode("me: pw ")));
    }

    #[test]
    fn header_is_sent_by_its_name() {
        let c = McpCredential::header("X-API-Key", "secret value 9876").unwrap();
        assert_eq!(sent(&c), ("x-api-key".into(), "secret value 9876".into()));
        assert_eq!(c.label(), Some("X-API-Key"));
        assert_eq!(c.last4().as_deref(), Some("9876"));
    }

    #[test]
    fn authorization_may_be_named_with_another_scheme() {
        let c = McpCredential::header("Authorization", "Token abc").unwrap();
        assert_eq!(sent(&c), ("authorization".into(), "Token abc".into()));
    }

    #[test]
    fn headers_the_client_sets_are_refused() {
        for name in [
            "Host",
            "accept",
            "Content-Type",
            "Content-Length",
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
            "Transfer-Encoding",
            "Connection",
            "Proxy-Authorization",
            "TE",
        ] {
            assert!(McpCredential::header(name, "x").is_err(), "{name}");
        }
    }

    #[test]
    fn malformed_input_is_refused() {
        assert!(McpCredential::bearer("").is_err());
        assert!(McpCredential::bearer("two words").is_err());
        assert!(McpCredential::bearer(&"a".repeat(CREDENTIAL_MAX + 1)).is_err());
        assert!(McpCredential::bearer(&"a".repeat(CREDENTIAL_MAX)).is_ok());
        assert!(McpCredential::basic("", "pw").is_err());
        assert!(McpCredential::basic("a:b", "pw").is_err());
        assert!(McpCredential::basic("me", "").is_err());
        assert!(McpCredential::basic("me", "line\nbreak").is_err());
        assert!(McpCredential::header("", "v").is_err());
        assert!(McpCredential::header("X Key", "v").is_err());
        assert!(McpCredential::header("X-Key:", "v").is_err());
        assert!(McpCredential::header(&"X".repeat(HEADER_NAME_MAX + 1), "v").is_err());
        assert!(McpCredential::header("X-Key", "").is_err());
        assert!(McpCredential::header("X-Key", "caf\u{e9}").is_err());
        assert!(McpCredential::header("X-Key", "a\tb").is_err());
    }

    #[test]
    fn debug_shows_no_secret() {
        let c = McpCredential::basic("me", "hunter2").unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        let c = McpCredential::header("X-Key", "abcdefgh").unwrap();
        assert!(!format!("{c:?}").contains("abcd"));
    }
}
