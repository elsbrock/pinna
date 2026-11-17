//! Settings that Home Assistant or the local API change at runtime, persisted on /var.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Unknown keys are kept, not refused: after a rollback to an older image, a
/// key only the newer pinnad wrote must not reset the rest (mute included),
/// and must still be there after the next update (ADR 0007).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Speaker volume 0..=100 ("Volume" number, media player volume).
    pub volume: u8,
    /// Volume for replies and announcements; `None` follows `volume`.
    pub reply_volume: Option<u8>,
    /// Wake word sensitivity 0..=100 (the previous firmware's scale).
    pub wake_sensitivity: u8,
    /// LED ring brightness 0..=100.
    pub led_brightness: u8,
    /// Wake word detection on/off ("Wake word" switch).
    pub wake_word: bool,
    /// Software microphone mute (the Home Assistant switch).
    pub muted: bool,
    /// Point the listening ring at the talker ("Listening direction" switch).
    pub listening_direction: bool,
    /// A notification is pending ("Notification" switch). Persisted so an
    /// update, a crash or a power cut doesn't lose it before anyone saw it.
    pub notification: bool,
    /// The active wake words' ids, at most two (ADR 0013). `None` until they
    /// are first chosen: then `[wake] id` from pinnad.toml listens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_words: Option<Vec<String>>,
    /// The name Home Assistant shows (ADR 0007). `None`: pinnad.toml's, else
    /// the model and the MAC suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friendly_name: Option<String>,
    /// The hostname, also the ESPHome node name (ADR 0010). `None`:
    /// pinnad.toml's `[device] name`, else `pinna-` and the MAC suffix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// An IANA zone name (ADR 0007). `None`: UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// A chime when listening starts. `None`: `[audio] wake_sound`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_sound: Option<bool>,
    /// The dashboard address, e.g. `https://dot.example.com/` (ADR 0021).
    /// Checked and normalised (`check_url`). `None`: the `.local` name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The AirPlay receiver on or off ("AirPlay" switch, ADR 0011). Off by
    /// default: anyone on the network can play to it.
    pub airplay: bool,
    /// Bluetooth on or off ("Bluetooth" switch, ADR 0011). Off by default.
    #[serde(default)]
    pub bluetooth: bool,
    /// Where the satellite looks for releases ("Updates" select, ADR 0022).
    /// `None`: `updates::default_channel`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<crate::updates::UpdateChannel>,
    /// Keys a newer pinnad wrote, kept as they are.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 50,
            reply_volume: None,
            wake_sensitivity: sensitivity_for_threshold(0.55),
            led_brightness: 100,
            wake_word: true,
            muted: false,
            listening_direction: true,
            notification: false,
            wake_words: None,
            friendly_name: None,
            hostname: None,
            time_zone: None,
            wake_sound: None,
            url: None,
            airplay: false,
            bluetooth: false,
            update_channel: None,
            extra: toml::Table::new(),
        }
    }
}

impl Settings {
    pub fn reply_volume(&self) -> u8 {
        self.reply_volume.unwrap_or(self.volume)
    }

    pub fn wake_threshold(&self) -> f32 {
        pinna_wakeword::WakeConfig::threshold_for_sensitivity(self.wake_sensitivity)
    }
}

/// Inverse of `WakeConfig::threshold_for_sensitivity`, for a configured threshold.
pub fn sensitivity_for_threshold(threshold: f32) -> u8 {
    ((0.85 - threshold) / 0.0044).round().clamp(0.0, 100.0) as u8
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    /// Read the persisted settings; a missing or unreadable file yields `defaults`.
    pub fn load(path: &Path, defaults: Settings) -> (Self, Settings) {
        let settings = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; using defaults", path.display());
                defaults
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => defaults,
            Err(e) => {
                tracing::warn!("{}: {e}; using defaults", path.display());
                defaults
            }
        };
        (
            Self {
                path: path.to_owned(),
            },
            settings,
        )
    }

    /// Write durably: a temporary file, fsync, rename, fsync the directory.
    pub fn try_save(&self, s: &Settings) -> std::io::Result<()> {
        use std::io::Write as _;
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("tmp");
        let text = toml::to_string(s).map_err(std::io::Error::other)?;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        std::fs::File::open(dir)?.sync_all()
    }

    /// [`Store::try_save`], with errors logged, not returned.
    pub fn save(&self, s: &Settings) {
        if let Err(e) = self.try_save(s) {
            tracing::warn!("saving {}: {e}", self.path.display());
        }
    }
}

/// The most characters a friendly name may have.
pub const NAME_MAX_CHARS: usize = 64;
/// The most bytes: an mDNS TXT string holds 255, `friendly_name=` included.
const NAME_MAX_BYTES: usize = 200;

/// A friendly name as it is stored, or why it can't be one.
pub fn check_friendly_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the satellite a name.".into());
    }
    // Zero-width and other invisible characters alone make no name.
    let invisible = |c: char| {
        c.is_whitespace()
            || matches!(c, '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
    };
    if name.chars().all(invisible) {
        return Err("Give the satellite a name.".into());
    }
    if name.chars().any(char::is_control) {
        return Err("A name can't contain line breaks or other control characters.".into());
    }
    if name.chars().count() > NAME_MAX_CHARS || name.len() > NAME_MAX_BYTES {
        return Err(format!(
            "A name can have at most {NAME_MAX_CHARS} characters."
        ));
    }
    Ok(name.to_owned())
}

/// A hostname as it is stored (lowercase, one label), or why it can't be one.
pub fn check_hostname(name: &str) -> Result<String, String> {
    let name = name.trim();
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    if name.is_empty() {
        return Err("Enter a hostname.".into());
    }
    if name.ends_with(".local") {
        return Err("Leave out .local: the satellite adds it.".into());
    }
    if name == "localhost" {
        return Err("The hostname can't be localhost.".into());
    }
    if !crate::identity::valid_hostname(&name) {
        return Err(
            "A hostname is 1 to 63 letters, digits and hyphens, and doesn't start or end with a hyphen."
                .into(),
        );
    }
    Ok(name)
}

/// The dashboard address as it is stored (`scheme://host[:port]/`), `None` when
/// it is cleared, or why it can't be one.
pub fn check_url(s: &str) -> Result<Option<String>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let start = "Enter the full address, starting with http:// or https://.";
    if s.chars().any(char::is_whitespace) {
        return Err("An address has no spaces.".into());
    }
    let url = url::Url::parse(s).map_err(|e| match e {
        url::ParseError::InvalidPort => "That port doesn't exist: use 1 to 65535.".to_string(),
        _ => start.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") || !s.contains("://") {
        return Err(start.into());
    }
    let Some(host) = url.host_str() else {
        return Err("The address needs a host name, like dot.example.com.".into());
    };
    // Addresses other satellites and browsers elsewhere can't reach.
    let unreachable = match url.host() {
        Some(url::Host::Ipv4(a)) => {
            a.is_loopback() || a.is_unspecified() || a.octets()[..3] == [10, 42, 0]
        }
        Some(url::Host::Ipv6(a)) => {
            a.is_loopback() || a.is_unspecified() || a.segments()[0] & 0xffc0 == 0xfe80
        }
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    };
    if unreachable {
        return Err(format!(
            "Other satellites can't reach {host}: use an address on your network."
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave out the user name and password.".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Leave out everything after the host name and port.".into());
    }
    if !matches!(url.path(), "" | "/") {
        return Err("Leave out the path: the address ends after the host name and port.".into());
    }
    // `Url` lowercases the scheme and host and drops a default port.
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(Some(format!("{}://{host}{port}/", url.scheme())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_roundtrip() {
        assert_eq!(sensitivity_for_threshold(0.55), 68);
        let s = Settings {
            wake_sensitivity: 68,
            ..Default::default()
        };
        assert!((s.wake_threshold() - 0.5508).abs() < 1e-4);
        assert_eq!(sensitivity_for_threshold(0.9), 0);
        assert_eq!(sensitivity_for_threshold(0.1), 100);
    }

    #[test]
    fn friendly_names() {
        assert_eq!(check_friendly_name("  Büro  ").unwrap(), "Büro");
        assert!(check_friendly_name(" ").is_err());
        assert!(check_friendly_name("\u{200B}").is_err());
        assert!(check_friendly_name("Dot\u{200B}").is_ok());
        assert!(check_friendly_name("Kitchen\nDot").is_err());
        assert!(check_friendly_name(&"a".repeat(64)).is_ok());
        assert!(check_friendly_name(&"a".repeat(65)).is_err());
        // 64 characters, but more bytes than an mDNS TXT string holds.
        assert!(check_friendly_name(&"🔊".repeat(64)).is_err());
    }

    #[test]
    fn hostnames() {
        assert_eq!(check_hostname(" Kitchen-Dot. ").unwrap(), "kitchen-dot");
        assert_eq!(check_hostname("dot2").unwrap(), "dot2");
        assert_eq!(check_hostname(" ").unwrap_err(), "Enter a hostname.");
        assert!(
            check_hostname("kitchen.local")
                .unwrap_err()
                .contains(".local")
        );
        assert!(check_hostname("localhost").is_err());
        for bad in ["-x", "x-", "a.b", "küche", "a b", &"a".repeat(64)] {
            assert!(check_hostname(bad).is_err(), "{bad:?}");
        }
        assert!(check_hostname(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn dashboard_urls() {
        for unreachable in [
            "http://10.42.0.1/",
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://[fe80::1]/",
            "http://[::1]/",
        ] {
            assert!(
                check_url(unreachable).unwrap_err().contains("can't reach"),
                "{unreachable}"
            );
        }
        assert!(check_url("http://x:99999/").unwrap_err().contains("port"));
        assert!(
            check_url("https://dot.example.com/ extra")
                .unwrap_err()
                .contains("spaces")
        );
        assert_eq!(check_url("  ").unwrap(), None);
        assert_eq!(check_url("").unwrap(), None);
        assert_eq!(
            check_url(" HTTPS://Dot.Example.com ").unwrap().unwrap(),
            "https://dot.example.com/"
        );
        assert_eq!(
            check_url("http://dot.example.com:80/").unwrap().unwrap(),
            "http://dot.example.com/"
        );
        assert_eq!(
            check_url("https://dot.example.com:443").unwrap().unwrap(),
            "https://dot.example.com/"
        );
        assert_eq!(
            check_url("http://192.168.1.20:8080/").unwrap().unwrap(),
            "http://192.168.1.20:8080/"
        );
        assert_eq!(
            check_url("http://[2001:db8::1]:8080").unwrap().unwrap(),
            "http://[2001:db8::1]:8080/"
        );
        for bad in [
            "dot.example.com",
            "dot.example.com:8080",
            "ftp://dot.example.com/",
            "mailto:me@example.com",
            "http://",
            "http://user:pw@dot.example.com/",
            "http://user@dot.example.com/",
            "http://dot.example.com/?a=1",
            "http://dot.example.com/#top",
            "http://dot.example.com/ui/",
            "http://dot.example.com:99999/",
            "not a url",
        ] {
            assert!(check_url(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn reply_volume_follows_volume() {
        let mut s = Settings {
            volume: 30,
            ..Default::default()
        };
        assert_eq!(s.reply_volume(), 30);
        s.reply_volume = Some(80);
        assert_eq!(s.reply_volume(), 80);
    }

    #[test]
    fn store_roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("pinnad-settings-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let (store, s) = Store::load(&path, Settings::default());
        assert_eq!(s, Settings::default());
        // Never chosen: the key is left out, so the pinnad.toml word applies.
        store.save(&s);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("wake_words")
        );
        // Chosen as none: kept as an empty list, not as "never chosen".
        let none = Settings {
            wake_words: Some(Vec::new()),
            ..Default::default()
        };
        store.save(&none);
        assert_eq!(Store::load(&path, Settings::default()).1, none);
        let changed = Settings {
            volume: 33,
            reply_volume: Some(70),
            wake_word: false,
            notification: true,
            wake_words: Some(vec!["hey_jarvis_v0.1".into(), "okay_nabu".into()]),
            ..Default::default()
        };
        store.save(&changed);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("wake_words")
        );
        assert_eq!(Store::load(&path, Settings::default()).1, changed);
        // A key from a newer pinnad (after a rollback) keeps everything else.
        std::fs::write(
            &path,
            "muted = true\nvolume = 20\nsetting_from_the_future = [1, 2]\n",
        )
        .unwrap();
        let rolled_back = Store::load(&path, Settings::default()).1;
        assert!(rolled_back.muted);
        assert_eq!(rolled_back.volume, 20);
        // ...and is written back, so it survives until the newer pinnad returns.
        store.save(&rolled_back);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("setting_from_the_future = [1, 2]"), "{text}");
        assert_eq!(Store::load(&path, Settings::default()).1, rolled_back);
        // The dashboard's settings: unset means "the file's", and stays out.
        assert!(
            !text.contains("friendly_name")
                && !text.contains("hostname")
                && !text.contains("time_zone")
                && !text.contains("wake_sound")
                && !text.contains("url")
        );
        let named = Settings {
            friendly_name: Some("Küche".into()),
            hostname: Some("kueche".into()),
            time_zone: Some("Europe/Berlin".into()),
            wake_sound: Some(true),
            url: Some("https://dot.example.com/".into()),
            ..Default::default()
        };
        store.try_save(&named).unwrap();
        assert_eq!(Store::load(&path, Settings::default()).1, named);
        // The old host names are no setting any more, but stay in the file.
        std::fs::write(&path, "allowed_hosts = [\"x\"]\n").unwrap();
        let old = Store::load(&path, Settings::default()).1;
        assert_eq!(old.url, None);
        assert!(old.extra.contains_key("allowed_hosts"));
        store.save(&old);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("allowed_hosts = [\"x\"]"), "{text}");
        std::fs::write(&path, "garbage = [").unwrap();
        assert_eq!(
            Store::load(&path, Settings::default()).1,
            Settings::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
