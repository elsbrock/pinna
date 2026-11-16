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
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
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

/// The device MAC: the Wi-Fi MAC on the Echo, from the bootloader's idme
/// record so that pinnad need not wait for Wi-Fi (wlan0 carries the same
/// address), else wlan0's; otherwise a stable, locally administered address
/// derived from the machine id (VMs, laptops).
pub fn device_mac() -> [u8; 6] {
    if let Some(mac) = pinna_hal::ident::idme_mac().and_then(|s| parse_mac(&s)) {
        return mac;
    }
    if !pinna_hal::ident::board_model().is_empty() {
        tracing::warn!("no idme mac_addr in the device tree; the device MAC comes from wlan0");
    }
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

/// The ESPHome node name: `[device] name`, or `pinna-` and the MAC suffix.
pub fn node_name(configured: &str, mac: &[u8; 6]) -> String {
    if configured.is_empty() {
        default_name(mac)
    } else {
        configured.to_owned()
    }
}

/// Whether `name` can be a hostname: one DNS label, as mDNS announces it.
pub fn valid_hostname(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// The image's static hostname: only a satellite image is renamed, never the
/// computer a developer runs pinnad on.
const IMAGE_HOSTNAME: &str = "pinna";

/// Make `name` the kernel hostname (ADR 0010), which systemd-resolved
/// announces over mDNS and systemd-networkd sends with DHCP. Leaves a name
/// that is already set alone, and a machine that isn't a satellite image.
pub fn set_hostname(name: &str) -> Result<bool> {
    anyhow::ensure!(valid_hostname(name), "{name:?} can't be a hostname");
    let image = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if image.trim() != IMAGE_HOSTNAME {
        return Ok(false);
    }
    let current = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    if current.trim() == name {
        return Ok(false);
    }
    // SAFETY: a valid pointer and length for the duration of the call.
    let r = unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) };
    if r != 0 {
        return Err(std::io::Error::last_os_error()).context("sethostname");
    }
    Ok(true)
}

/// The model Home Assistant shows, from `board_model()`'s id. Unknown
/// hardware (a VM, a PC) reports as the Echo Dot, like `board_model()`.
pub fn model_name(model: &str) -> &'static str {
    match model {
        "echo-2" => "Echo (2nd Gen)",
        _ => "Echo Dot (2nd Gen)",
    }
}

/// The name until one is chosen (ADR 0007): the model and the last three MAC
/// bytes, the way Home Assistant Voice PE names itself.
pub fn default_friendly_name(model: &str, mac: &[u8; 6]) -> String {
    let short = match model {
        "echo-2" => "Echo",
        _ => "Echo Dot",
    };
    format!("{short} {:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

/// The speaker profile follows the hardware unless a developer set one.
pub fn speaker_flat(profile: Option<&str>, model: &str) -> bool {
    match profile {
        Some(p) => p.eq_ignore_ascii_case("flat"),
        None => model != "echo-2",
    }
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
    fn node_names_make_hostnames() {
        let mac = [0xf0, 0x27, 0x2d, 0x2a, 0x91, 0x4e];
        assert_eq!(node_name("", &mac), "pinna-2a914e");
        assert_eq!(node_name("kitchen-dot", &mac), "kitchen-dot");
        assert!(valid_hostname("pinna-2a914e"));
        for bad in ["", "-x", "x-", "a.b", "küche", &"a".repeat(64)] {
            assert!(!valid_hostname(bad), "{bad:?}");
        }
        assert!(set_hostname("not a hostname").is_err());
    }

    #[test]
    fn names_and_profile_follow_the_hardware() {
        let mac = [0xf0, 0x27, 0x2d, 0x2a, 0x91, 0x4e];
        assert_eq!(default_friendly_name("echo-dot-2", &mac), "Echo Dot 2a914e");
        assert_eq!(default_friendly_name("echo-2", &mac), "Echo 2a914e");
        assert_eq!(default_friendly_name("", &mac), "Echo Dot 2a914e");
        assert_eq!(model_name("echo-2"), "Echo (2nd Gen)");
        assert_eq!(model_name("echo-dot-2"), "Echo Dot (2nd Gen)");
        assert!(!speaker_flat(None, "echo-2"));
        assert!(speaker_flat(None, "echo-dot-2"));
        assert!(speaker_flat(None, ""));
        assert!(speaker_flat(Some("Flat"), "echo-2"));
        assert!(!speaker_flat(Some("Radar"), "echo-dot-2"));
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
        assert!(unit.contains("mac=02:00:00:0a:0b:0c"));
    }
}
