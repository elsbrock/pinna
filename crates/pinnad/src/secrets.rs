//! Provider keys and MCP credentials (`secrets.toml`, mode 0600; ADR 0017).
//!
//! The file is a [`pinna_assist::secrets::SecretsFile`]: plain strings for TOML.
//! Loading salvages it key by key through [`Store`], so a value written by a
//! newer image never resets the rest. Saving converts back to the file form and
//! writes it atomically, owner-only, through [`write_durable`].

use std::io;
use std::path::Path;

use pinna_assist::secrets::{Secrets, SecretsFile};

use crate::store::Store;

/// Only the owner reads provider keys and MCP tokens.
pub const FILE_MODE: u32 = 0o600;

/// Load `secrets.toml`. A missing file, or one whose keys are all invalid,
/// gives empty secrets. Returns the store to save through (it writes back the
/// entries this build couldn't read), the secrets, and the names dropped.
pub fn load(path: &Path) -> (Store<SecretsFile>, Secrets, Vec<String>) {
    let (store, file, dropped) = Store::load(path, FILE_MODE, SecretsFile::default());
    (store, Secrets::from(file), dropped)
}

/// Write `secrets.toml` atomically, with mode 0600.
pub fn save(store: &Store<SecretsFile>, secrets: &Secrets) -> io::Result<()> {
    store.try_save(&secrets.to_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_assist::secrets::{ConnectionFile, GroupKeyFile, McpFile};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_missing_file_means_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        let (_, secrets, dropped) = load(&path);
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
                    extra: toml::Table::new(),
                },
            )]
            .into_iter()
            .collect(),
            group: Some(GroupKeyFile {
                id: "00112233445566778899aabbccddeeff".to_owned(),
                key: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_owned(),
                extra: toml::Table::new(),
            }),
            extra: toml::Table::new(),
        };
        save(&Store::new(&path, FILE_MODE), &Secrets::from(file)).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );

        let (_, secrets, dropped) = load(&path);
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
        let url = secrets.mcp("home").unwrap();
        assert_eq!(url.expose(), "https://mcp.example:8443/private-token");
        assert_eq!(
            secrets
                .group_key("00112233445566778899aabbccddeeff")
                .unwrap()
                .expose(),
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
        );
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
        let (store, secrets, dropped) = load(&path);
        assert!(dropped.is_empty());
        save(&store, &secrets).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_top_level = 9"), "{text}");
        assert!(text.contains("future_connection_key = 7"), "{text}");
    }
}
