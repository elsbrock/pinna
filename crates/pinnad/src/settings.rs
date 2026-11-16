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
    /// A chime when listening starts. `None`: `[audio] wake_sound`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_sound: Option<bool>,
    /// Names the dashboard answers to, added there; `[web] allowed_hosts` adds
    /// to them. Checked and normalised (`check_host_names`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_hosts: Option<Vec<String>>,
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
            wake_sound: None,
            allowed_hosts: None,
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
/// The most host names the dashboard adds.
pub const HOSTS_MAX: usize = 16;

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

/// Host names as they are stored (lowercase, no trailing dot, each once), or
/// why one can't be added.
pub fn check_host_names(names: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in names {
        let name = raw.trim().trim_end_matches('.').to_ascii_lowercase();
        let host = name.trim_start_matches('[').trim_end_matches(']');
        if name.is_empty() {
            return Err("Enter a host name.".into());
        }
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Err(format!(
                "No need to add {}: IP addresses always work.",
                raw.trim()
            ));
        }
        if name == "localhost" {
            return Err("No need to add localhost: it always works.".into());
        }
        if name.ends_with(".local") {
            return Err(format!(
                "No need to add {name}: the dashboard answers to every .local name. \
                 Your network only finds the hostname's, though."
            ));
        }
        if name.contains(':') || name.contains('/') {
            return Err(format!(
                "That isn't a host name: leave out http:// and the port ({}).",
                raw.trim()
            ));
        }
        let label_ok = |l: &str| {
            (1..=63).contains(&l.len())
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        };
        if name.len() > 253 || !name.split('.').all(label_ok) {
            return Err(format!(
                "That isn't a host name: use letters, digits, hyphens and dots ({}).",
                raw.trim()
            ));
        }
        if !out.contains(&name) {
            out.push(name);
        }
    }
    if out.len() > HOSTS_MAX {
        return Err(format!("There can be at most {HOSTS_MAX} names."));
    }
    Ok(out)
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
    fn host_names() {
        let names =
            |v: &[&str]| check_host_names(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            names(&[" Kitchen.Home.Arpa. ", "kitchen.home.arpa", "dot-2"]).unwrap(),
            ["kitchen.home.arpa", "dot-2"]
        );
        assert_eq!(names(&[]).unwrap(), Vec::<String>::new());
        for bad in [
            "",
            "192.168.1.20",
            "[fe80::1]",
            "pinna.local",
            "localhost",
            "kitchen:8080",
            "http://kitchen",
            "-kitchen",
            "kitchen_dot",
            "a..b",
        ] {
            assert!(names(&[bad]).is_err(), "{bad:?} accepted");
        }
        let many: Vec<String> = (0..17).map(|i| format!("dot{i}")).collect();
        assert!(check_host_names(&many).is_err());
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
        assert!(!text.contains("friendly_name") && !text.contains("wake_sound"));
        let named = Settings {
            friendly_name: Some("Küche".into()),
            wake_sound: Some(true),
            allowed_hosts: Some(vec!["kitchen.home.arpa".into()]),
            ..Default::default()
        };
        store.try_save(&named).unwrap();
        assert_eq!(Store::load(&path, Settings::default()).1, named);
        std::fs::write(&path, "garbage = [").unwrap();
        assert_eq!(
            Store::load(&path, Settings::default()).1,
            Settings::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
