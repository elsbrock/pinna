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
    pub web: Web,
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
    /// Digital gain on the captured (mono) stream before the wake word and Home
    /// Assistant, in dB. Unset: +12 dB on the Echo Dot's PipeWire graph (the echo
    /// canceller's output is far below what the wake word model expects), 0 otherwise.
    pub capture_gain_db: Option<f32>,
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
    /// Noise encryption key (base64 of 32 bytes). Empty = plaintext API.
    /// A key stored in `key_file` (e.g. pushed by Home Assistant) takes precedence.
    pub encryption_key: String,
    /// Where a key provisioned by Home Assistant is persisted.
    pub key_file: PathBuf,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Web {
    /// Dashboard and control API for the LAN (ADR 0005). No authentication.
    /// Empty = off.
    pub listen: String,
    /// The built dashboard (static files).
    pub root: PathBuf,
    /// Host names the dashboard may be opened by, besides IP addresses,
    /// `localhost`, the hostname and the node name (each also with `.local`).
    pub allowed_hosts: Vec<String>,
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
            capture_gain_db: None,
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
            encryption_key: String::new(),
            key_file: "/var/lib/pinna/esphome-key".into(),
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

impl Default for Web {
    fn default() -> Self {
        Self {
            listen: String::new(),
            root: "/usr/share/pinna/web".into(),
            allowed_hosts: Vec::new(),
        }
    }
}

/// Merge `over` into `base`: tables merge key by key, everything else is replaced.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn read_table(path: &Path) -> Result<Option<toml::Table>> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map(Some)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

impl Config {
    /// The image's configuration with the device-local file (on /var) layered on top.
    pub fn load_layered(image: &Path, local: &Path) -> Result<Self> {
        let mut table = read_table(image)?.unwrap_or_else(|| {
            tracing::warn!("{} not found, using defaults", image.display());
            toml::Table::new()
        });
        if let Some(over) = read_table(local)? {
            tracing::info!("local configuration: {}", local.display());
            merge(&mut table, over);
        }
        toml::Value::Table(table)
            .try_into()
            .with_context(|| format!("{} + {}", image.display(), local.display()))
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
        assert_eq!(c.web.listen, "");
        assert_eq!(c.web.root, Path::new("/usr/share/pinna/web"));
    }

    #[test]
    fn parses_web() {
        let c: Config = toml::from_str(
            r#"
            [web]
            listen = "0.0.0.0:80"
            allowed_hosts = ["kitchen.home.arpa"]
            "#,
        )
        .unwrap();
        assert_eq!(c.web.listen, "0.0.0.0:80");
        assert_eq!(c.web.allowed_hosts, ["kitchen.home.arpa"]);
        assert_eq!(c.web.root, Path::new("/usr/share/pinna/web"));
    }

    #[test]
    fn local_file_overrides_image() {
        let dir = std::env::temp_dir().join(format!("pinnad-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (image, local) = (dir.join("image.toml"), dir.join("local.toml"));
        std::fs::write(
            &image,
            "[device]\nfriendly_name = \"Pinna\"\nmodel = \"Dot\"\n",
        )
        .unwrap();
        std::fs::write(
            &local,
            "[device]\nname = \"echodot2\"\nfriendly_name = \"Büro\"\n",
        )
        .unwrap();
        let c = Config::load_layered(&image, &local).unwrap();
        assert_eq!(
            (
                c.device.name.as_str(),
                c.device.friendly_name.as_str(),
                c.device.model.as_str()
            ),
            ("echodot2", "Büro", "Dot")
        );
        let c = Config::load_layered(&image, &dir.join("missing.toml")).unwrap();
        assert_eq!(c.device.name, "");
        std::fs::write(&local, "[device]\nnmae = 1\n").unwrap();
        assert!(Config::load_layered(&image, &local).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("[device]\nnmae = \"x\"\n").is_err());
    }
}
