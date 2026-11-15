//! Device identity (MAC, ESPHome node name) and mDNS publication through
//! systemd-resolved's DNS-SD support.

use std::path::Path;

use anyhow::{Context, Result};
use pinna_esphome::{DeviceConfig, mdns_txt, parse_encryption_key};

use crate::config;

/// The Noise key: the persisted key file (provisioned by Home Assistant) wins
/// over the configured key; neither = plaintext.
pub fn load_encryption_key(cfg: &config::Esphome) -> Result<Option<[u8; 32]>> {
    if let Ok(text) = std::fs::read_to_string(&cfg.key_file) {
        let key = parse_encryption_key(text.trim())
            .map_err(|e| anyhow::anyhow!("{}: {e:?}", cfg.key_file.display()))?;
        return Ok(Some(key));
    }
    let configured = cfg.encryption_key.trim();
    if configured.is_empty() {
        return Ok(None);
    }
    let key = parse_encryption_key(configured)
        .map_err(|e| anyhow::anyhow!("esphome.encryption_key: {e:?}"))?;
    Ok(Some(key))
}

/// Persist a key pushed by Home Assistant (mode 0600) so it survives restarts.
pub fn store_encryption_key(path: &Path, key: &[u8; 32]) -> Result<()> {
    use base64::Engine as _;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    writeln!(
        f,
        "{}",
        base64::engine::general_purpose::STANDARD.encode(key)
    )?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = s
        .trim()
        .split(':')
        .map(|p| u8::from_str_radix(p, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    parts.try_into().ok()
}

/// The device MAC: wlan0 on the Echo; otherwise a stable, locally administered
/// address derived from the machine id (VMs, laptops).
pub fn device_mac() -> [u8; 6] {
    if let Some(mac) = pinna_hal::ident::wlan0_mac().and_then(|s| parse_mac(&s)) {
        return mac;
    }
    let seed = std::fs::read_to_string("/etc/machine-id").unwrap_or_else(|_| "pinna".into());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.trim().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let b = h.to_be_bytes();
    [0x02, b[0], b[1], b[2], b[3], b[4]]
}

pub fn default_name(mac: &[u8; 6]) -> String {
    format!("pinna-{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

fn quote_txt(k: &str, v: &str) -> String {
    let kv = format!("{k}={v}");
    if kv
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        format!("\"{}\"", kv.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        kv
    }
}

/// systemd.dnssd(5) unit advertising the ESPHome native API.
pub fn dnssd_unit(cfg: &DeviceConfig) -> String {
    let txt: Vec<String> = mdns_txt(cfg).iter().map(|(k, v)| quote_txt(k, v)).collect();
    format!(
        "# Written by pinnad.\n[Service]\nName={}\nType=_esphomelib._tcp\nPort={}\nTxtText={}\n",
        cfg.name,
        cfg.port,
        txt.join(" ")
    )
}

/// Publish via resolved: write /run/systemd/dnssd/ and ask resolved to reload.
pub fn publish_mdns(cfg: &DeviceConfig) -> Result<()> {
    if !Path::new("/run/systemd/resolve").exists() {
        tracing::info!("systemd-resolved not running; skipping mDNS publication");
        return Ok(());
    }
    let dir = Path::new("/run/systemd/dnssd");
    std::fs::create_dir_all(dir)?;
    let path = dir.join("pinna-esphome.dnssd");
    let unit = dnssd_unit(cfg);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(unit.as_str()) {
        std::fs::write(&path, &unit).with_context(|| format!("writing {}", path.display()))?;
        let status = std::process::Command::new("systemctl")
            .args(["reload-or-restart", "systemd-resolved.service"])
            .status();
        match status {
            Ok(s) if s.success() => tracing::info!("published _esphomelib._tcp as {}", cfg.name),
            other => tracing::warn!("could not reload systemd-resolved: {other:?}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parsing() {
        assert_eq!(
            parse_mac("aa:bb:cc:00:11:22\n"),
            Some([0xaa, 0xbb, 0xcc, 0, 0x11, 0x22])
        );
        assert_eq!(parse_mac("nope"), None);
    }

    #[test]
    fn key_file_wins_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("pinnad-key-{}", std::process::id()));
        let file = dir.join("esphome-key");
        let mut cfg = config::Esphome {
            key_file: file.clone(),
            ..Default::default()
        };
        assert_eq!(load_encryption_key(&cfg).unwrap(), None);
        cfg.encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
        let configured = load_encryption_key(&cfg).unwrap().unwrap();
        assert_eq!(configured[1], 1);
        let pushed = [7u8; 32];
        store_encryption_key(&file, &pushed).unwrap();
        assert_eq!(load_encryption_key(&cfg).unwrap(), Some(pushed));
        cfg.encryption_key = "not base64".into();
        std::fs::remove_file(&file).unwrap();
        assert!(load_encryption_key(&cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dnssd_quotes_spaces() {
        let mut cfg = DeviceConfig::new("pinna-0a0b0c", [2, 0, 0, 0xa, 0xb, 0xc]);
        cfg.friendly_name = "Echo Dot".into();
        let unit = dnssd_unit(&cfg);
        assert!(unit.contains("Name=pinna-0a0b0c"));
        assert!(unit.contains("Type=_esphomelib._tcp"));
        assert!(unit.contains("Port=6053"));
        assert!(unit.contains("\"friendly_name=Echo Dot\""));
        assert!(unit.contains("mac=02:00:00:0A:0B:0C"));
    }
}
