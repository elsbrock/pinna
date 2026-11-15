//! Device identity (MAC, ESPHome node name) and mDNS publication through
//! systemd-resolved's DNS-SD support.

use std::path::Path;

use anyhow::{Context, Result};
use pinna_esphome::{DeviceConfig, mdns_txt};

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
