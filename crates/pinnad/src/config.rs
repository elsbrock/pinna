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
    pub update: Update,
    pub sendspin: Sendspin,
    pub dev: Dev,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Device {
    /// ESPHome node name. Empty: "pinna-" + last three MAC bytes.
    pub name: String,
    /// The name Home Assistant shows until one is chosen on the dashboard
    /// (ADR 0007). Unset: the model and the last three MAC bytes.
    pub friendly_name: Option<String>,
    /// The model Home Assistant shows. Unset: the hardware's.
    pub model: Option<String>,
    pub manufacturer: String,
    pub suggested_area: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// auto | echodot | echodot-pipewire | pipewire | none | wav:<path>
    pub backend: String,
    /// Startup volume 0..=100.
    pub volume: u8,
    /// Speaker codec profile ("Flat" or "Radar"). Unset: the hardware's,
    /// Radar on the Echo (2nd gen), Flat otherwise.
    pub speaker_profile: Option<String>,
    /// Play a short chime when the wake word is detected.
    pub wake_sound: bool,
    /// Fixed digital gain on the captured (mono) stream before the wake word and
    /// Home Assistant, in dB. Unset: automatic gain (ADR 0004), starting at
    /// +12 dB on the Echo Dot's PipeWire graph and 0 dB otherwise.
    pub capture_gain_db: Option<f32>,
    /// Unix socket for live PCM streams into the media lane (a header line
    /// `s16le <44100|48000> 2`, then stereo PCM; AirPlay/Bluetooth bridges,
    /// ADR 0011). Empty = off.
    pub stream_socket: PathBuf,
    /// PipeWire node to record from (`pipewire` backend). Unset: the default
    /// source. The virtual Echo Dot's microphone is a filter, which
    /// WirePlumber never makes the default.
    pub capture_target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wake {
    pub enabled: bool,
    /// The openWakeWord models: the shared mel and embedding models, and the
    /// built-in words' classifiers (ADR 0013).
    pub models: PathBuf,
    /// The speaker model that tells people apart by voice (ADR 0038). It
    /// loads only once someone is enrolled.
    pub speaker_model: PathBuf,
    /// The word that listens until the active words are first chosen (from
    /// Home Assistant or the dashboard): its classifier file in `models`...
    pub keyword: String,
    /// ...its id, as Home Assistant knows it...
    pub id: String,
    /// ...and its phrase. A built-in word keeps its own phrase.
    pub phrase: String,
    pub threshold: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Esphome {
    pub port: u16,
    /// Publish `_esphomelib._tcp` via systemd-resolved (writes /run/systemd/dnssd).
    pub mdns: bool,
    /// Noise encryption key (base64 of 32 bytes). Empty: none until Home
    /// Assistant sets one (ADR 0018). A key stored in `key_file` takes precedence.
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
    /// `localhost`, the hostname and the node name (each also with `.local`),
    /// and the host of the dashboard address setting (ADR 0021).
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Update {
    /// A manifest URL checked instead of the update channel (ADR 0022), for
    /// a developer serving `just release` output. Its version is offered when
    /// it is newer than the booted one.
    pub manifest: Option<String>,
}

/// Where the Sendspin player listens and publishes itself (ADR 0027 §2), for a developer: a
/// test runs its own pinnad beside one that already holds the port. Owners switch the player
/// on and off on the dashboard.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sendspin {
    /// The TCP port, on every address. 8928 is the one the specification recommends.
    pub port: u16,
    /// The `_sendspin._tcp` record for systemd-resolved. Anywhere else, the file is written
    /// and removed but resolved isn't told: a test on a computer leaves the computer's alone.
    pub record: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dev {
    /// The virtual Echo Dot for the development VM (ADR 0014): buttons, the
    /// microphone switch, microphone and speaker on the dashboard's
    /// `/virtual-device` socket. Only the x86_64 image sets it; pinnad refuses
    /// it on real hardware.
    pub virtual_device: bool,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            name: String::new(),
            friendly_name: None,
            model: None,
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
            speaker_profile: None,
            wake_sound: false,
            capture_gain_db: None,
            stream_socket: PathBuf::new(),
            capture_target: None,
        }
    }
}

impl Default for Wake {
    fn default() -> Self {
        Self {
            enabled: true,
            models: "/usr/share/pinna/models/openwakeword-0.5.1".into(),
            speaker_model: "/usr/share/pinna/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced-f16.onnx".into(),
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

impl Default for Sendspin {
    fn default() -> Self {
        Self {
            port: crate::sendspin::PORT,
            record: crate::sendspin::RECORD.into(),
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
    /// The image's configuration files, each layered over the one before, with
    /// the device-local file (on /var) layered on top.
    pub fn load_layered(image: &[PathBuf], local: &Path) -> Result<Self> {
        let mut table = toml::Table::new();
        for path in image {
            match read_table(path)? {
                Some(over) => merge(&mut table, over),
                None => tracing::warn!("{} not found, using defaults", path.display()),
            }
        }
        if let Some(over) = read_table(local)? {
            tracing::info!("local configuration: {}", local.display());
            merge(&mut table, over);
        }
        let files: Vec<_> = image.iter().map(|p| p.display().to_string()).collect();
        toml::Value::Table(table)
            .try_into()
            .with_context(|| format!("{} + {}", files.join(" + "), local.display()))
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
        assert_eq!(c.device.friendly_name.as_deref(), Some("Kitchen"));
        assert_eq!(c.wake.threshold, 0.6);
        assert_eq!(c.esphome.port, 6053);
        assert_eq!(c.api.listen, "127.0.0.1:7470");
        assert_eq!(c.web.listen, "");
        assert_eq!(c.web.root, Path::new("/usr/share/pinna/web"));
        assert_eq!(c.audio.stream_socket, Path::new(""));
    }

    #[test]
    fn parses_stream_socket() {
        let c: Config =
            toml::from_str("[audio]\nstream_socket = \"/run/pinna/stream.sock\"\n").unwrap();
        assert_eq!(c.audio.stream_socket, Path::new("/run/pinna/stream.sock"));
        assert_eq!(c.audio.volume, 50);
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
        let c = Config::load_layered(std::slice::from_ref(&image), &local).unwrap();
        assert_eq!(
            (
                c.device.name.as_str(),
                c.device.friendly_name.as_deref(),
                c.device.model.as_deref()
            ),
            ("echodot2", Some("Büro"), Some("Dot"))
        );
        let c =
            Config::load_layered(std::slice::from_ref(&image), &dir.join("missing.toml")).unwrap();
        assert_eq!(c.device.name, "");
        // A second image file (the x86_64 image's vm.toml) layers between them.
        let vm = dir.join("vm.toml");
        std::fs::write(
            &vm,
            "[device]\nmodel = \"VM\"\n[dev]\nvirtual_device = true\n",
        )
        .unwrap();
        let c = Config::load_layered(&[image.clone(), vm], &local).unwrap();
        assert_eq!(
            (c.device.model.as_deref(), c.device.name.as_str()),
            (Some("VM"), "echodot2")
        );
        assert!(c.dev.virtual_device);
        std::fs::write(&local, "[device]\nnmae = 1\n").unwrap();
        assert!(Config::load_layered(std::slice::from_ref(&image), &local).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The image's files parse, and leave what follows the hardware unset
    /// (ADR 0007): the base layer would otherwise always win.
    #[test]
    fn image_files_leave_hardware_and_name_unset() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mkosi");
        let image = root.join("mkosi.extra/etc/pinna/pinnad.toml");
        let c =
            Config::load_layered(std::slice::from_ref(&image), Path::new("/nonexistent")).unwrap();
        assert_eq!(c.device.model, None);
        assert_eq!(c.device.friendly_name, None);
        assert_eq!(c.audio.speaker_profile, None);
        assert_eq!(c.web.listen, "[::]:80");
        let vm = root.join("vm.extra/etc/pinna/vm.toml");
        let c = Config::load_layered(&[image, vm], Path::new("/nonexistent")).unwrap();
        assert!(c.dev.virtual_device);
        assert_eq!(c.audio.speaker_profile, None);
    }

    #[test]
    fn parses_sendspin() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.sendspin.port, 8928);
        assert_eq!(
            c.sendspin.record,
            Path::new("/run/systemd/dnssd/pinna-sendspin.dnssd")
        );
        let c: Config =
            toml::from_str("[sendspin]\nport = 18928\nrecord = \"/tmp/t/s.dnssd\"\n").unwrap();
        assert_eq!(c.sendspin.port, 18928);
        assert_eq!(c.sendspin.record, Path::new("/tmp/t/s.dnssd"));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("[device]\nnmae = \"x\"\n").is_err());
    }
}
