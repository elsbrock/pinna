//! The `secrets.toml` codec and URL-bound provider credentials (ADR 0017
//! §"Secrets").
//!
//! The file representation contains strings for TOML only. The in-memory
//! representation wraps keys and MCP URLs in [`Secret`], and its debug output
//! contains no credential material. MCP servers have no token yet; a `bearer`
//! from another build stays in `extra`.

use std::{collections::BTreeMap, fmt};

use pinna_http::Secret;
use serde::{Deserialize, Serialize};

/// The plain-string representation written to `secrets.toml`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretsFile {
    pub connections: BTreeMap<String, ConnectionFile>,
    pub mcp: BTreeMap<String, McpFile>,
    /// Keys written by a newer image survive a rollback and roll-forward.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionFile {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpFile {
    pub url: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl fmt::Debug for SecretsFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsFile")
            .field("connections", &self.connections)
            .field("mcp", &self.mcp)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ConnectionFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionFile")
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for McpFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpFile")
            .field("url", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
struct Connection {
    url: String,
    key: Option<Secret>,
    extra: toml::Table,
}

#[derive(Clone)]
struct Mcp {
    url: Secret,
    extra: toml::Table,
}

impl Default for Mcp {
    fn default() -> Self {
        Self {
            url: Secret::new(""),
            extra: toml::Table::new(),
        }
    }
}

/// Provider credentials in memory. A connection key is returned only for the
/// exact URL it was entered with.
#[derive(Clone, Default)]
pub struct Secrets {
    connections: BTreeMap<String, Connection>,
    mcp: BTreeMap<String, Mcp>,
    extra: toml::Table,
}

impl Secrets {
    pub fn key_for(&self, connection: &str, current_url: &str) -> Option<&Secret> {
        let connection = self.connections.get(connection)?;
        if connection.url != current_url {
            return None;
        }
        connection.key.as_ref()
    }

    /// Store a key together with the URL it was entered for.
    pub fn set_key(&mut self, connection: impl Into<String>, url: impl Into<String>, key: Secret) {
        let connection = self.connections.entry(connection.into()).or_default();
        connection.url = url.into();
        connection.key = Some(key);
    }

    /// Remove a connection's URL and key, for example after its URL changes.
    pub fn clear(&mut self, connection: &str) {
        self.connections.remove(connection);
    }

    pub fn last4(&self, connection: &str) -> Option<String> {
        Some(self.connections.get(connection)?.key.as_ref()?.last4())
    }

    pub fn set_mcp(&mut self, id: impl Into<String>, url: Secret) {
        self.mcp.entry(id.into()).or_default().url = url;
    }

    /// An MCP server's address. It remains a secret and is never part of a
    /// dashboard view.
    pub fn mcp(&self, id: &str) -> Option<&Secret> {
        Some(&self.mcp.get(id)?.url)
    }

    pub fn clear_mcp(&mut self, id: &str) {
        self.mcp.remove(id);
    }

    /// The only MCP location metadata exposed to the dashboard.
    pub fn mcp_host(&self, id: &str) -> Option<String> {
        let url = url::Url::parse(self.mcp.get(id)?.url.expose()).ok()?;
        let host = url.host_str()?;
        Some(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        })
    }

    pub fn to_file(&self) -> SecretsFile {
        SecretsFile::from(self)
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("connections", &self.connections.len())
            .field("mcp", &self.mcp.len())
            .finish_non_exhaustive()
    }
}

impl From<SecretsFile> for Secrets {
    fn from(file: SecretsFile) -> Self {
        Self {
            connections: file
                .connections
                .into_iter()
                .map(|(id, connection)| {
                    (
                        id,
                        Connection {
                            url: connection.url,
                            key: connection.key.map(Secret::new),
                            extra: connection.extra,
                        },
                    )
                })
                .collect(),
            mcp: file
                .mcp
                .into_iter()
                .map(|(id, server)| {
                    (
                        id,
                        Mcp {
                            url: Secret::new(server.url),
                            extra: server.extra,
                        },
                    )
                })
                .collect(),
            extra: file.extra,
        }
    }
}

impl From<&Secrets> for SecretsFile {
    fn from(secrets: &Secrets) -> Self {
        Self {
            connections: secrets
                .connections
                .iter()
                .map(|(id, connection)| {
                    (
                        id.clone(),
                        ConnectionFile {
                            url: connection.url.clone(),
                            key: connection.key.as_ref().map(|key| key.expose().to_owned()),
                            extra: connection.extra.clone(),
                        },
                    )
                })
                .collect(),
            mcp: secrets
                .mcp
                .iter()
                .map(|(id, server)| {
                    (
                        id.clone(),
                        McpFile {
                            url: server.url.expose().to_owned(),
                            extra: server.extra.clone(),
                        },
                    )
                })
                .collect(),
            extra: secrets.extra.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_bound_to_the_url_it_was_entered_for() {
        let mut secrets = Secrets::default();
        secrets.set_key(
            "openai",
            "https://api.openai.com/v1",
            Secret::new("sk-abcd12345678"),
        );
        assert_eq!(
            secrets
                .key_for("openai", "https://api.openai.com/v1")
                .unwrap()
                .expose(),
            "sk-abcd12345678"
        );
        assert_eq!(secrets.last4("openai").as_deref(), Some("5678"));
        assert!(
            secrets
                .key_for("openai", "https://attacker.example/v1")
                .is_none()
        );
        assert!(
            secrets
                .key_for("missing", "https://api.openai.com/v1")
                .is_none()
        );

        secrets.clear("openai");
        assert!(secrets.last4("openai").is_none());
        assert!(
            secrets
                .key_for("openai", "https://api.openai.com/v1")
                .is_none()
        );
    }

    #[test]
    fn debug_output_contains_no_secrets() {
        let mut secrets = Secrets::default();
        secrets.set_key("one", "https://one.example/v1", Secret::new("key-material"));
        secrets.set_mcp("home", Secret::new("https://mcp.example/private-token"));
        let debug = format!("{secrets:?}");
        for secret in [
            "key-material",
            "https://mcp.example/private-token",
            "private-token",
        ] {
            assert!(!debug.contains(secret), "{debug}");
        }

        let file_debug = format!("{:?}", secrets.to_file());
        assert!(!file_debug.contains("key-material"));
        assert!(!file_debug.contains("private-token"));
    }

    #[test]
    fn toml_round_trip_preserves_credentials_and_unknown_keys() {
        let text = r#"
future_top_level = 9

[connections.openai]
url = "https://api.openai.com/v1"
key = "sk-secret"
future_connection_key = 7

[mcp.home]
url = "https://mcp.example:8443/private-token"
bearer = "bearer-secret"
future_mcp_key = "kept"
"#;
        let file: SecretsFile = toml::from_str(text).unwrap();
        let secrets = Secrets::from(file.clone());
        assert_eq!(
            secrets.mcp_host("home").as_deref(),
            Some("mcp.example:8443")
        );
        let url = secrets.mcp("home").unwrap();
        assert_eq!(url.expose(), "https://mcp.example:8443/private-token");

        let encoded = toml::to_string(&secrets.to_file()).unwrap();
        let decoded: SecretsFile = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
        assert_eq!(
            decoded.extra.get("future_top_level").unwrap().as_integer(),
            Some(9)
        );
        assert_eq!(
            decoded.connections["openai"].extra["future_connection_key"].as_integer(),
            Some(7)
        );
        assert_eq!(
            decoded.mcp["home"].extra["future_mcp_key"].as_str(),
            Some("kept")
        );
    }

    #[test]
    fn an_empty_file_has_no_secrets() {
        let file: SecretsFile = toml::from_str("").unwrap();
        assert!(file.connections.is_empty());
        assert!(file.mcp.is_empty());
        let secrets = Secrets::from(file);
        assert!(secrets.last4("missing").is_none());
        assert!(secrets.mcp("missing").is_none());
    }
}
