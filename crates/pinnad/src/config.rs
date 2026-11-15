use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub device: Device,
    pub audio: Audio,
    pub wake: Wake,
    pub esphome: Esphome,
    pub leds: Leds,
    pub api: Api,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Device {
    /// ESPHome node name. Empty: "pinna-" + last three MAC bytes.
    pub name: String,
    pub friendly_name: String,
    pub model: String,
    pub manufacturer: String,
    pub suggested_area: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// auto | echodot | pipewire | none | wav:<path>
    pub backend: String,
    /// Startup volume 0..=100.
    pub volume: u8,
    /// Speaker codec profile on the Echo Dot ("Flat" on biscuit, "Radar" on the Echo).
    pub speaker_profile: String,
    /// Play a short chime when the wake word is detected.
    pub wake_sound: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wake {
    pub enabled: bool,
    pub models: PathBuf,
    pub keyword: String,
    /// Wake word id reported to Home Assistant.
    pub id: String,
    /// Phrase reported to Home Assistant.
    pub phrase: String,
    pub threshold: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Esphome {
    pub port: u16,
    /// Publish `_esphomelib._tcp` via systemd-resolved (writes /run/systemd/dnssd).
    pub mdns: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Leds {
    /// auto | is31 | mock | none
    pub backend: String,
    /// Overall ring brightness 0..=255.
    pub brightness: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Api {
    /// Local control API (ConnectRPC over HTTP). No authentication: keep it on loopback.
    pub listen: String,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            name: String::new(),
            friendly_name: "Pinna".into(),
            model: "Echo Dot (2nd Gen)".into(),
            manufacturer: "Pinna".into(),
            suggested_area: None,
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            volume: 50,
            speaker_profile: "Flat".into(),
            wake_sound: false,
        }
    }
}

impl Default for Wake {
    fn default() -> Self {
        Self {
            enabled: true,
            models: "/usr/share/pinna/models/openwakeword-0.5.1".into(),
            keyword: "alexa_v0.1.onnx".into(),
            id: "alexa_v0.1".into(),
            phrase: "Alexa".into(),
            threshold: 0.55,
        }
    }
}

impl Default for Esphome {
    fn default() -> Self {
        Self {
            port: 6053,
            mdns: true,
        }
    }
}

impl Default for Leds {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            brightness: 255,
        }
    }
}

impl Default for Api {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:7470".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!("{} not found, using defaults", path.display());
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let c: Config = toml::from_str(
            r#"
            [device]
            friendly_name = "Kitchen"
            [wake]
            threshold = 0.6
            "#,
        )
        .unwrap();
        assert_eq!(c.device.friendly_name, "Kitchen");
        assert_eq!(c.wake.threshold, 0.6);
        assert_eq!(c.esphome.port, 6053);
        assert_eq!(c.api.listen, "127.0.0.1:7470");
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("[device]\nnmae = \"x\"\n").is_err());
    }
}
