//! The player's name and its `_sendspin._tcp` record (ADR 0027 §2, ADR 0024): a `.dnssd` file
//! that systemd-resolved publishes while Sendspin is on. It is written and removed as AirPlay's
//! is: a new file only needs a reload, a changed or removed one a restart, and a removal
//! restarts resolved at once, so the record is gone before the connections end.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::PATH;

/// The record, for systemd-resolved.
const DNSSD: &str = "/run/systemd/dnssd/pinna-sendspin.dnssd";
/// The protocol's limit for the player's name, in bytes.
const NAME_MAX: usize = 64;
/// An mDNS instance name is one DNS label.
const LABEL_MAX: usize = 63;

/// The name servers list the player under, from the satellite's friendly name: without `%`,
/// which systemd would expand in the record (AirPlay drops it too), or control characters, cut
/// to the protocol's 64 bytes on a character boundary.
pub fn player_name(friendly: &str) -> String {
    let name: String = friendly
        .trim()
        .chars()
        .filter(|&c| c != '%' && !c.is_control())
        .collect();
    let name = cut(&name, NAME_MAX);
    if name.is_empty() {
        "Pinna".into()
    } else {
        name.to_owned()
    }
}

/// `text` cut to at most `max` bytes, on a character boundary.
pub fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The record for a player named `name` (see [`player_name`]) on `port`.
pub fn record(name: &str, port: u16) -> String {
    // A backslash could end a line as a continuation in a unit file; the name in the TXT
    // record, which servers show, keeps it quoted.
    let instance: String = cut(name, LABEL_MAX)
        .chars()
        .filter(|&c| c != '\\')
        .collect();
    format!(
        "# Written by pinnad: the Sendspin player (ADR 0027).\n\
         [Service]\nName={instance}\nType=_sendspin._tcp\nPort={port}\nTxtText=path={PATH} {}\n",
        crate::identity::quote_txt("name", name)
    )
}

/// Where the record goes, and whether systemd-resolved is told about it.
#[derive(Debug, Clone)]
pub struct Mdns {
    pub file: PathBuf,
    /// False in tests: they write the file and never restart the computer's resolved.
    pub resolved: bool,
}

impl Default for Mdns {
    fn default() -> Self {
        Self {
            file: DNSSD.into(),
            resolved: true,
        }
    }
}

impl Mdns {
    /// Without systemd-resolved (a developer's computer without it) there is nothing to tell.
    fn skipped(&self) -> bool {
        self.resolved && !Path::new("/run/systemd/resolve").exists()
    }

    /// Publishes the record, if it changed. Blocking.
    pub fn publish(&self, name: &str, port: u16) -> Result<()> {
        if self.skipped() {
            return Ok(());
        }
        let text = record(name, port);
        let old = std::fs::read_to_string(&self.file).ok();
        if old.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&self.file, text)
            .with_context(|| format!("writing {}", self.file.display()))?;
        if self.resolved {
            crate::identity::reread_dnssd(old.is_some());
        }
        Ok(())
    }

    /// Withdraws the record, and returns once resolved has let it go. Restarted here and now,
    /// not batched: the record has to go before the connections end (ADR 0027 §2), and a reload
    /// would leave it published (ADR 0024). Blocking.
    pub fn withdraw(&self) {
        if std::fs::remove_file(&self.file).is_ok() && self.resolved && !self.skipped() {
            let status = std::process::Command::new("systemctl")
                .args(["restart", "systemd-resolved.service"])
                .status();
            match status {
                Ok(s) if s.success() => {}
                other => tracing::warn!("sendspin: restarting systemd-resolved: {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_percent_signs_and_control_characters_and_are_cut() {
        assert_eq!(player_name("100% Kitchen\n"), "100 Kitchen");
        assert_eq!(player_name("a\u{7}b"), "ab");
        assert_eq!(player_name(&"x".repeat(70)).len(), 64);
        // Never inside a character.
        assert_eq!(player_name(&format!("{}é", "x".repeat(63))), "x".repeat(63));
        assert_eq!(player_name(" % "), "Pinna");
    }

    #[test]
    fn the_record_names_the_path_and_the_player() {
        let r = record("Kitchen Dot", 8928);
        assert_eq!(
            r,
            "# Written by pinnad: the Sendspin player (ADR 0027).\n\
             [Service]\nName=Kitchen Dot\nType=_sendspin._tcp\nPort=8928\n\
             TxtText=path=/sendspin \"name=Kitchen Dot\"\n"
        );
        let r = record("Hall", 8928);
        assert!(r.ends_with("\nTxtText=path=/sendspin name=Hall\n"), "{r}");
        // The instance is one label; the TXT name keeps all 64 bytes.
        let long = "y".repeat(64);
        let r = record(&long, 8928);
        assert!(r.contains(&format!("\nName={}\n", "y".repeat(63))), "{r}");
        assert!(r.contains(&format!("name={long}\n")), "{r}");
        let r = record("a\\b", 8928);
        assert!(r.contains("\nName=ab\n"), "{r}");
    }

    #[test]
    fn publishes_and_withdraws_without_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let mdns = Mdns {
            file: dir.path().join("dnssd/pinna-sendspin.dnssd"),
            resolved: false,
        };
        mdns.publish("Hall", 8928).unwrap();
        let text = std::fs::read_to_string(&mdns.file).unwrap();
        assert!(text.contains("Type=_sendspin._tcp\n"), "{text}");
        mdns.withdraw();
        assert!(!mdns.file.exists());
        // Nothing to withdraw: nothing happens.
        mdns.withdraw();
    }
}
