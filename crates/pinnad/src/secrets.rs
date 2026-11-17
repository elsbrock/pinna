//! Provider keys and MCP credentials (`secrets.toml`, mode 0600; ADR 0017).
//!
//! The file is a [`pinna_assist::secrets::SecretsFile`]: plain strings for TOML.
//! Loading salvages it key by key through [`Store`], so a value written by a
//! newer image never resets the rest. Saving converts back to the file form and
//! writes it atomically, owner-only, through [`write_durable`].

// The service wires these in a later change; the codec stands on its own here.
#![cfg_attr(not(test), allow(dead_code))]

use std::io;
use std::path::Path;

use pinna_assist::secrets::{Secrets, SecretsFile};

use crate::store::{Store, write_durable};

/// Only the owner reads provider keys and MCP tokens.
pub const FILE_MODE: u32 = 0o600;

/// Load `secrets.toml`. A missing file, or one whose keys are all invalid,
/// gives empty secrets. The returned names are the keys that were dropped.
pub fn load(path: &Path) -> (Secrets, Vec<String>) {
    let (_, file, dropped) = Store::load(path, FILE_MODE, SecretsFile::default());
    (Secrets::from(file), dropped)
}

/// Write `secrets.toml` atomically, with mode 0600.
pub fn save(path: &Path, secrets: &Secrets) -> io::Result<()> {
    let text = toml::to_string(&secrets.to_file()).map_err(io::Error::other)?;
    write_durable(path, text.as_bytes(), FILE_MODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_assist::secrets::{ConnectionFile, McpFile};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_missing_file_means_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        let (secrets, dropped) = load(&path);
        assert!(dropped.is_empty());
        assert!(secrets.last4("openai").is_none());
        assert!(
            secrets
                .key_for("openai", "https://api.openai.com/v1")
                .is_none()
        );
        assert!(secrets.mcp("home").is_none());
        assert!(!path.exists());
    }

    #[test]
    fn saving_writes_an_owner_only_file_that_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        let file = SecretsFile {
            connections: [(
                "openai".to_owned(),
                ConnectionFile {
                    url: "https://api.openai.com/v1".to_owned(),
                    key: Some("sk-abcd12345678".to_owned()),
                    extra: toml::Table::new(),
                },
            )]
            .into_iter()
            .collect(),
            mcp: [(
                "home".to_owned(),
                McpFile {
                    url: "https://mcp.example:8443/private-token".to_owned(),
                    bearer: Some("bearer-secret".to_owned()),
                    extra: toml::Table::new(),
                },
            )]
            .into_iter()
            .collect(),
            extra: toml::Table::new(),
        };
        save(&path, &Secrets::from(file)).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );

        let (secrets, dropped) = load(&path);
        assert!(dropped.is_empty());
        assert_eq!(secrets.last4("openai").as_deref(), Some("5678"));
        assert_eq!(
            secrets
                .key_for("openai", "https://api.openai.com/v1")
                .unwrap()
                .expose(),
            "sk-abcd12345678"
        );
        assert_eq!(
            secrets.mcp_host("home").as_deref(),
            Some("mcp.example:8443")
        );
        let (url, bearer) = secrets.mcp("home").unwrap();
        assert_eq!(url.expose(), "https://mcp.example:8443/private-token");
        assert_eq!(bearer.unwrap().expose(), "bearer-secret");
    }

    #[test]
    fn unknown_keys_survive_a_load_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        fs::write(
            &path,
            "future_top_level = 9\n\
             [connections.openai]\n\
             url = \"https://api.openai.com/v1\"\n\
             key = \"sk-secret\"\n\
             future_connection_key = 7\n",
        )
        .unwrap();
        let (secrets, dropped) = load(&path);
        assert!(dropped.is_empty());
        save(&path, &secrets).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_top_level = 9"), "{text}");
        assert!(text.contains("future_connection_key = 7"), "{text}");
    }
}
