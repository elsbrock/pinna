//! Settings that Home Assistant or the local API change at runtime, persisted on /var.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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

    /// Write atomically (temp file + rename). Errors are logged, not fatal.
    pub fn save(&self, s: &Settings) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("tmp");
            let text = toml::to_string(s).map_err(std::io::Error::other)?;
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = result {
            tracing::warn!("saving {}: {e}", self.path.display());
        }
    }
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
        let changed = Settings {
            volume: 33,
            reply_volume: Some(70),
            wake_word: false,
            notification: true,
            ..Default::default()
        };
        store.save(&changed);
        assert_eq!(Store::load(&path, Settings::default()).1, changed);
        std::fs::write(&path, "garbage = [").unwrap();
        assert_eq!(
            Store::load(&path, Settings::default()).1,
            Settings::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
